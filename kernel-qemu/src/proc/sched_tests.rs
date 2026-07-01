// AGENT: focused RunQueue regressions shared by Rust cfg(test) and optional
// QEMU boot selftests.
use super::*;
use crate::kernel::kernel_core::{init_timer_wheel, TIMER_WHEEL};
use crate::kernel::{FramePool, Kernel, TaskRunState, MEM_OFF, PAGE_SZ, PRIO_MIN};

// AGENT: expose focused scheduler queue checks to the optional QEMU boot
// selftest path.
pub fn run_all() {
    dequeue_preserves_fifo_for_equal_priority();
    duplicate_enqueue_updates_policy_without_duplicate_entry();
    kernel_boost_updates_task_policy_and_queue_cache();
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
    let pool = FramePool::new(pages, MEM_OFF);
    let end = MEM_OFF + pages * PAGE_SZ;
    pool.mark_free_range(MEM_OFF, end);
    pool
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
    let first = kernel.tasks.spawn("first");
    let second = kernel.tasks.spawn("second");

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
