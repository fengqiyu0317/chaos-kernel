// AGENT
use kernel_sim::{
    Kernel, TaskRunState, N_FRAMES, SIGUSR1, SYS_EXIT, SYS_FUTEX, SYS_GETPID, SYS_KILL,
    SYS_SIGACTION, SYS_SIGRETURN,
};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::Duration;

#[repr(C)]
#[derive(Clone, Copy)]
struct UserSigAction {
    sa_handler: usize,
    sa_sigaction: usize,
    sa_mask: u64,
    sa_flags: i32,
}

#[test]
fn boot_kernel_in_standalone_runtime() {
    let kernel = Kernel::new(N_FRAMES);
    kernel.proc_init();

    assert_eq!(kernel.cur_task(0).expect("init should be current").id(), 1);

    let pid = kernel
        .dispatch_syscall(SYS_GETPID, 0, 0, 0, 0, 0, 0)
        .expect("getpid should succeed in standalone runtime");

    assert_eq!(pid, 1);
}

#[test]
// AGENT
fn default_signal_action_terminates_current_task() {
    let kernel = Kernel::new(N_FRAMES);
    kernel.proc_init();
    let child = kernel.do_fork(1).expect("fork should create child task");

    kernel
        .dispatch_syscall(SYS_KILL, 1, SIGUSR1 as usize, 0, 0, 0, 0)
        .expect("kill should enqueue and deliver the signal");

    let init = kernel
        .tasks
        .find(1)
        .expect("init task should still be reaped later");
    assert!(init.done());
    assert_eq!(*init.exit_code.lock().unwrap(), 128 + SIGUSR1 as usize);
    assert_eq!(
        kernel.cur_task(0).expect("child should be scheduled").id(),
        child
    );
}

#[test]
// AGENT
fn custom_signal_handler_updates_context_and_sigreturn_restores_it() {
    let kernel = Kernel::new(N_FRAMES);
    kernel.proc_init();
    let task = kernel.cur_task(0).expect("init should be current");
    {
        let mut thd = task.thd_ctx.lock().unwrap();
        let ctx = thd.as_mut().expect("thread context should exist");
        ctx.uctx.set_ip(0x1234);
        ctx.uctx.r[3] = 0x7777;
    }

    let act = UserSigAction {
        sa_handler: 0x5555,
        sa_sigaction: 0,
        sa_mask: 1u64 << SIGUSR1,
        sa_flags: 0,
    };
    let act_addr = &act as *const UserSigAction as usize;
    kernel
        .dispatch_syscall(SYS_SIGACTION, SIGUSR1 as usize, act_addr, 0, 0, 0, 0)
        .expect("sigaction should install handler");

    kernel
        .dispatch_syscall(SYS_KILL, 1, SIGUSR1 as usize, 0, 0, 0, 0)
        .expect("kill should enter signal handler");

    {
        let thd = task.thd_ctx.lock().unwrap();
        let ctx = thd.as_ref().expect("thread context should exist");
        assert_eq!(ctx.uctx.ip, 0x5555);
        assert_eq!(ctx.uctx.r[0], SIGUSR1 as u64);
        assert_eq!(ctx.uctx.r[1], u64::MAX);
        assert_eq!(ctx.uctx.r[2], 0x1234);
        assert_eq!(ctx.sig_frames.len(), 1);
        assert_ne!(*task.sig_mask.lock().unwrap() & (1u64 << SIGUSR1), 0);
    }

    kernel
        .dispatch_syscall(SYS_SIGRETURN, 0, 0, 0, 0, 0, 0)
        .expect("sigreturn should restore interrupted context");

    let thd = task.thd_ctx.lock().unwrap();
    let ctx = thd.as_ref().expect("thread context should exist");
    assert_eq!(ctx.uctx.ip, 0x1234);
    assert_eq!(ctx.uctx.r[3], 0x7777);
    assert_eq!(ctx.sig_frames.len(), 0);
    assert_eq!(*task.sig_mask.lock().unwrap(), 0);
}

