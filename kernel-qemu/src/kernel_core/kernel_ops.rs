// AGENT
use super::*;

mod exec;
mod fs_store;
mod ipc;
mod memory;
mod pipe;
mod process;
mod runtime;
mod sched_signal;
mod tty;

// AGENT: keep page-fault access classes explicit at the Kernel boundary.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum KernelPageFaultAccess {
    Instruction,
    Load,
    Store,
}
