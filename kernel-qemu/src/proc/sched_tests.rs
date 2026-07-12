// AGENT: focused RunQueue regressions shared by Rust cfg(test) and optional
// QEMU boot selftests.
use super::*;
use crate::kernel::kernel_core::{init_timer_wheel, TIMER_WHEEL};
use crate::kernel::{
    Context, FramePool, Kernel, SigAction, TaskRunState, MEM_OFF, PRIO_MIN, SIGCONT, SIGSTOP,
    SIGUSR1, SIGUSR2,
};

// AGENT: expose focused scheduler queue checks to the optional QEMU boot
// selftest path.
pub fn run_all() {
    dequeue_preserves_fifo_for_equal_priority();
    duplicate_enqueue_updates_policy_without_duplicate_entry();
    kernel_boost_updates_task_policy_and_queue_cache();
    signal_stop_uses_job_stopped_flag();
    sigcont_resumes_stopped_task_without_resuming_for_plain_signal();
    sigcont_keeps_sleeping_task_asleep_until_wait_wakeup();
    signal_handler_uses_supplied_interrupted_context();
}

// AGENT: QEMU boot selftests initialize the timer wheel in rust_main(), while
// ordinary Rust tests may construct Kernel directly.
fn ensure_timer_wheel() {
    if TIMER_WHEEL.get().is_none() {
        init_timer_wheel();
    }
}

// AGENT: build a tiny fully-free frame pool for scheduler-only selftests.
fn test_frame_pool(pages: usize) -> FramePool {
    FramePool::new(pages, MEM_OFF)
}

// AGENT: same-priority tasks should be selected in insertion order.
#[cfg_attr(test, test)]
fn dequeue_preserves_fifo_for_equal_priority() {
    let rq = RunQueue::new();
    rq.enqueue(1, SchedulePolicy::with_prio(0));
    rq.enqueue(2, SchedulePolicy::with_prio(0));
    rq.enqueue(3, SchedulePolicy::with_prio(0));

    assert_eq!(rq.dequeue().map(|(id, _)| id), Some(1));
    assert_eq!(rq.dequeue().map(|(id, _)| id), Some(2));
    assert_eq!(rq.dequeue().map(|(id, _)| id), Some(3));
    assert_eq!(rq.dequeue().map(|(id, _)| id), None);
}

// AGENT: duplicate enqueue refreshes policy without adding another queue
// entry, so a boosted task can be selected according to the new priority.
#[cfg_attr(test, test)]
fn duplicate_enqueue_updates_policy_without_duplicate_entry() {
    let rq = RunQueue::new();
    rq.enqueue(1, SchedulePolicy::with_prio(5));
    rq.enqueue(2, SchedulePolicy::with_prio(0));
    rq.enqueue(1, SchedulePolicy::with_prio(-5));

    assert_eq!(rq.len(), 2);
    assert_eq!(rq.pick_next(), Some(1));
    let (id, policy) = rq.dequeue().expect("updated task should dequeue");
    assert_eq!(id, 1);
    assert_eq!(policy.prio, -5);
    assert_eq!(rq.dequeue().map(|(id, _)| id), Some(2));
}

// AGENT: Kernel-level boosts update the task-owned policy and refresh any
// already queued runnable entry so the next pick observes the same priority.
#[cfg_attr(test, test)]
fn kernel_boost_updates_task_policy_and_queue_cache() {
    ensure_timer_wheel();

    let kernel = Kernel::new(test_frame_pool(8));
    let first = kernel.tasks.spawn("first").expect("spawn first task");
    let second = kernel.tasks.spawn("second").expect("spawn second task");

    second.boost_priority(5);
    first.set_sched_state(TaskRunState::Runnable);
    second.set_sched_state(TaskRunState::Runnable);
    kernel.run_queue.enqueue(first.id(), first.sched_policy());
    kernel.run_queue.enqueue(second.id(), second.sched_policy());
    assert_eq!(kernel.run_queue.pick_next(), Some(second.id()));

    assert!(kernel.boost_task_priority(first.id(), 10));
    assert_eq!(first.sched_policy().prio, -10);
    assert_eq!(kernel.run_queue.pick_next(), Some(first.id()));
    let (id, policy) = kernel
        .run_queue
        .dequeue()
        .expect("boosted task should dequeue first");
    assert_eq!(id, first.id());
    assert_eq!(policy.prio, -10);

    assert!(kernel.boost_task_priority(first.id(), i32::MAX));
    assert_eq!(first.sched_policy().prio, PRIO_MIN);
    kernel.run_queue.enqueue(first.id(), first.sched_policy());
    assert_eq!(kernel.run_queue.pick_next(), Some(first.id()));
}

