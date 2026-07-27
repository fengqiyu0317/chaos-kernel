// AGENT
use super::*;

mod checkpoint;
mod exec;
mod fs_store;
mod ipc;
mod memory;
mod process;
mod runtime;
mod sched_signal;
mod tty;

// AGENT: represent only the three meaningful regular-file creation policies
// instead of allowing independent O_CREAT/O_EXCL booleans to form an undefined
// O_EXCL-without-O_CREAT state.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum CreateDisposition {
    OpenExisting,
    CreateIfMissing,
    CreateNew,
}

// AGENT: keep page-fault access classes explicit at the Kernel boundary.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum KernelPageFaultAccess {
    Instruction,
    Load,
    Store,
}
