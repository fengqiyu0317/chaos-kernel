#![allow(dead_code)]

use core::cell::UnsafeCell;
use core::hint::spin_loop;
use core::mem::MaybeUninit;
use core::ops::{Deref, DerefMut};
use core::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use core::time::Duration;

use crate::csr;

const ONCE_UNINIT: usize = 0;
const ONCE_INITING: usize = 1;
const ONCE_INIT: usize = 2;

// AGENT: no_std once cell for QEMU globals that must be explicitly initialized
// after heap setup instead of lazily constructed through std::sync::OnceLock.
pub struct IrqOnceCell<T> {
    state: AtomicUsize,
    value: UnsafeCell<MaybeUninit<T>>,
}

impl<T> IrqOnceCell<T> {
    // AGENT: build an empty once cell that can live in a static.
    pub const fn new() -> Self {
        Self {
            state: AtomicUsize::new(ONCE_UNINIT),
            value: UnsafeCell::new(MaybeUninit::uninit()),
        }
    }

    // AGENT: install the value once; callers decide whether duplicate
    // initialization is fatal for their boot stage.
    pub fn init(&self, value: T) -> Result<(), T> {
        if self
            .state
            .compare_exchange(
                ONCE_UNINIT,
                ONCE_INITING,
                Ordering::AcqRel,
                Ordering::Acquire,
            )
            .is_err()
        {
            return Err(value);
        }

        unsafe {
            (*self.value.get()).write(value);
        }
        self.state.store(ONCE_INIT, Ordering::Release);
        Ok(())
    }

    // AGENT: return the initialized value, spinning only if another context is
    // in the short critical section between state transition and value publish.
    pub fn get(&self) -> Option<&T> {
        loop {
            match self.state.load(Ordering::Acquire) {
                ONCE_UNINIT => return None,
                ONCE_INITING => spin_loop(),
                ONCE_INIT => {
                    let ptr = unsafe { (*self.value.get()).as_ptr() };
                    return Some(unsafe { &*ptr });
                }
                _ => unreachable!("invalid IrqOnceCell state"),
            }
        }
    }
}

unsafe impl<T: Sync> Sync for IrqOnceCell<T> {}

// AGENT: spin mutex for QEMU data that can be touched from both normal kernel
// code and interrupt handlers; it disables S-mode interrupts while held.
pub struct IrqSafeMutex<T> {
    locked: AtomicBool,
    data: UnsafeCell<T>,
}

// AGENT: compatibility name for migrated kernel-sim code; QEMU currently maps
// ordinary Mutex users to the irq-safe lock until lock classes are split.
pub type Mutex<T> = IrqSafeMutex<T>;

impl<T> IrqSafeMutex<T> {
    // AGENT: construct an interrupt-safe mutex around already-created data.
    pub const fn new(data: T) -> Self {
        Self {
            locked: AtomicBool::new(false),
            data: UnsafeCell::new(data),
        }
    }

    // AGENT: save SIE, disable interrupts, then acquire the spin lock so timer
    // interrupt handlers cannot recursively take the same lock on this hart.
    pub fn lock(&self) -> IrqSafeMutexGuard<'_, T> {
        let sstatus = csr::read_sstatus();
        let restore_sie = sstatus & csr::SSTATUS_SIE != 0;
        unsafe {
            csr::clear_sstatus_bits(csr::SSTATUS_SIE);
        }

        while self
            .locked
            .compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed)
            .is_err()
        {
            spin_loop();
        }

        IrqSafeMutexGuard {
            lock: self,
            restore_sie,
        }
    }
}

unsafe impl<T: Send> Send for IrqSafeMutex<T> {}
unsafe impl<T: Send> Sync for IrqSafeMutex<T> {}

// AGENT: guard that releases the spin lock before restoring the saved interrupt
// state, preserving nested critical sections that already had SIE disabled.
#[must_use = "IrqSafeMutexGuard releases the lock and restores SIE when dropped"]
pub struct IrqSafeMutexGuard<'a, T> {
    lock: &'a IrqSafeMutex<T>,
    restore_sie: bool,
}

impl<T> IrqSafeMutexGuard<'_, T> {
    // AGENT: preserve migrated std::sync::Mutex call sites that still use
    // lock().unwrap(); QEMU locks are non-poisoning, so unwrap is identity.
    pub fn unwrap(self) -> Self {
        self
    }
}

impl<T> Deref for IrqSafeMutexGuard<'_, T> {
    type Target = T;

    // AGENT: expose shared access while the interrupt-safe lock is held.
    fn deref(&self) -> &Self::Target {
        unsafe { &*self.lock.data.get() }
    }
}

// AGENT: temporary no_std RwLock compatibility layer. It is exclusive under the
// hood for now; later QEMU milestones can split read-mostly state to a real
// reader/writer lock without touching migrated call sites again.
pub struct IrqSafeRwLock<T> {
    inner: IrqSafeMutex<T>,
}

// AGENT: compatibility name for migrated kernel-sim code.
pub type RwLock<T> = IrqSafeRwLock<T>;

impl<T> IrqSafeRwLock<T> {
    // AGENT: build a QEMU rw-lock wrapper around data.
    pub const fn new(data: T) -> Self {
        Self {
            inner: IrqSafeMutex::new(data),
        }
    }