// AGENT: SIGSTOP is process-level job-control state, not a scheduler run-state
// variant; the stopped task stays runnable but cannot be queued.
#[cfg_attr(test, test)]
fn signal_stop_uses_job_stopped_flag() {
    ensure_timer_wheel();

    let kernel = Kernel::new(test_frame_pool(8));
    kernel.proc_init();
    let task = kernel.cur_task(0).expect("init task should be current");

    kernel.send_signal_to_task(&task, SIGSTOP as i32, -1);

    assert_eq!(kernel.deliver_pending_signals(0), 1);
    assert!(task.is_job_stopped());
    assert_eq!(task.sched_state(), TaskRunState::Runnable);
    assert!(kernel.cur_task(0).is_none());
    assert_eq!(kernel.run_queue.pick_next(), None);
}

// AGENT: ordinary pending signals stay queued for a stopped task; SIGCONT is
// the explicit transition back to runnable state.
#[cfg_attr(test, test)]
fn sigcont_resumes_stopped_task_without_resuming_for_plain_signal() {
    ensure_timer_wheel();

    let kernel = Kernel::new(test_frame_pool(8));
    let task = kernel.tasks.spawn("worker").expect("spawn worker");
    task.set_sched_state(TaskRunState::Runnable);
    task.set_job_stopped(true);

    kernel.send_signal_to_task(&task, SIGUSR1 as i32, -1);
    assert!(task.is_job_stopped());
    assert_eq!(task.sched_state(), TaskRunState::Runnable);
    assert_eq!(kernel.run_queue.pick_next(), None);

    kernel.send_signal_to_task(&task, SIGCONT as i32, -1);
    assert!(!task.is_job_stopped());
    assert_eq!(task.sched_state(), TaskRunState::Runnable);
    assert_eq!(kernel.run_queue.pick_next(), Some(task.id()));
}

// AGENT: SIGCONT clears job-control stop but does not collapse a still-blocked
// wait into runnable state; the real wait wakeup owns that transition.
#[cfg_attr(test, test)]
fn sigcont_keeps_sleeping_task_asleep_until_wait_wakeup() {
    ensure_timer_wheel();

    let kernel = Kernel::new(test_frame_pool(8));
    let task = kernel.tasks.spawn("worker").expect("spawn worker");
    task.set_sched_state(TaskRunState::Sleeping);
    task.set_job_stopped(true);

    kernel.send_signal_to_task(&task, SIGCONT as i32, -1);
    assert!(!task.is_job_stopped());
    assert_eq!(task.sched_state(), TaskRunState::Sleeping);
    assert_eq!(kernel.run_queue.pick_next(), None);

    assert!(kernel.wake_task_for_wait(task.id()));
    assert_eq!(task.sched_state(), TaskRunState::Runnable);
    assert_eq!(kernel.run_queue.pick_next(), Some(task.id()));
}

// AGENT: QEMU syscall delivery supplies the live TrapFrame-derived context; the
// saved signal frame must use that context instead of a stale Task::uctx copy.
#[cfg_attr(test, test)]
fn signal_handler_uses_supplied_interrupted_context() {
    ensure_timer_wheel();

    let kernel = Kernel::new(test_frame_pool(8));
    kernel.proc_init();
    let task = kernel.cur_task(0).expect("init task should be current");
    let handler = 0x5000usize;
    task.process.sig_state.lock().unwrap().set_action(
        SIGUSR1,
        SigAction {
            handler,
            flags: 0,
            mask: 1u64 << SIGUSR2,
        },
    );

    let mut interrupted = Context::new();
    interrupted.ip = 0x1234;
    interrupted.r[0] = 0xfeed;
    interrupted.set_sp(0x8000_0000);

    kernel.send_signal_to_task(&task, SIGUSR1 as i32, 77);
    let next = kernel
        .deliver_pending_signals_from_context(0, interrupted.clone())
        .expect("handler delivery should produce a next context");

    assert_eq!(next.ip, handler as u64);
    assert_eq!(next.r[0], SIGUSR1 as u64);
    assert_eq!(next.r[1], 77);
    assert_eq!(next.r[2], interrupted.ip);
    assert_ne!(*task.sig_mask.lock().unwrap() & (1u64 << SIGUSR1), 0);
    assert_ne!(*task.sig_mask.lock().unwrap() & (1u64 << SIGUSR2), 0);

    let thd = task.thd_ctx.lock().unwrap();
    let ctx = thd.as_ref().expect("task context should exist");
    assert_eq!(ctx.sig_frames.len(), 1);
    assert_eq!(ctx.sig_frames[0].saved_ctx.r[0], 0xfeed);
    assert_eq!(ctx.sig_frames[0].saved_ctx.ip, interrupted.ip);
}
