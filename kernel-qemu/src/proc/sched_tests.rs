// AGENT: focused RunQueue regressions shared by Rust cfg(test) and optional
// QEMU boot selftests.
use super::*;

// AGENT: expose focused scheduler queue checks to the optional QEMU boot
// selftest path.
pub fn run_all() {
    dequeue_preserves_fifo_for_equal_priority();
    duplicate_enqueue_updates_policy_without_duplicate_entry();
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
