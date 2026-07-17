// AGENT: keep thread-private user context separate from the Task container.
use super::*;

// AGENT: keep thread-private user context and signal-frame state together.
#[derive(Clone)]
pub struct ThdCtx {
    pub uctx: Context,
    pub clear_tid: usize,
    pub smask: u64,
    // AGENT: stack interrupted contexts while simulated signal handlers run.
    pub sig_frames: Vec<SigFrame>,
}

// AGENT: construct the initial blank user context for a new schedulable task.
impl Default for ThdCtx {
    // AGENT: initialize thread-local context, masks, and signal frames.
    fn default() -> Self {
        Self {
            uctx: Context::new(),
            clear_tid: 0,
            smask: 0,
            sig_frames: Vec::new(),
        }
    }
}
