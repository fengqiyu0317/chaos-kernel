// AGENT: preserve the historical proc::task API while routing each task
// responsibility to a focused implementation module.
use super::*;

mod core;
pub(crate) mod fd;
// AGENT: keep live scheduling, blocking, and terminal teardown transitions in
// one task-lifecycle implementation module.
mod lifecycle;
mod signal;
mod table;
// AGENT: isolate access to the user return frame stored on each task's kernel
// stack from task construction and scheduler lifecycle behavior.
mod trap_frame;

// AGENT: keep the init process identifier explicit at process construction.
pub const INIT_PID: usize = 1;

pub use self::core::*;
pub use self::table::*;
