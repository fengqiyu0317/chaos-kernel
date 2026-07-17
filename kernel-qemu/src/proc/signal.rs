// AGENT
use super::*;
use crate::trap::TrapFrame;

// AGENT: translate the Linux/RISC-V signal-number ABI (1..=64) to compact
// zero-based storage without reserving an unused slot for signal zero.
pub const fn signal_index(signo: u32) -> Option<usize> {
    if signo == 0 || signo > NSIG {
        None
    } else {
        Some((signo - 1) as usize)
    }
}

// AGENT: keep sigset_t compatible with Linux: signal 1 occupies bit 0 and
// signal 64 occupies bit 63 of the one-word mask.
pub const fn signal_bit(signo: u32) -> Option<u64> {
    match signal_index(signo) {
        Some(index) => Some(1u64 << index),
        None => None,
    }
}

// AGENT: SIGKILL and SIGSTOP can never be blocked; express their mask with the
// same signo-minus-one mapping used for userspace sigset_t values.
pub const UNMASKABLE_SIGNAL_MASK: u64 = (1u64 << (SIGKILL - 1)) | (1u64 << (SIGSTOP - 1));

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
    // AGENT: allocate one compact action slot for every Linux/RISC-V signal;
    // signal_index() maps public numbers 1..=NSIG onto these NSIG slots.
    pub fn new() -> Self {
        let mut actions = Vec::with_capacity(NSIG as usize);
        for _ in 0..NSIG {
            actions.push(SigAction::default_action());
        }
        Self { actions }
    }

    // AGENT: reject invalid or uncatchable signals before translating the
    // public one-based signal number to its compact action-table index.
    pub fn set_action(&mut self, signo: u32, action: SigAction) -> bool {
        let Some(index) = signal_index(signo) else {
            return false;
        };
        if signo == SIGKILL || signo == SIGSTOP {
            return false;
        }
        self.actions[index] = action;
        true
    }

    // AGENT: make an invalid signal lookup explicit instead of aliasing it to
    // a real disposition slot now that signal 1 occupies actions[0].
    pub fn get_action(&self, signo: u32) -> Option<&SigAction> {
        self.actions.get(signal_index(signo)?)
    }

    pub fn fork_copy(&self) -> Self {
        Self {
            actions: self.actions.clone(),
        }
    }

    // AGENT: resolve only valid signal-number lookups; invalid inputs cannot
    // inherit the disposition of any real signal.
    pub fn is_ignored(&self, signo: u32) -> bool {
        self.get_action(signo)
            .is_some_and(|action| action.resolve(signo) == SignalDeliveryAction::Ignore)
    }

    // AGENT: exec visits every compact slot, resets caught handlers, preserves
    // ignored dispositions, and clears handler-only masks from every action.
    pub fn reset_for_exec(&mut self) {
        for action in &mut self.actions {
            let handler = if action.handler == SIG_IGN {
                SIG_IGN
            } else {
                SIG_DFL
            };
            *action = SigAction { handler, mask: 0 };
        }
    }
}
