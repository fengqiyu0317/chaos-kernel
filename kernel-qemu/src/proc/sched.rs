// AGENT
use super::*;

// AGENT: Keep only the stored scheduling priority; policy class, nice, and
// time-slice length are not separate state in the current scheduler.
#[derive(Clone)]
pub struct SchedulePolicy {
    pub prio: i32,
}

// AGENT: Derive the time-slice length from priority instead of storing a
// duplicate field in SchedulePolicy.
impl SchedulePolicy {
    pub fn new() -> Self {
        Self { prio: PRIO_DEFAULT }
    }

    pub fn with_prio(prio: i32) -> Self {
        let prio = prio.clamp(PRIO_MIN, PRIO_MAX);
        Self { prio }
    }

    pub fn time_slice(&self) -> usize {
        ((PRIO_MAX - self.prio + 1) / 2).max(1) as usize
    }
}

// AGENT: keep this enum limited to scheduler placement; job-control stop state
// lives separately on Process so signal semantics do not pollute run state.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TaskRunState {
    Runnable,
    Running,
    Sleeping,
    Zombie,
}

// AGENT: group the mutable per-task scheduler placement, policy, and remaining
// slice alongside the policy and run queue that consume this state.
pub struct SchedEntity {
    pub state: TaskRunState,
    pub policy: SchedulePolicy,
    pub slice_left: usize,
}

// AGENT: initialize per-task scheduler state from one canonical policy.
impl SchedEntity {
    // AGENT: initialize the runtime countdown from the priority-derived slice.
    pub fn new() -> Self {
        let policy = SchedulePolicy::new();
        let slice_left = policy.time_slice();
        Self {
            state: TaskRunState::Runnable,
            policy,
            slice_left,
        }
    }
}

// AGENT: keep runnable task ownership without copying each task's scheduling
// policy; Processor.current remains the authority for the executing task.
pub struct RunQueue {
    queue: Mutex<Vec<Arc<Task>>>,
}

impl RunQueue {
    // AGENT: initialize an empty runnable set without mirroring Processor.current.
    pub fn new() -> Self {
        Self {
            queue: Mutex::new(Vec::new()),
        }
    }

    // AGENT: retain each runnable task once; priority changes stay visible
    // through the task-owned SchedEntity instead of refreshing a queue cache.
    pub fn enqueue(&self, task: &Arc<Task>) {
        let mut q = self.queue.lock().unwrap();
        if q.iter().any(|queued| queued.id() == task.id()) {
            return;
        }
        q.push(task.clone());
    }

    // AGENT: Dequeue through the shared best-index helper so it preserves the
    // same priority and FIFO tie-break rules as pick_next().
    pub fn dequeue(&self) -> Option<Arc<Task>> {
        let mut q = self.queue.lock().unwrap();
        let best_idx = Self::best_idx(&q)?;
        Some(q.remove(best_idx))
    }

    // AGENT: Peek through the shared best-index helper without mutating the
    // queue, matching dequeue's priority and FIFO tie-break rules.
    pub fn pick_next(&self) -> Option<usize> {
        let q = self.queue.lock().unwrap();
        let best_idx = Self::best_idx(&q)?;
        Some(q[best_idx].id())
    }

    // AGENT: Share priority selection between peek and dequeue so same-priority
    // FIFO behavior stays identical in both paths.
    fn best_idx(q: &[Arc<Task>]) -> Option<usize> {
        if q.is_empty() {
            return None;
        }
        let mut best_idx = 0;
        for idx in 1..q.len() {
            if Self::cmp_priority(&q[idx], &q[best_idx]) == CmpOrd::Less {
                best_idx = idx;
            }
        }
        Some(best_idx)
    }

    fn cmp_priority(a: &Arc<Task>, b: &Arc<Task>) -> CmpOrd {
        a.sched_policy().prio.cmp(&b.sched_policy().prio)
    }

    pub fn rebalance(&self) {
        let mut q = self.queue.lock().unwrap();
        q.sort_by(Self::cmp_priority);
    }

    pub fn len(&self) -> usize {
        self.queue.lock().unwrap().len()
    }

    pub fn remove(&self, task_id: usize) -> bool {
        let mut q = self.queue.lock().unwrap();
        let before = q.len();
        let mut i = 0;
        while i < q.len() {
            if q[i].id() == task_id {
                q.remove(i);
            } else {
                i += 1;
            }
        }
        q.len() < before
    }
}

pub type Tid = usize;

#[cfg(any(test, feature = "qemu-sched-selftest"))]
#[path = "sched_tests.rs"]
pub mod tests;
