// AGENT: keep process-exit representation separate from lifecycle and teardown behavior.
use super::*;

// AGENT: retain the Linux-compatible distinction between normal and signaled process exit.
// TODO(AGENT): replace raw Signal(u8) with a validated 1..=NSIG value and
// carry the Linux core-dump flag once kernel-qemu has a real core-image path.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExitReason {
    Code(u8),
    Signal(u8),
}

// AGENT: translate internal process-exit reasons into wait-compatible status words.
impl ExitReason {
    // AGENT: encode normal exit codes and terminating signals for wait syscalls.
    // TODO(AGENT): make this status u32 end-to-end; stopped and continued
    // statuses belong to a future wait-event model rather than ExitReason.
    pub fn wait_status(self) -> usize {
        match self {
            ExitReason::Code(code) => (code as usize) << 8,
            ExitReason::Signal(sig) => (sig as usize) & 0x7f,
        }
    }
}