#[test]
fn forked_task_enters_run_queue_and_receives_cpu_after_slice() {
    let kernel = Kernel::new(N_FRAMES);
    kernel.proc_init();
    let child = kernel.do_fork(1).expect("fork should create child task");

    assert_eq!(kernel.run_queue.len(), 1);
    assert_eq!(kernel.cur_task(0).expect("init should run first").id(), 1);

    for _ in 0..10 {
        kernel.schedule_tick(0);
    }

    let current = kernel
        .cur_task(0)
        .expect("scheduler should pick runnable child");
    assert_eq!(current.id(), child);
    assert_eq!(current.sched_state(), TaskRunState::Running);
}

#[test]
fn single_current_task_keeps_running_across_ticks() {
    let kernel = Kernel::new(N_FRAMES);
    kernel.proc_init();

    for _ in 0..25 {
        kernel.schedule_tick(0);
    }

    assert_eq!(
        kernel.cur_task(0).expect("init should remain current").id(),
        1
    );
    assert_eq!(kernel.run_queue.len(), 0);
}

#[test]
fn exiting_current_task_switches_to_next_runnable_task() {
    let kernel = Kernel::new(N_FRAMES);
    kernel.proc_init();
    let child = kernel.do_fork(1).expect("fork should create child task");

    kernel
        .dispatch_syscall(SYS_EXIT, 0, 0, 0, 0, 0, 0)
        .expect("exit should succeed");

    let current = kernel
        .cur_task(0)
        .expect("child should run after init exits");
    assert_eq!(current.id(), child);
    assert_eq!(current.sched_state(), TaskRunState::Running);
}

#[test]
fn futex_wait_returns_eagain_when_value_changed() {
    let kernel = Kernel::new(N_FRAMES);
    kernel.proc_init();
    let futex_word = AtomicU32::new(0);
    let uaddr = &futex_word as *const AtomicU32 as usize;

    let err = kernel
        .dispatch_syscall(SYS_FUTEX, uaddr, 0, 1, 0, 0, 0)
        .expect_err("wait should not sleep when the futex word differs");

    assert_eq!(err, "eagain");
}

#[test]
fn futex_wait_sleeps_until_wake() {
    let kernel = Arc::new(Kernel::new(N_FRAMES));
    kernel.proc_init();
    let futex_word = Arc::new(AtomicU32::new(1));
    let timeout = Arc::new([1usize, 0usize]);
    let uaddr = futex_word.as_ref() as *const AtomicU32 as usize;
    let timeout_addr = timeout.as_ptr() as usize;

    let waiter_kernel = kernel.clone();
    let waiter_word = futex_word.clone();
    let waiter = thread::spawn(move || {
        let uaddr = waiter_word.as_ref() as *const AtomicU32 as usize;
        waiter_kernel
            .dispatch_syscall(SYS_FUTEX, uaddr, 0, 1, timeout_addr, 0, 0)
            .expect("wait should be woken before the timeout")
    });

    thread::sleep(Duration::from_millis(25));
    futex_word.store(0, Ordering::SeqCst);
    let woken = kernel
        .dispatch_syscall(SYS_FUTEX, uaddr, 1, 1, 0, 0, 0)
        .expect("wake should succeed");

    assert_eq!(woken, 1);
    assert_eq!(waiter.join().expect("waiter thread should finish"), 0);
}

#[test]
fn futex_wake_zero_wakes_nobody() {
    let kernel = Kernel::new(N_FRAMES);
    kernel.proc_init();
    let futex_word = AtomicU32::new(1);
    let uaddr = &futex_word as *const AtomicU32 as usize;

    let woken = kernel
        .dispatch_syscall(SYS_FUTEX, uaddr, 1, 0, 0, 0, 0)
        .expect("wake with count zero should succeed");

    assert_eq!(woken, 0);
}

