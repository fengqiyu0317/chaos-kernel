// AGENT
use super::*;
use crate::trap::TrapFrame;

mod dispatch;
mod epoll;
mod fs;
mod mm;
mod proc;
// AGENT: expose exec copy-in/rollback regressions through the existing process
// QEMU selftest feature without coupling them to filesystem syscall tests.
#[cfg(any(test, feature = "qemu-proc-selftest"))]
pub mod proc_tests;
mod signal;
mod stat;
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
pub(crate) use self::stat::*;
pub(crate) use self::sync::*;
pub(crate) use self::time::*;

pub(crate) enum SyscallOutcome {
    Return(usize),
    // AGENT: exec committed a new image; the trap owner must atomically replace
    // the live frame instead of mutating it through a second task-stack alias.
    ReplaceUserContext { entry: usize, stack_pointer: usize },
    // AGENT: sigreturn restores every architectural register and return CSR.
    RestoreUserContext(TrapFrame),
    NoReturn,
}
