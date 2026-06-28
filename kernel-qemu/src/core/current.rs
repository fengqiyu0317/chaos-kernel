// AGENT
use core::sync::atomic::{AtomicUsize, Ordering};

pub(crate) const NO_CURRENT_TASK_ID: usize = 0;

// AGENT: keep host-test-thread isolation while storing the task id in a core
// atomic cell instead of std::cell::Cell.
std::thread_local! {
    static CURRENT_TASK_ID: AtomicUsize = const { AtomicUsize::new(NO_CURRENT_TASK_ID) };
}

// AGENT: scheduler-owned current task marker. The value is a simulator
// Task::id() installed by Kernel::set_cur() or focused tests; it is
// intentionally separate from host std::thread identity and from the full
// Kernel object.
pub fn set_current_task_id(task_id: Option<usize>) {
    let id = match task_id {
        Some(id) => {
            validate_current_task_id(id);
            id
        }
        None => NO_CURRENT_TASK_ID,
    };
    CURRENT_TASK_ID.with(|slot| slot.store(id, Ordering::Relaxed));
}

// AGENT: expose the current simulator task id for diagnostics and focused
// tests without exposing the sentinel value.
pub fn current_task_id() -> Option<usize> {
    let id = CURRENT_TASK_ID.with(|slot| slot.load(Ordering::Relaxed));
    match id {
        NO_CURRENT_TASK_ID => None,
        id => Some(id),
    }
}

// AGENT: shared assertion helper for low-level code that needs a current
// simulator task but must not depend on Kernel.
pub(crate) fn require_current_task_id(caller: &str) -> usize {
    match current_task_id() {
        Some(id) => id,
        None => panic!("{caller} needs a current nonzero simulator Task::id()"),
    }
}

// AGENT: reserve zero as the no-current-task sentinel for current-task context
// and Spin owner fields.
fn validate_current_task_id(id: usize) {
    assert_ne!(
        id, NO_CURRENT_TASK_ID,
        "current task id must be a nonzero simulator Task::id()"
    );
}
