// AGENT
use kernel_sim::{Kernel, TaskRunState, N_FRAMES, SYS_EXIT, SYS_GETPID};

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
