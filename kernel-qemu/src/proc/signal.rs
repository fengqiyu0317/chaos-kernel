// AGENT
use super::*;
use crate::trap::TrapFrame;

// AGENT: keep only signal disposition state with live delivery semantics;
// sigaction flag support remains an explicit syscall-boundary TODO.
#[derive(Clone)]
pub struct SigAction {
    pub handler: usize,
    pub mask: u64,
}

// AGENT: separate a stored sigaction disposition from the concrete operation
// the carrier must perform when that disposition is resolved for one signal.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SignalDeliveryAction {
    Ignore,
    Continue,
    Stop,
    Terminate,
    Handler(usize),
}

impl SigAction {
    // AGENT: keep the canonical default disposition in one place so exec reset
    // paths do not leave a stale handler mask behind.
    pub fn default_action() -> Self {
        Self {
            handler: SIG_DFL,
            mask: 0,
        }
    }

    // AGENT: resolve SIG_DFL through the Linux/RISC-V signal-specific default
    // table; core-dump defaults currently collapse into signal termination
    // because kernel-qemu has no core-image carrier yet.
    pub fn resolve(&self, signo: u32) -> SignalDeliveryAction {
        match self.handler {
            SIG_IGN => SignalDeliveryAction::Ignore,
            SIG_DFL => match signo {
                SIGCHLD | SIGURG | SIGWINCH => SignalDeliveryAction::Ignore,
                SIGCONT => SignalDeliveryAction::Continue,
                SIGSTOP | SIGTSTP | SIGTTIN | SIGTTOU => SignalDeliveryAction::Stop,
                _ => SignalDeliveryAction::Terminate,
            },
            handler => SignalDeliveryAction::Handler(handler),
        }
    }
}

// AGENT: retain every architectural register and return CSR required by sigreturn.
#[derive(Clone)]
pub struct SigFrame {
    pub saved_frame: TrapFrame,
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
            actions.push(SigAction::default_action());
        }
        Self { actions }
    }

    pub fn set_action(&mut self, signo: u32, action: SigAction) -> bool {
        if signo > 0 && signo < NSIG && signo != SIGKILL && signo != SIGSTOP {
            self.actions[signo as usize] = action;
            true
        } else {
            false
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
            self.actions[signo as usize].resolve(signo) == SignalDeliveryAction::Ignore
        } else {
            false
        }
    }

    // AGENT: exec keeps ignored dispositions but resets caught handlers to a
    // clean default action, including stale handler masks.
    pub fn clear_non_caught(&mut self) {
        for i in 1..self.actions.len() {
            if self.actions[i].handler != SIG_DFL && self.actions[i].handler != SIG_IGN {
                self.actions[i] = SigAction::default_action();
            }
        }
    }
}
