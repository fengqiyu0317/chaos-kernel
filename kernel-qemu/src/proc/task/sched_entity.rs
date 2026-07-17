// AGENT: keep per-task scheduler state separate from general task lifecycle.
use super::*;

// AGENT: keep this enum limited to scheduler placement; job-control stop state
// lives separately on ProcessState so signal semantics do not pollute run state.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TaskRunState {
    Runnable,
    Running,
    Sleeping,
    Zombie,
}

// AGENT: group the mutable scheduler placement, policy, and remaining slice.
pub struct SchedEntity {
    pub state: TaskRunState,
    pub policy: SchedulePolicy,
    pub slice_left: usize,
}

// AGENT: initialize scheduler state from one canonical scheduling policy.
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
