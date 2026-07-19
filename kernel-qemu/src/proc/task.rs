// AGENT: preserve the historical proc::task API while routing each task
// responsibility to a focused implementation module.
use super::*;

mod core;
mod exit_reason;
pub(crate) mod fd;
mod sched_entity;
mod signal;
mod table;

// AGENT: keep the init process identifier explicit at process construction.
pub const INIT_PID: usize = 1;

pub use self::core::*;
pub use self::exit_reason::*;
pub use self::sched_entity::*;
pub use self::table::*;
