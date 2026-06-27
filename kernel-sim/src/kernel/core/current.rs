// AGENT
use std::cell::Cell;

pub(crate) const NO_CURRENT_TASK_ID: usize = 0;

std::thread_local! {
    static CURRENT_TASK_ID: Cell<usize> = const { Cell::new(NO_CURRENT_TASK_ID) };
}

// AGENT: scheduler-owned CPU-local current task marker. The value is a
// simulator Task::id() installed by Kernel::set_cur() or focused tests; it is
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
    CURRENT_TASK_ID.with(|slot| slot.set(id));
}

// AGENT: expose the current simulator task id for diagnostics and focused
// tests without exposing the sentinel value.
pub fn current_task_id() -> Option<usize> {
    let id = CURRENT_TASK_ID.with(|slot| slot.get());
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
