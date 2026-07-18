use core::arch::global_asm;
use core::cell::UnsafeCell;

use crate::kernel::qemu_wait_kernel;
use crate::{println, sbi, trap};

global_asm!(include_str!("context.S"));

unsafe extern "C" {
    fn __switch(current: *mut KernelContext, next: *const KernelContext);
}

// AGENT: store only the callee-saved kernel execution state needed to suspend
// and resume one task; complete user state remains in the task's TrapFrame.
#[repr(C)]
pub struct KernelContext {
    ra: usize,
    sp: usize,
    s: [usize; 12],
}

const _: () = {
    assert!(core::mem::offset_of!(KernelContext, ra) == 0);
    assert!(core::mem::offset_of!(KernelContext, sp) == core::mem::size_of::<usize>());
    assert!(core::mem::offset_of!(KernelContext, s) == 2 * core::mem::size_of::<usize>());
    assert!(core::mem::size_of::<KernelContext>() == 14 * core::mem::size_of::<usize>());
};

// AGENT: construct an initial kernel context without duplicating user register
// state from TrapFrame.
impl KernelContext {
    pub fn for_new_task(kernel_stack_top: usize) -> Result<Self, &'static str> {
        let sp = kernel_stack_top
            .checked_sub(core::mem::size_of::<trap::TrapFrame>())
            .ok_or("ekstk")?;
        if sp % core::mem::align_of::<trap::TrapFrame>() != 0 || sp % 16 != 0 {
            return Err("ekstk");
        }
        Ok(Self {
            ra: task_bootstrap as *const () as usize,
            sp,
            s: [0; 12],
        })
    }
}

// AGENT: localize the single-hart UnsafeCell contract instead of making every
// present and future field of Task part of one broad unsafe Sync declaration.
pub struct KernelContextCell(UnsafeCell<KernelContext>);

// AGENT: allow Task to remain Sync while CPU0 is the sole context-switch owner.
// Safety: callers may obtain raw pointers only at the scheduler switch boundary;
// they must keep the containing Arc<Task> alive and may not switch concurrently.
unsafe impl Sync for KernelContextCell {}

// AGENT: create and address the stable context cell without a lock guard.
impl KernelContextCell {
    pub fn new(context: KernelContext) -> Self {
        Self(UnsafeCell::new(context))
    }

    pub fn get(&self) -> *mut KernelContext {
        self.0.get()
    }
}

// AGENT: expose the raw architecture switch boundary. Callers must provide
// stable, distinct contexts and must not retain any lock guard across the call.
// On return, execution has resumed in `current` after another task switched
// back to it; therefore all scheduler state must be published before calling.
pub unsafe fn switch_kernel_context(current: *mut KernelContext, next: *const KernelContext) {
    unsafe { __switch(current, next) }
}

// AGENT: finish the first activation of the CPU0 current task by restoring the
// complete TrapFrame already installed at the top of its own kernel stack.
#[no_mangle]
pub extern "C" fn task_bootstrap() -> ! {
    let Some(kernel) = qemu_wait_kernel() else {
        println!("[kernel-qemu] task bootstrap has no installed kernel");
        sbi::shutdown();
    };
    let Some(task) = kernel.cur_task(0) else {
        println!("[kernel-qemu] task bootstrap has no CPU0 task");
        sbi::shutdown();
    };
    unsafe { trap::enter_task_user_mode(&task) }
}

// AGENT: keep initial-context layout checks available to the focused QEMU
// scheduler selftest without exposing architecture fields to other modules.
#[cfg(any(test, feature = "qemu-sched-selftest"))]
pub mod tests {
    use super::*;

    // AGENT: run the architecture context checks from the QEMU boot harness.
    pub fn run_all() {
        new_task_context_targets_its_trap_frame_and_bootstrap();
    }

    // AGENT: pin the first-switch contract independently from Task locks and
    // allocation: sp addresses the top TrapFrame, ra enters task_bootstrap,
    // and every callee-saved register starts cleared.
    #[cfg_attr(test, test)]
    fn new_task_context_targets_its_trap_frame_and_bootstrap() {
        let kernel_stack_top = 0x8000usize;
        let context = KernelContext::for_new_task(kernel_stack_top)
            .expect("aligned kernel stack should produce an initial context");

        assert_eq!(
            context.sp,
            kernel_stack_top - core::mem::size_of::<trap::TrapFrame>()
        );
        assert_eq!(context.ra, task_bootstrap as *const () as usize);
        assert_eq!(context.s, [0; 12]);
    }
}
