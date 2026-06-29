#![allow(dead_code)]

use core::cell::UnsafeCell;
use core::hint::spin_loop;
use core::mem::MaybeUninit;
use core::ops::{Deref, DerefMut};
use core::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

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

impl<T> Deref for IrqSafeMutexGuard<'_, T> {
    type Target = T;

    // AGENT: expose shared access while the interrupt-safe lock is held.
    fn deref(&self) -> &Self::Target {
        unsafe { &*self.lock.data.get() }
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