#[test]
fn futex_requeue_wakes_and_moves_waiters() {
    let kernel = Arc::new(Kernel::new(N_FRAMES));
    kernel.proc_init();
    let src = Arc::new(AtomicU32::new(1));
    let dst = Arc::new(AtomicU32::new(0));
    let timeout = Arc::new([1usize, 0usize]);
    let src_addr = src.as_ref() as *const AtomicU32 as usize;
    let dst_addr = dst.as_ref() as *const AtomicU32 as usize;
    let timeout_addr = timeout.as_ptr() as usize;

    let first_kernel = kernel.clone();
    let first_src = src.clone();
    let first = thread::spawn(move || {
        let src_addr = first_src.as_ref() as *const AtomicU32 as usize;
        first_kernel
            .dispatch_syscall(SYS_FUTEX, src_addr, 0, 1, timeout_addr, 0, 0)
            .expect("first waiter should be woken")
    });

    let second_kernel = kernel.clone();
    let second_src = src.clone();
    let second = thread::spawn(move || {
        let src_addr = second_src.as_ref() as *const AtomicU32 as usize;
        second_kernel
            .dispatch_syscall(SYS_FUTEX, src_addr, 0, 1, timeout_addr, 0, 0)
            .expect("second waiter should be requeued then woken")
    });

    thread::sleep(Duration::from_millis(25));
    let affected = kernel
        .dispatch_syscall(SYS_FUTEX, src_addr, 3, 1, 1, dst_addr, 0)
        .expect("requeue should succeed");
    // AGENT: FUTEX_REQUEUE returns the number of waiters directly woken, not
    // the number moved to the destination futex.
    assert_eq!(affected, 1);

    let woken = kernel
        .dispatch_syscall(SYS_FUTEX, dst_addr, 1, 1, 0, 0, 0)
        .expect("wake on destination should find the requeued waiter");
    assert_eq!(woken, 1);
    assert_eq!(first.join().expect("first waiter should finish"), 0);
    assert_eq!(second.join().expect("second waiter should finish"), 0);
}

#[test]
fn futex_wake_op_updates_uaddr2_and_conditionally_wakes_both_queues() {
    const FUTEX_OP_ADD: usize = 1;
    const FUTEX_OP_CMP_EQ: usize = 0;

    let kernel = Arc::new(Kernel::new(N_FRAMES));
    kernel.proc_init();
    let src = Arc::new(AtomicU32::new(1));
    let dst = Arc::new(AtomicU32::new(0));
    let timeout = Arc::new([1usize, 0usize]);
    let src_addr = src.as_ref() as *const AtomicU32 as usize;
    let dst_addr = dst.as_ref() as *const AtomicU32 as usize;
    let timeout_addr = timeout.as_ptr() as usize;

    let first_kernel = kernel.clone();
    let first_src = src.clone();
    let first = thread::spawn(move || {
        let src_addr = first_src.as_ref() as *const AtomicU32 as usize;
        first_kernel
            .dispatch_syscall(SYS_FUTEX, src_addr, 0, 1, timeout_addr, 0, 0)
            .expect("source waiter should be woken")
    });

    let second_kernel = kernel.clone();
    let second_dst = dst.clone();
    let second = thread::spawn(move || {
        let dst_addr = second_dst.as_ref() as *const AtomicU32 as usize;
        second_kernel
            .dispatch_syscall(SYS_FUTEX, dst_addr, 0, 0, timeout_addr, 0, 0)
            .expect("destination waiter should be conditionally woken")
    });

    thread::sleep(Duration::from_millis(25));
    let encoded = (FUTEX_OP_ADD << 28) | (FUTEX_OP_CMP_EQ << 24) | (1 << 12);
    let woken = kernel
        .dispatch_syscall(SYS_FUTEX, src_addr, 5, 1, 1, dst_addr, encoded)
        .expect("wake-op should succeed");

    assert_eq!(dst.load(Ordering::SeqCst), 1);
    assert_eq!(woken, 2);
    assert_eq!(first.join().expect("source waiter should finish"), 0);
    assert_eq!(second.join().expect("destination waiter should finish"), 0);
}

#[test]
fn futex_wake_op_sign_extends_oparg_and_cmparg() {
    const FUTEX_OP_ADD: usize = 1;
    const FUTEX_OP_CMP_EQ: usize = 0;

    let kernel = Kernel::new(N_FRAMES);
    kernel.proc_init();
    let src = AtomicU32::new(0);
    let dst = AtomicU32::new(u32::MAX);
    let src_addr = &src as *const AtomicU32 as usize;
    let dst_addr = &dst as *const AtomicU32 as usize;
    let encoded = (FUTEX_OP_ADD << 28) | (FUTEX_OP_CMP_EQ << 24) | (0xFFF << 12) | 0xFFF;

    let woken = kernel
        .dispatch_syscall(SYS_FUTEX, src_addr, 5, 0, 0, dst_addr, encoded)
        .expect("wake-op should accept signed 12-bit operands");

    assert_eq!(woken, 0);
    assert_eq!(dst.load(Ordering::SeqCst), u32::MAX.wrapping_sub(1));
}

