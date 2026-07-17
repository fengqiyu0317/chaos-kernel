// AGENT: keep task-local signal queue, mask, and handler-frame transitions
// separate from generic task lifecycle and Kernel scheduler orchestration.
use super::*;
use crate::trap::TrapFrame;

// AGENT: centralize signal operations whose mutable state belongs to one Task
// or its shared ProcessState rather than to the Kernel scheduler.
impl Task {
    // AGENT: enqueue a non-duplicated standard pending signal for this process.
    pub(crate) fn enqueue_signal(&self, signo: i32, sender_tid: isize) -> bool {
        if signo <= 0 || signo as u32 > NSIG {
            return false;
        }
        let mut sq = self.process.sig_queue.lock().unwrap();
        let sig_state = self.process.sig_state.lock().unwrap();
        if sig_state.is_ignored(signo as u32) {
            return false;
        }
        drop(sig_state);
        if sq.iter().any(|(sig, _)| *sig == signo) {
            return false;
        }
        sq.push_back((signo, sender_tid));
        drop(sq);
        self.process.ev.lock().unwrap().set(EvFlag::RECV_SIG);
        true
    }

    // AGENT: restore a signal that could not be delivered without user context.
    pub(crate) fn requeue_signal_front(&self, signo: i32, sender_tid: isize) {
        self.process
            .sig_queue
            .lock()
            .unwrap()
            .push_front((signo, sender_tid));
        self.process.ev.lock().unwrap().set(EvFlag::RECV_SIG);
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
            let bit = signal_bit(signo).expect("validated pending signal");
            if (mask & bit) != 0 {
                return false;
            }
            sig_state
                .get_action(signo)
                .is_some_and(|action| action.resolve(signo) != SignalDeliveryAction::Ignore)
        })
    }

    // AGENT: select the first unblocked non-ignored pending signal for delivery.
    pub fn take_deliverable_signal(&self) -> Option<PendingSignal> {
        let mask = *self.sig_mask.lock().unwrap();
        loop {
            let (signo, sender_tid) = {
                let mut sq = self.process.sig_queue.lock().unwrap();
                let pos = sq.iter().position(|(sig, _)| {
                    *sig > 0 && signal_bit(*sig as u32).is_some_and(|bit| (mask & bit) == 0)
                })?;
                sq.remove(pos)?
            };
            let action = {
                let sig_state = self.process.sig_state.lock().unwrap();
                if sig_state.is_ignored(signo as u32) {
                    continue;
                }
                sig_state.get_action(signo as u32)?.clone()
            };
            return Some(PendingSignal {
                signo: signo as u32,
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
