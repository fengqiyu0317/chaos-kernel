// AGENT
use crate::kernel::kernel_core::prelude::*;

const SPIN_NO_OWNER: usize = 0;

// AGENT: validate the Task::id() selected from Processor.current by the caller;
// zero remains reserved for the unlocked state.
fn validate_spin_owner(owner: usize) {
    assert_ne!(owner, SPIN_NO_OWNER, "Spin needs a nonzero Task::id()");
}

// AGENT: ticket-based spinlock with private state, FIFO acquisition, RAII guard
// support, and explicit task-id owner checks. It still models only short
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
    // AGENT: FIFO acquire uses the Task::id() supplied by the scheduler-aware
    // caller instead of consulting a shadow current-task global.
    pub fn acquire(&self, owner: usize) {
        validate_spin_owner(owner);
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
    // AGENT: non-blocking acquire validates the explicit owner and only succeeds
    // when no owner or queued waiter is ahead, preserving ticket-lock fairness.
    pub fn try_acquire(&self, owner: usize) -> bool {
        validate_spin_owner(owner);
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
    // AGENT: release verifies the explicitly supplied task owns this Spin.
    pub fn release(&self, owner: usize) {
        validate_spin_owner(owner);
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
    pub fn guard(&self, owner: usize) -> SpinGuard<'_> {
        self.acquire(owner);
        let owner = self.owner.load(Ordering::Relaxed);
        SpinGuard { lock: self, owner }
    }
    // AGENT: try_guard reuses try_acquire() and captures the stored owner only
    // after the non-blocking acquisition succeeds.
    pub fn try_guard(&self, owner: usize) -> Option<SpinGuard<'_>> {
        if self.try_acquire(owner) {
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
    // AGENT: typed locking forwards the Task::id() selected by the caller to
    // the untyped Spin owner check.
    pub fn lock(&self, owner: usize) -> SpinLockGuard<'_, T> {
        let guard = self.lock.guard(owner);
        SpinLockGuard {
            _guard: guard,
            data: self.data.get(),
        }
    }
    // AGENT: typed try-locking preserves the same explicit owner contract.
    pub fn try_lock(&self, owner: usize) -> Option<SpinLockGuard<'_, T>> {
        self.lock.try_guard(owner).map(|guard| SpinLockGuard {
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
