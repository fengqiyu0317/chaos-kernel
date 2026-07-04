// AGENT
use super::{EvBus, EvFlag};
use crate::kernel::kernel_core::prelude::*;

// AGENT: keep only semaphore state that is currently wired; last-operator PID
// can return with semop/semctl semantics if those syscalls are implemented.
struct SemaInner {
    cnt: isize,
    rm: bool,
    bus: EvBus,
}

pub struct Sema {
    inner: Arc<Mutex<SemaInner>>,
}

pub struct SemaGuard<'a> {
    s: &'a Sema,
}

impl Sema {
    // AGENT: initialize active semaphore state only; last-operator PID is not
    // modeled until System V semaphore syscall semantics are wired.
    pub fn new(c: isize) -> Self {
        Sema {
            inner: Arc::new(Mutex::new(SemaInner {
                cnt: c,
                rm: false,
                bus: EvBus::default(),
            })),
        }
    }
    // AGENT: mark the simplified semaphore removed and make removed state win
    // over any stale acquire-ready bit.
    pub fn remove(&self) {
        let mut i = self.inner.lock().unwrap();
        if i.rm {
            return;
        }
        i.rm = true;
        i.bus.change(EvFlag::SEM_ACQ, EvFlag::SEM_RM);
    }
    // AGENT: release is a no-op after remove(); Drop callers cannot propagate a
    // Result, and removed semaphores must not become acquire-ready again.
    pub fn release(&self) {
        let mut i = self.inner.lock().unwrap();
        if i.rm {
            return;
        }
        i.cnt += 1;
        if i.cnt >= 1 {
            i.bus.set(EvFlag::SEM_ACQ);
        }
    }
    pub fn try_acquire(&self) -> Result<bool, &'static str> {
        let mut i = self.inner.lock().unwrap();
        if i.rm {
            return Err("removed");
        }
        if i.cnt >= 1 {
            i.cnt -= 1;
            if i.cnt < 1 {
                i.bus.clear(EvFlag::SEM_ACQ);
            }
            Ok(true)
        } else {
            Ok(false)
        }
    }
    pub fn acquire_spin(&self) -> Result<(), &'static str> {
        loop {
            match self.try_acquire()? {
                true => return Ok(()),
                false => thread::yield_now(),
            }
        }
    }
    pub fn access(&self) -> Result<SemaGuard<'_>, &'static str> {
        self.acquire_spin()?;
        Ok(SemaGuard { s: self })
    }
    pub fn get_val(&self) -> isize {
        self.inner.lock().unwrap().cnt
    }
    pub fn get_ncnt(&self) -> usize {
        self.inner.lock().unwrap().bus.cb_len()
    }
    // AGENT: keep SEM_ACQ synchronized with the current simplified count value
    // and avoid reviving semaphores after remove().
    pub fn set_val(&self, v: isize) {
        let mut i = self.inner.lock().unwrap();
        if i.rm {
            return;
        }
        i.cnt = v;
        if i.cnt >= 1 {
            i.bus.set(EvFlag::SEM_ACQ);
        } else {
            i.bus.clear(EvFlag::SEM_ACQ);
        }
    }
}

impl<'a> Drop for SemaGuard<'a> {
    fn drop(&mut self) {
        self.s.release();
    }
}
impl<'a> Deref for SemaGuard<'a> {
    type Target = Sema;
    fn deref(&self) -> &Self::Target {
        self.s
    }
}
