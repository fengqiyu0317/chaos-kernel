// AGENT
use super::*;

#[derive(Clone)]
pub struct SigAction {
    pub handler: usize,
    pub flags: u32,
    pub mask: u64,
}

// AGENT: signal frame now stores only the state required by sigreturn.
#[derive(Clone)]
pub struct SigFrame {
    pub saved_ctx: Context,
    pub saved_mask: u64,
}

// AGENT: signal selected from Task::sig_queue with its disposition snapshot.
#[derive(Clone)]
pub struct PendingSignal {
    pub signo: u32,
    pub sender_tid: isize,
    pub action: SigAction,
}

// AGENT: current QEMU signal state keeps dispositions only; pending signals
// live in ProcessState::sig_queue and blocked masks live in Task::sig_mask.
#[derive(Clone)]
pub struct SigSet {
    pub actions: Vec<SigAction>,
}

impl SigSet {
    // AGENT: initialize exactly the signal action slots accepted by the current
    // QEMU signal paths; valid delivered signals are below NSIG.
    pub fn new() -> Self {
        let mut actions = Vec::with_capacity(NSIG as usize);
        for _ in 0..NSIG {
            actions.push(SigAction {
                handler: SIG_DFL,
                flags: 0,
                mask: 0,
            });
        }
        Self { actions }
    }

    pub fn set_action(&mut self, signo: u32, action: SigAction) {
        if signo > 0 && signo < NSIG && signo != SIGKILL && signo != SIGSTOP {
            self.actions[signo as usize] = action;
        }
    }

    pub fn get_action(&self, signo: u32) -> &SigAction {
        if (signo as usize) < self.actions.len() {
            &self.actions[signo as usize]
        } else {
            &self.actions[0]
        }
    }

    pub fn fork_copy(&self) -> Self {
        Self {
            actions: self.actions.clone(),
        }
    }

    pub fn is_ignored(&self, signo: u32) -> bool {
        if (signo as usize) < self.actions.len() {
            self.actions[signo as usize].handler == SIG_IGN
        } else {
            false
        }
    }

    pub fn clear_non_caught(&mut self) {
        for i in 1..self.actions.len() {
            if self.actions[i].handler != SIG_DFL && self.actions[i].handler != SIG_IGN {
                self.actions[i].handler = SIG_DFL;
            }
        }
    }
}