    // AGENT: read access currently takes the same irq-safe exclusive lock.
    pub fn read(&self) -> IrqSafeRwLockReadGuard<'_, T> {
        IrqSafeRwLockReadGuard {
            guard: self.inner.lock(),
        }
    }

    // AGENT: write access takes the irq-safe exclusive lock.
    pub fn write(&self) -> IrqSafeRwLockWriteGuard<'_, T> {
        IrqSafeRwLockWriteGuard {
            guard: self.inner.lock(),
        }
    }
}

unsafe impl<T: Send> Send for IrqSafeRwLock<T> {}
unsafe impl<T: Send> Sync for IrqSafeRwLock<T> {}

// AGENT: read guard for the temporary exclusive RwLock shim.
#[must_use = "IrqSafeRwLockReadGuard releases the lock when dropped"]
pub struct IrqSafeRwLockReadGuard<'a, T> {
    guard: IrqSafeMutexGuard<'a, T>,
}

impl<T> IrqSafeRwLockReadGuard<'_, T> {
    // AGENT: preserve migrated std::sync::RwLock read().unwrap() call sites.
    pub fn unwrap(self) -> Self {
        self
    }
}

impl<T> Deref for IrqSafeRwLockReadGuard<'_, T> {
    type Target = T;

    // AGENT: expose shared access while the temporary rw-lock is held.
    fn deref(&self) -> &Self::Target {
        &self.guard
    }
}

// AGENT: write guard for the temporary exclusive RwLock shim.
#[must_use = "IrqSafeRwLockWriteGuard releases the lock when dropped"]
pub struct IrqSafeRwLockWriteGuard<'a, T> {
    guard: IrqSafeMutexGuard<'a, T>,
}

impl<T> IrqSafeRwLockWriteGuard<'_, T> {
    // AGENT: preserve migrated std::sync::RwLock write().unwrap() call sites.
    pub fn unwrap(self) -> Self {
        self
    }
}

impl<T> Deref for IrqSafeRwLockWriteGuard<'_, T> {
    type Target = T;

    // AGENT: expose shared access while the temporary rw-lock is held.
    fn deref(&self) -> &Self::Target {
        &self.guard
    }
}

impl<T> DerefMut for IrqSafeRwLockWriteGuard<'_, T> {
    // AGENT: expose mutable access while the temporary rw-lock is held.
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.guard
    }
}

// AGENT: no_std placeholder for migrated host Condvar paths. QEMU wait
// semantics should use WaitToken/run-queue paths; this only keeps non-live
// runtime ticker code type-checkable until it is removed or gated.
pub struct Condvar;

impl Condvar {
    // AGENT: construct the placeholder condition variable.
    pub const fn new() -> Self {
        Self
    }

    // AGENT: return the guard immediately; there is no host thread backend in
    // QEMU, so real waits must not depend on this compatibility shim.
    pub fn wait_timeout<'a, T>(
        &self,
        guard: IrqSafeMutexGuard<'a, T>,
        _timeout: Duration,
    ) -> Result<(IrqSafeMutexGuard<'a, T>, ()), ()> {
        Ok((guard, ()))
    }

    // AGENT: placeholder notify for non-live migrated runtime ticker code.
    pub fn notify_all(&self) {}
}

// AGENT: tiny thread facade for migrated code paths that should not run on the
// QEMU carrier. It keeps type-checking localized while real waiting/scheduling
// migrates to task state and timer interrupts.
pub mod thread {
    use alloc::string::String;
    use core::hint::spin_loop;
    use core::marker::PhantomData;
    use core::time::Duration;

    pub struct JoinHandle<T> {
        _value: PhantomData<T>,
    }

    impl JoinHandle<()> {
        // AGENT: QEMU never spawns a host thread, so joining a placeholder
        // handle is a no-op.
        pub fn join(self) -> Result<(), ()> {
            Ok(())
        }
    }

    pub struct Builder {
        _name: Option<String>,
    }

    impl Builder {
        // AGENT: construct a host-thread builder facade.
        pub fn new() -> Self {
            Self { _name: None }
        }

        // AGENT: preserve the migrated builder API surface.
        pub fn name(mut self, name: String) -> Self {
            self._name = Some(name);
            self
        }

        // AGENT: fail spawn explicitly because QEMU must not create host
        // threads; callers already handle the error path.
        pub fn spawn<F>(self, _f: F) -> Result<JoinHandle<()>, ()>
        where
            F: FnOnce() + Send + 'static,
        {
            Err(())
        }
    }

    // AGENT: cooperative yield placeholder for code awaiting real scheduling.
    pub fn yield_now() {
        spin_loop();
    }

    // AGENT: QEMU cannot sleep a host thread; timer waits must use WaitToken.
    pub fn sleep(_duration: Duration) {
        spin_loop();
    }
}

impl<T> DerefMut for IrqSafeMutexGuard<'_, T> {
    // AGENT: expose mutable access while the interrupt-safe lock is held.
    fn deref_mut(&mut self) -> &mut Self::Target {
        unsafe { &mut *self.lock.data.get() }
    }
}

impl<T> Drop for IrqSafeMutexGuard<'_, T> {
    // AGENT: release the lock and restore SIE only if this guard disabled it.
    fn drop(&mut self) {
        self.lock.locked.store(false, Ordering::Release);
        if self.restore_sie {
            unsafe {
                csr::set_sstatus_bits(csr::SSTATUS_SIE);
            }
        }
    }
}
