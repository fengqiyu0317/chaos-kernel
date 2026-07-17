// AGENT: keep thread-private lifecycle metadata separate from the Task container.
use super::*;

// AGENT: keep only non-register thread metadata here; the complete user
// register state lives in TrapFrame at the top of the task's kernel stack.
#[derive(Clone, Default)]
pub struct ThdCtx {
    pub clear_tid: usize,
    // AGENT: stack complete interrupted frames while the first-stage in-kernel
    // signal-frame bridge remains in use.
    pub sig_frames: Vec<SigFrame>,
}
