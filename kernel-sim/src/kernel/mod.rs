// AGENT
// Standard module tree for the standalone kernel simulation.
#![allow(
    unused,
    dead_code,
    non_upper_case_globals,
    non_camel_case_types,
    unused_assignments,
    unused_mut
)]

pub mod core;
pub mod fs;
pub mod mm;
pub mod proc;
pub mod syscall;
pub mod util;

// AGENT: keep the former flat public API while giving rust-analyzer real modules.
pub use self::core::*;
pub use self::fs::*;
pub use self::mm::*;
pub use self::proc::*;
pub use self::syscall::*;
pub use self::util::*;
