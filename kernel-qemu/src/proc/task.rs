// AGENT: preserve the historical proc::task API while routing each task
// responsibility to a focused implementation module.
use super::*;

mod core;
mod exit_reason;
mod fd;
mod process_state;
mod sched_entity;
mod signal;
mod table;

// AGENT: keep process identifiers as integers while naming the two lifecycle
// values that task creation and registration treat specially.
pub const INIT_PID: usize = 1;
const UNREGISTERED_PID: usize = 0;

pub use self::core::*;
pub use self::exit_reason::*;
pub use self::process_state::*;
pub use self::sched_entity::*;
pub use self::table::*;
