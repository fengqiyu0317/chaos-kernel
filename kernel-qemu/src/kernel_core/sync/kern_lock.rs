// AGENT
use crate::kernel::kernel_core::prelude::*;

// AGENT: fields are private so callers must use owner-checked enter/leave or guard APIs.
pub struct KernLock {
    flag: AtomicBool,
    holder: AtomicUsize,
    depth: AtomicUsize,
}

// AGENT: no-owner sentinel is independent from task/thread id limits.
const KERNLOCK_NO_OWNER: usize = usize::MAX;

impl KernLock {
    // AGENT: initialize holder with the owner-token sentinel, not MAX_THREAD_ID.
    pub const fn new() -> Self {
        Self {
            flag: AtomicBool::new(false),
            holder: AtomicUsize::new(KERNLOCK_NO_OWNER), // AGENT
            depth: AtomicUsize::new(0),
        }
    }
    // AGENT: KernLock owner ids are lock-owner tokens, not TaskTable indexes.
    pub fn enter(&self, id: usize) {
        assert_ne!(id, KERNLOCK_NO_OWNER, "KernLock owner id is reserved");
        if self.holder.load(Ordering::Relaxed) == id {
            self.depth.fetch_add(1, Ordering::Relaxed);
            return;
        }
        while self
            .flag
            .compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed)
            .is_err()
        {
            ::core::hint::spin_loop();
        }
        self.holder.store(id, Ordering::Relaxed);
        self.depth.store(1, Ordering::Relaxed);
    }
    // AGENT: release requires the caller id so incorrect owner/depth state is
    // caught at the lock boundary instead of silently unlocking GKL.
    pub fn leave(&self, id: usize) {
        assert_ne!(id, KERNLOCK_NO_OWNER, "KernLock owner id is reserved");
        let owner = self.holder.load(Ordering::Relaxed);
        let depth = self.depth.load(Ordering::Relaxed);
        assert!(
            self.flag.load(Ordering::Relaxed) && depth > 0,
            "KernLock::leave by owner {} without held lock",
            id
        );
        assert_eq!(
            owner, id,
            "KernLock::leave by non-owner {}, owner is {}",
            id, owner
        );
        if depth > 1 {
            self.depth.store(depth - 1, Ordering::Relaxed);
        } else {
            self.holder.store(KERNLOCK_NO_OWNER, Ordering::Relaxed); // AGENT
            self.depth.store(0, Ordering::Relaxed);
            self.flag.store(false, Ordering::Release);
        }
    }
    pub fn held(&self) -> bool {
        self.flag.load(Ordering::Relaxed)
    }
    pub fn owner(&self) -> usize {
        self.holder.load(Ordering::Relaxed)
    }
    pub fn level(&self) -> usize {
        self.depth.load(Ordering::Relaxed)
    }
    // AGENT: try_enter follows the same owner-token rule as enter().
    pub fn try_enter(&self, id: usize) -> bool {
        assert_ne!(id, KERNLOCK_NO_OWNER, "KernLock owner id is reserved");
        if self.holder.load(Ordering::Relaxed) == id {
            self.depth.fetch_add(1, Ordering::Relaxed);
            return true;
        }
        if self
            .flag
            .compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed)
            .is_ok()
        {
            self.holder.store(id, Ordering::Relaxed);
            self.depth.store(1, Ordering::Relaxed);
            true
        } else {
            false
        }
    }
    // AGENT: preferred GKL entry path; Drop pairs the owner-checked release.
    pub fn guard(&self, id: usize) -> KernLockGuard<'_> {
        self.enter(id);
        KernLockGuard { lock: self, id }
    }
    // AGENT: non-blocking guard constructor for future paths that cannot spin.
    pub fn try_guard(&self, id: usize) -> Option<KernLockGuard<'_>> {
        if self.try_enter(id) {
            Some(KernLockGuard { lock: self, id })
        } else {
            None
        }
    }
}
unsafe impl Send for KernLock {}
unsafe impl Sync for KernLock {}
pub static GKL: KernLock = KernLock::new();

// AGENT: RAII token for GKL-style locking; releasing goes through leave(id).
#[must_use = "KernLockGuard releases the lock when dropped"]
pub struct KernLockGuard<'a> {
    lock: &'a KernLock,
    id: usize,
}

// AGENT: make guard drop the only release step needed by normal callers.
impl Drop for KernLockGuard<'_> {
    fn drop(&mut self) {
        self.lock.leave(self.id);
    }
}
