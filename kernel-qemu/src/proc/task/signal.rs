// AGENT: keep task-local signal queue, mask, and handler-frame transitions
// separate from generic task lifecycle and Kernel scheduler orchestration.
use super::*;
use crate::trap::TrapFrame;

// AGENT: centralize signal operations whose mutable state belongs to one Task
// or its shared Process rather than to the Kernel scheduler.
impl Task {
    // AGENT: report whether this thread may currently receive one valid signal;
    // the syscall boundary already strips unmaskable bits, but enforce them here
    // as well for kernel-generated signals and focused state tests.
    pub(crate) fn signal_is_unblocked(&self, signo: u32) -> bool {
        signal_is_unblocked_by_mask(*self.sig_mask.lock().unwrap(), signo)
    }

    // AGENT: only a caught signal or a default terminate/stop action interrupts
    // a blocking syscall; ignored/default-continue signals do not report EINTR.
    pub(crate) fn signal_interrupts_wait(&self, signo: u32) -> bool {
        if !self.signal_is_unblocked(signo) {
            return false;
        }
        self.process
            .sig_state
            .lock()
            .unwrap()
            .get_action(signo)
            .is_some_and(|action| {
                matches!(
                    action.resolve(signo),
                    SignalDeliveryAction::Handler(_)
                        | SignalDeliveryAction::Stop
                        | SignalDeliveryAction::Terminate
                )
            })
    }

    // AGENT: apply Linux generation rules to the process-wide pending queue:
    // standard signals coalesce, realtime instances queue, and SIGCONT/stop
    // signals discard their pending opposites before disposition is consulted.
    pub(crate) fn enqueue_signal(&self, signo: i32, sender_tid: isize) -> SignalEnqueueResult {
        if signo <= 0 || signo as u32 > NSIG {
            return SignalEnqueueResult::Rejected;
        }
        if self.done() || self.process.is_terminating() || self.process.is_zombie() {
            return SignalEnqueueResult::Rejected;
        }
        let signo = signo as u32;
        let mut sq = self.process.sig_queue.lock().unwrap();
        if signo == SIGCONT {
            sq.retain(|(pending, _)| !is_stop_signal(*pending));
        } else if is_stop_signal(signo as i32) {
            sq.retain(|(pending, _)| *pending != SIGCONT as i32);
        }
        let sig_state = self.process.sig_state.lock().unwrap();
        if sig_state.is_ignored(signo) {
            return SignalEnqueueResult::Ignored;
        }
        drop(sig_state);
        if !is_realtime_signal(signo) && sq.iter().any(|(pending, _)| *pending == signo as i32) {
            return SignalEnqueueResult::AlreadyPending;
        }
        sq.push_back((signo as i32, sender_tid));
        SignalEnqueueResult::Queued
    }

    // AGENT: detect pending signals that should interrupt a blocking syscall.
    pub fn has_interrupting_signal(&self) -> bool {
        let mask = *self.sig_mask.lock().unwrap();
        let sq = self.process.sig_queue.lock().unwrap();
        let sig_state = self.process.sig_state.lock().unwrap();
        sq.iter().any(|(sig, _)| {
            if *sig <= 0 || (*sig as u32) > NSIG {
                return false;
            }
            let signo = *sig as u32;
            if !signal_is_unblocked_by_mask(mask, signo) {
                return false;
            }
            sig_state.get_action(signo).is_some_and(|action| {
                matches!(
                    action.resolve(signo),
                    SignalDeliveryAction::Handler(_)
                        | SignalDeliveryAction::Stop
                        | SignalDeliveryAction::Terminate
                )
            })
        })
    }

    // AGENT: select an unblocked standard signal before realtime signals, then
    // use ascending realtime signal number while preserving FIFO among equal
    // realtime instances.
    pub fn take_deliverable_signal(&self) -> Option<PendingSignal> {
        let mask = *self.sig_mask.lock().unwrap();
        loop {
            let (signo, sender_tid) = {
                let mut queue = self.process.sig_queue.lock().unwrap();
                let pos = queue
                    .iter()
                    .enumerate()
                    .filter(|(_, (signo, _))| {
                        *signo > 0 && signal_is_unblocked_by_mask(mask, *signo as u32)
                    })
                    .min_by_key(|(_, (signo, _))| {
                        let signo = *signo as u32;
                        if is_realtime_signal(signo) {
                            (1, signo)
                        } else {
                            (0, 0)
                        }
                    })
                    .map(|(pos, _)| pos)?;
                queue.remove(pos)?
            };
            let signo = signo as u32;
            let action = self
                .process
                .sig_state
                .lock()
                .unwrap()
                .get_action(signo)?
                .clone();
            if action.resolve(signo) == SignalDeliveryAction::Ignore {
                continue;
            }
            return Some(PendingSignal {
                signo,
                sender_tid,
                action,
            });
        }
    }

    // AGENT: save the interrupted user state and build the complete frame that
    // enters one userspace signal handler.
    pub(crate) fn enter_signal_handler(
        &self,
        sig: PendingSignal,
        handler: usize,
        interrupted: TrapFrame,
    ) -> TrapFrame {
        let old_mask = *self.sig_mask.lock().unwrap();
        let interrupted_pc = interrupted.sepc;
        self.sig_frames.lock().unwrap().push(SigFrame {
            saved_frame: interrupted.clone(),
            saved_mask: old_mask,
        });
        let delivered_bit = signal_bit(sig.signo).expect("validated pending signal");
        let next_mask = (old_mask | sig.action.mask | delivered_bit) & !UNMASKABLE_SIGNAL_MASK;
        *self.sig_mask.lock().unwrap() = next_mask;

        let mut next = interrupted;
        next.regs[1] = USER_SIGTRAMP;
        next.regs[10] = sig.signo as usize;
        next.regs[11] = sig.sender_tid as usize;
        next.regs[12] = interrupted_pc;
        next.sepc = handler;
        next
    }

    // AGENT: pop the most recent handler frame and restore the mask that was
    // active before that handler was entered.
    pub(crate) fn restore_signal_frame(&self) -> Result<TrapFrame, &'static str> {
        let frame = self.sig_frames.lock().unwrap().pop().ok_or("einval")?;
        *self.sig_mask.lock().unwrap() = frame.saved_mask;
        Ok(frame.saved_frame)
    }
}

// AGENT: keep the job-control stop class explicit so enqueue-time SIGCONT
// cancellation cannot drift from default stop-action resolution.
fn is_stop_signal(signo: i32) -> bool {
    matches!(signo as u32, SIGSTOP | SIGTSTP | SIGTTIN | SIGTTOU)
}
