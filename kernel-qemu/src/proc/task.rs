// AGENT: preserve the historical proc::task API while routing each task
// responsibility to a focused implementation module.
use super::*;

mod core;
mod exit_reason;
mod fd;
mod pid;
mod process_state;
mod sched_entity;
mod signal;
mod table;

pub use self::core::*;
pub use self::exit_reason::*;
pub use self::pid::*;
pub use self::process_state::*;
pub use self::sched_entity::*;
pub use self::table::*;
