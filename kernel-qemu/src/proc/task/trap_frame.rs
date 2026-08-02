// AGENT: isolate the fixed kernel-stack TrapFrame layout and all task-local
// access to the authoritative user return frame.
use super::*;
use crate::trap::TrapFrame;

// AGENT: prove the fixed top-of-stack TrapFrame fits both its owning kernel
// stack and the single page rebound through the TRAP_CONTEXT alias.
const _: () = {
    assert!(mem::size_of::<TrapFrame>() <= KSTK_SZ);
    assert!(mem::size_of::<TrapFrame>() <= PAGE_SZ);
    assert!(PAGE_SZ % mem::align_of::<TrapFrame>() == 0);
};

// AGENT: centralize user TrapFrame addressing and snapshots independently of
// task construction, kernel context switching, and lifecycle transitions.
impl Task {
    // AGENT: run one operation against a live kernel stack while keeping its
    // ownership guard held for the complete operation.
    fn with_kstk<R>(&self, f: impl FnOnce(&KStk) -> R) -> Result<R, &'static str> {
        let kstk = self.kstk.lock().unwrap();
        Ok(f(kstk.as_ref().ok_or("ekstk")?))
    }

    // AGENT: derive the fixed trap-frame slot from the statically checked stack
    // and frame layout while the caller keeps the owning kstk guard held.
    fn trap_frame_ptr_in(kstk: &KStk) -> *mut TrapFrame {
        (kstk.top() - mem::size_of::<TrapFrame>()) as *mut TrapFrame
    }

    // AGENT: locate the architecture frame trap.S owns at the fixed top slot of
    // this task's kernel stack. Callers must not create a second mutable access
    // while the live trap path already holds &mut TrapFrame for the same slot.
    pub(crate) fn user_trap_frame_ptr(&self) -> Result<*mut TrapFrame, &'static str> {
        self.with_kstk(Self::trap_frame_ptr_in)
    }

    // AGENT: return the physical page backing the authoritative TrapFrame so
    // CPU0 can rebind the fixed supervisor-only TRAP_CONTEXT alias before sret.
    pub(crate) fn user_trap_frame_page_paddr(&self) -> Result<usize, &'static str> {
        self.with_kstk(KStk::top_page_paddr)
    }

    // AGENT: initialize or replace an off-CPU task's complete user return frame.
    pub fn install_user_trap_frame(&self, frame: TrapFrame) -> Result<(), &'static str> {
        self.with_kstk(|kstk| unsafe {
            Self::trap_frame_ptr_in(kstk).write(frame);
        })
    }

    // AGENT: clone an off-CPU task's complete user return frame for fork,
    // checkpoint, tests, or scheduler-side signal delivery.
    pub fn snapshot_user_trap_frame(&self) -> Result<TrapFrame, &'static str> {
        self.with_kstk(|kstk| unsafe { (&*Self::trap_frame_ptr_in(kstk)).clone() })
    }
}