#[test]
fn futex_wake_op_invalid_cmp_does_not_wake_waiters() {
    const FUTEX_OP_ADD: usize = 1;
    const FUTEX_OP_CMP_INVALID: usize = 6;

    let kernel = Arc::new(Kernel::new(N_FRAMES));
    kernel.proc_init();
    let src = Arc::new(AtomicU32::new(1));
    let dst = AtomicU32::new(0);
    let timeout = Arc::new([0usize, 100_000_000usize]);
    let src_addr = src.as_ref() as *const AtomicU32 as usize;
    let dst_addr = &dst as *const AtomicU32 as usize;
    let timeout_addr = timeout.as_ptr() as usize;

    let waiter_kernel = kernel.clone();
    let waiter_src = src.clone();
    let waiter = thread::spawn(move || {
        let src_addr = waiter_src.as_ref() as *const AtomicU32 as usize;
        waiter_kernel.dispatch_syscall(SYS_FUTEX, src_addr, 0, 1, timeout_addr, 0, 0)
    });

    thread::sleep(Duration::from_millis(25));
    let encoded = (FUTEX_OP_ADD << 28) | (FUTEX_OP_CMP_INVALID << 24) | (1 << 12);
    let err = kernel
        .dispatch_syscall(SYS_FUTEX, src_addr, 5, 1, 0, dst_addr, encoded)
        .expect_err("invalid wake-op comparison should fail");

    assert_eq!(err, "einval");
    assert_eq!(dst.load(Ordering::SeqCst), 1);
    assert_eq!(
        waiter.join().expect("waiter thread should finish"),
        Err("timeout")
    );
}

#[test]
fn futex_cmp_requeue_returns_eagain_when_source_value_changed() {
    let kernel = Kernel::new(N_FRAMES);
    kernel.proc_init();
    let src = AtomicU32::new(1);
    let dst = AtomicU32::new(0);
    let src_addr = &src as *const AtomicU32 as usize;
    let dst_addr = &dst as *const AtomicU32 as usize;

    let err = kernel
        .dispatch_syscall(SYS_FUTEX, src_addr, 9, 1, 1, dst_addr, 0)
        .expect_err("cmp-requeue should fail when the source word differs");

    assert_eq!(err, "eagain");
}

#[test]
fn futex_cmp_requeue_wakes_and_moves_after_compare() {
    let kernel = Arc::new(Kernel::new(N_FRAMES));
    kernel.proc_init();
    let src = Arc::new(AtomicU32::new(1));
    let dst = Arc::new(AtomicU32::new(0));
    let timeout = Arc::new([1usize, 0usize]);
    let src_addr = src.as_ref() as *const AtomicU32 as usize;
    let dst_addr = dst.as_ref() as *const AtomicU32 as usize;
    let timeout_addr = timeout.as_ptr() as usize;

    let first_kernel = kernel.clone();
    let first_src = src.clone();
    let first = thread::spawn(move || {
        let src_addr = first_src.as_ref() as *const AtomicU32 as usize;
        first_kernel
            .dispatch_syscall(SYS_FUTEX, src_addr, 0, 1, timeout_addr, 0, 0)
            .expect("first waiter should be woken")
    });

    let second_kernel = kernel.clone();
    let second_src = src.clone();
    let second = thread::spawn(move || {
        let src_addr = second_src.as_ref() as *const AtomicU32 as usize;
        second_kernel
            .dispatch_syscall(SYS_FUTEX, src_addr, 0, 1, timeout_addr, 0, 0)
            .expect("second waiter should be requeued then woken")
    });

    thread::sleep(Duration::from_millis(25));
    let affected = kernel
        .dispatch_syscall(SYS_FUTEX, src_addr, 9, 1, 1, dst_addr, 1)
        .expect("cmp-requeue should succeed when the source word matches");
    assert_eq!(affected, 2);

    let woken = kernel
        .dispatch_syscall(SYS_FUTEX, dst_addr, 1, 1, 0, 0, 0)
        .expect("wake on destination should find the cmp-requeued waiter");
    assert_eq!(woken, 1);
    assert_eq!(first.join().expect("first waiter should finish"), 0);
    assert_eq!(second.join().expect("second waiter should finish"), 0);
}
