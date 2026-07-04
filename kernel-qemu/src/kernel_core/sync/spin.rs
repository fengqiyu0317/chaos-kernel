// AGENT
use crate::kernel::kernel_core::current::{require_current_task_id, NO_CURRENT_TASK_ID};
use crate::kernel::kernel_core::prelude::*;

const SPIN_NO_OWNER: usize = NO_CURRENT_TASK_ID;

// AGENT: Spin derives its owner from the current-task context maintained by
// Kernel::set_cur(), so callers do not pass owner ids through every lock call
// and Spin does not depend on the full Kernel object.
fn spin_owner() -> usize {
    require_current_task_id("Spin")
}

// AGENT: ticket-based simulator spinlock with private state, FIFO acquisition,
// RAII guard support, and task-id owner checks. It still models only short
// non-blocking critical sections; it does not mask interrupts or preemption.
pub struct Spin {
    next_ticket: AtomicUsize,
    serving: AtomicUsize,
    owner: AtomicUsize,
}
impl Spin {
    pub const fn new() -> Self {
        Self {
            next_ticket: AtomicUsize::new(0),
            serving: AtomicUsize::new(0),
            owner: AtomicUsize::new(SPIN_NO_OWNER),
        }
    }
    // AGENT: FIFO acquire now owns current-task lookup and ticket acquisition
    // directly instead of delegating through an owner-parameter helper.
    pub fn acquire(&self) {
        let owner = spin_owner();
        assert_ne!(
            self.owner.load(Ordering::Relaxed),
            owner,
            "Spin::acquire attempted recursive locking by task {}",
            owner
        );
        let ticket = self.next_ticket.fetch_add(1, Ordering::Relaxed);
        while self.serving.load(Ordering::Acquire) != ticket {
            ::core::hint::spin_loop();
        }
        self.owner.store(owner, Ordering::Relaxed);
    }
    // AGENT: non-blocking acquire performs owner lookup inline and only
    // succeeds when no owner or queued waiter is ahead, preserving ticket-lock
    // fairness for blocking acquirers.
    pub fn try_acquire(&self) -> bool {
        let owner = spin_owner();
        assert_ne!(
            self.owner.load(Ordering::Relaxed),
            owner,
            "Spin::try_acquire attempted recursive locking by task {}",
            owner
        );
        let serving = self.serving.load(Ordering::Acquire);
        let next = self.next_ticket.load(Ordering::Relaxed);
        if serving != next {
            return false;
        }
        if self
            .next_ticket
            .compare_exchange(
                next,
                next.wrapping_add(1),
                Ordering::Acquire,
                Ordering::Relaxed,
            )
            .is_err()
        {
            return false;
        }
        self.owner.store(owner, Ordering::Relaxed);
        true
    }
    // AGENT: release verifies the current simulator task owns this Spin without
    // delegating through a private owner-parameter wrapper.
    pub fn release(&self) {
        let owner = spin_owner();
        let current_owner = self.owner.load(Ordering::Relaxed);
        assert!(
            current_owner != SPIN_NO_OWNER,
            "Spin::release by task {} without held lock",
            owner
        );
        assert_eq!(
            current_owner, owner,
            "Spin::release by non-owner task {}, owner is {}",
            owner, current_owner
        );
        self.owner.store(SPIN_NO_OWNER, Ordering::Relaxed);
        self.serving.fetch_add(1, Ordering::Release);
    }
    pub fn is_held(&self) -> bool {
        self.serving.load(Ordering::Acquire) != self.next_ticket.load(Ordering::Relaxed)
    }
    pub fn level(&self) -> usize {
        usize::from(self.owner.load(Ordering::Relaxed) != SPIN_NO_OWNER)
    }
    // AGENT: guard reuses acquire() and records the owner written by acquire()
    // so Drop can release without requiring a still-current task context.
    pub fn guard(&self) -> SpinGuard<'_> {
        self.acquire();
        let owner = self.owner.load(Ordering::Relaxed);
        SpinGuard { lock: self, owner }
    }
    // AGENT: try_guard reuses try_acquire() and captures the stored owner only
    // after the non-blocking acquisition succeeds.
    pub fn try_guard(&self) -> Option<SpinGuard<'_>> {
        if self.try_acquire() {
            let owner = self.owner.load(Ordering::Relaxed);
            Some(SpinGuard { lock: self, owner })
        } else {
            None
        }
    }
}
unsafe impl Send for Spin {}
unsafe impl Sync for Spin {}

// AGENT: RAII token for Spin; normal callers should prefer Spin::guard().
#[must_use = "SpinGuard releases the lock when dropped"]
pub struct SpinGuard<'a> {
    lock: &'a Spin,
    owner: usize,
}

// AGENT: drop-based release keeps early returns from leaking the spinlock and
// uses the guard's recorded owner instead of the current-task helper.
impl Drop for SpinGuard<'_> {
    fn drop(&mut self) {
        let current_owner = self.lock.owner.load(Ordering::Relaxed);
        assert!(
            current_owner != SPIN_NO_OWNER,
            "SpinGuard::drop by task {} without held lock",
            self.owner
        );
        assert_eq!(
            current_owner, self.owner,
            "SpinGuard::drop by non-owner task {}, owner is {}",
            self.owner, current_owner
        );
        self.lock.owner.store(SPIN_NO_OWNER, Ordering::Relaxed);
        self.lock.serving.fetch_add(1, Ordering::Release);
    }
}

// AGENT: optional typed spinlock for future short critical sections that need
// data tied to a SpinGuard instead of a separate lock plus convention.
pub struct SpinLock<T> {
    lock: Spin,
    data: ::core::cell::UnsafeCell<T>,
}

impl<T> SpinLock<T> {
    pub const fn new(data: T) -> Self {
        Self {
            lock: Spin::new(),
            data: ::core::cell::UnsafeCell::new(data),
        }
    }
    pub fn lock(&self) -> SpinLockGuard<'_, T> {
        let guard = self.lock.guard();
        SpinLockGuard {
            _guard: guard,
            data: self.data.get(),
        }
    }
    pub fn try_lock(&self) -> Option<SpinLockGuard<'_, T>> {
        self.lock.try_guard().map(|guard| SpinLockGuard {
            _guard: guard,
            data: self.data.get(),
        })
    }
    pub fn is_locked(&self) -> bool {
        self.lock.is_held()
    }
}

unsafe impl<T: Send> Send for SpinLock<T> {}
unsafe impl<T: Send> Sync for SpinLock<T> {}

// AGENT: typed guard couples protected data access to SpinGuard lifetime.
pub struct SpinLockGuard<'a, T> {
    _guard: SpinGuard<'a>,
    data: *mut T,
}

impl<T> Deref for SpinLockGuard<'_, T> {
    type Target = T;
    fn deref(&self) -> &Self::Target {
        unsafe { &*self.data }
    }
}

impl<T> DerefMut for SpinLockGuard<'_, T> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        unsafe { &mut *self.data }
    }
}
