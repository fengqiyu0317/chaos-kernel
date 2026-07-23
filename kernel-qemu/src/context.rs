use core::arch::global_asm;
use core::cell::UnsafeCell;

use crate::kernel::global_kernel;
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
    // AGENT: initialize the boot/idle side of the first context switch; the
    // first idle -> task switch overwrites every field with the live boot stack.
    pub const fn idle() -> Self {
        Self {
            ra: 0,
            sp: 0,
            s: [0; 12],
        }
    }

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

    // AGENT: let QEMU scheduler and wait selftests enter a kernel-only task
    // function on a real Task stack without fabricating a user TrapFrame return.
    #[cfg(any(test, feature = "qemu-sched-selftest", feature = "qemu-sync-selftest"))]
    pub fn for_test_task(
        kernel_stack_top: usize,
        entry: extern "C" fn() -> !,
    ) -> Result<Self, &'static str> {
        let mut context = Self::for_new_task(kernel_stack_top)?;
        context.ra = entry as *const () as usize;
        Ok(context)
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
    let Some(kernel) = global_kernel() else {
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
    use core::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

    #[repr(align(16))]
    struct AlignedStack([u8; 4096]);

    static mut CONTEXT_TEST_STACK: AlignedStack = AlignedStack([0; 4096]);
    static TEST_IDLE_CONTEXT: AtomicUsize = AtomicUsize::new(0);
    static TEST_TASK_CONTEXT: AtomicUsize = AtomicUsize::new(0);
    static TEST_TASK_RAN: AtomicBool = AtomicBool::new(false);

    // AGENT: run the architecture context checks from the QEMU boot harness.
    pub fn run_all() {
        idle_context_starts_empty();
        new_task_context_targets_its_trap_frame_and_bootstrap();
        switch_round_trip_returns_to_idle_stack();
    }

    // AGENT: the idle context is an output slot on its first switch rather than
    // a fabricated execution frame.
    #[cfg_attr(test, test)]
    fn idle_context_starts_empty() {
        let context = KernelContext::idle();
        assert_eq!(context.ra, 0);
        assert_eq!(context.sp, 0);
        assert_eq!(context.s, [0; 12]);
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

    // AGENT: enter on the dedicated test stack, switch back through the saved
    // idle context, and prove that __switch performs a real two-context handoff.
    #[cfg_attr(test, test)]
    fn switch_round_trip_returns_to_idle_stack() {
        TEST_TASK_RAN.store(false, Ordering::Relaxed);
        let mut idle = KernelContext::idle();
        let stack_top = unsafe {
            core::ptr::addr_of_mut!(CONTEXT_TEST_STACK.0)
                .cast::<u8>()
                .add(4096) as usize
        };
        let mut task = KernelContext {
            ra: context_test_task as *const () as usize,
            sp: stack_top,
            s: [0; 12],
        };
        TEST_IDLE_CONTEXT.store(core::ptr::addr_of_mut!(idle) as usize, Ordering::Relaxed);
        TEST_TASK_CONTEXT.store(core::ptr::addr_of_mut!(task) as usize, Ordering::Relaxed);

        unsafe {
            switch_kernel_context(core::ptr::addr_of_mut!(idle), core::ptr::addr_of!(task));
        }

        assert!(TEST_TASK_RAN.load(Ordering::Relaxed));
        assert_ne!(idle.ra, 0);
        assert_ne!(idle.sp, 0);
    }

    // AGENT: test-only kernel-context entry that returns to the suspended boot
    // stack exactly as a blocked or preempted task returns to the idle loop.
    extern "C" fn context_test_task() -> ! {
        TEST_TASK_RAN.store(true, Ordering::Relaxed);
        let task = TEST_TASK_CONTEXT.load(Ordering::Relaxed) as *mut KernelContext;
        let idle = TEST_IDLE_CONTEXT.load(Ordering::Relaxed) as *const KernelContext;
        unsafe {
            switch_kernel_context(task, idle);
        }
        loop {
            core::hint::spin_loop();
        }
    }
}
