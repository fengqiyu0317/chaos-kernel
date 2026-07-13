// AGENT: preserve the historical proc::task API while routing each task
// responsibility to a focused implementation module.
use super::*;

mod core;
mod fd;
mod table;

pub use self::core::*;
pub use self::table::*;
