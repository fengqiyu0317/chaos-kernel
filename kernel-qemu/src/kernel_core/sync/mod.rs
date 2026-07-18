// AGENT
// AGENT: sync primitives are split by responsibility; this module preserves the
// former flat kernel_core::sync API through explicit re-exports.
// AGENT: Usage map for this module in the current kernel-sim code.
//
// Active paths:
// - GKL/KernLock backs Kernel::tick() through KernLockGuard so release stays
//   caller-checked and panic-safe.
// - Spin backs short critical sections through SpinGuard so release is panic-safe
//   and callers cannot touch the atomic state directly; ownership is keyed by
//   simulator Task::id() values instead of host std::thread identity.
// - EvBus/EvFlag is used as event-bit storage by pipe, process exit/signal,
//   semaphore state transitions, and pipe-backed epoll readiness notification.
// - WaitToken is the common task wait token used by Channel,
//   ConditionWait/CountingEvent helpers, epoll waiters, and FutexBucket.
// - ConditionWait is used by Channel through wait_until(), signal(), and
//   broadcast().
// - FutexBucket is wired to SYS_FUTEX and process-exit cleanup.
//
// Partially wired paths:
// - Sema is created through SemArr and uses remove()/release(), but process-local
//   handles, SEM_UNDO, and semget/semop/semctl-style syscall dispatch are TODO.
//
// Unused or reserved paths:
// - KernLock::enter/try_enter/held/owner/level are available for focused tests
//   or future paths that cannot use the guard API; Spin::try_acquire/is_held
//   and SpinLock<T> are available for short non-blocking critical sections.
// - EvFlag::WRITABLE/ERROR.
// - ConditionWait's generic condition-check helpers.
// - SocketState.
// AGENT TODO: KernLock is still a simulator recursive spin lock, not full
// real-kernel locking: it lacks fairness, blocking wait, preemption control,
// and interrupt masking semantics.

mod event;
mod futex;
mod kern_lock;
mod sema;
mod spin;
mod wait;

pub use self::event::{EvBus, EvCb, EvFlag};
pub use self::futex::FutexBucket;
pub use self::kern_lock::{KernLock, KernLockGuard, GKL};
pub use self::sema::{Sema, SemaGuard};
pub use self::spin::{Spin, SpinGuard, SpinLock, SpinLockGuard};
pub use self::wait::{
    install_qemu_wait_kernel, ConditionWait, CountingEvent, WaitOutcome, WaitToken,
};
pub(crate) use self::wait::{qemu_wait_kernel, qemu_wait_timer_tick, WaitQueue};

// AGENT: expose WaitToken-focused regressions to both Rust tests and the optional
// QEMU boot self-test feature, matching the mm/tests.rs pattern.
#[cfg(any(test, feature = "qemu-sync-selftest"))]
pub mod tests;
