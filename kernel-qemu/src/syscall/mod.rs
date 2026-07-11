// AGENT
use super::*;

mod dispatch;
mod epoll;
mod fs;
mod mm;
mod proc;
mod signal;
mod sync;
// AGENT: Expose usercopy-backed filesystem syscall regressions to a focused
// post-frame-pool QEMU boot selftest.
#[cfg(any(test, feature = "qemu-fs-selftest"))]
#[path = "fs_tests.rs"]
pub mod tests;
mod time;

pub use self::dispatch::*;
pub(crate) use self::epoll::*;
pub(crate) use self::fs::*;
pub(crate) use self::mm::*;
pub(crate) use self::proc::*;
pub(crate) use self::signal::*;
pub(crate) use self::sync::*;
pub(crate) use self::time::*;

pub(crate) enum SyscallOutcome {
    Return(usize),
    NoReturn,
}
