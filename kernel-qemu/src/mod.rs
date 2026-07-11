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

pub mod allocator;
pub mod checkpoint_image;
pub mod fs;
pub mod kernel_core;
pub mod mm;
pub mod proc;
pub mod syscall;
pub mod util;

// AGENT: keep the former flat public API while giving rust-analyzer real modules.
pub use self::allocator::*;
pub use self::checkpoint_image::*;
pub use self::fs::*;
pub use self::kernel_core::*;
pub use self::mm::*;
pub use self::proc::*;
pub use self::syscall::*;
pub use self::util::*;
