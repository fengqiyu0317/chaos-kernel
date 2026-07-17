// AGENT: keep process-exit representation separate from Task teardown behavior.
use super::*;

// AGENT: retain the Linux-compatible distinction between normal and signaled exit.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExitReason {
    Code(u8),
    Signal(u8),
}

// AGENT: translate internal exit reasons into wait-compatible status words.
impl ExitReason {
    // AGENT: encode normal exit codes and terminating signals for wait syscalls.
    pub fn wait_status(self) -> usize {
        match self {
            ExitReason::Code(code) => (code as usize) << 8,
            ExitReason::Signal(sig) => (sig as usize) & 0x7f,
        }
    }
}
