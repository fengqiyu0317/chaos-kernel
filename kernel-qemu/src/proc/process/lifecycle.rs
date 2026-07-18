// AGENT: keep process exit, signal-disposition, and teardown transitions
// separate from identity and process-family bookkeeping.
use super::*;

// AGENT: implement process-wide lifecycle transitions on the shared Process.
impl Process {
    // AGENT: report process death from the authoritative shared exit reason.
    pub fn is_exited(&self) -> bool {
        self.exit_reason.lock().unwrap().is_some()
    }

    // AGENT: record process death once and notify process and parent waiters.
    pub(crate) fn exit_once(&self, reason: ExitReason) -> bool {
        let mut exit_reason = self.exit_reason.lock().unwrap();
        if exit_reason.is_some() {
            return false;
        }
        *exit_reason = Some(reason);
        drop(exit_reason);

        self.ev.lock().unwrap().set(EvFlag::PROC_QUIT);
        if let Some(parent) = self.parent() {
            parent.ev.lock().unwrap().set(EvFlag::CHILD_QUIT);
        }
        true
    }

    // AGENT: expose the encoded process exit status directly to wait paths.
    pub fn wait_status(&self) -> usize {
        match *self.exit_reason.lock().unwrap() {
            Some(reason) => reason.wait_status(),
            None => 0,
        }
    }

    // AGENT: update one process-wide disposition while holding the pending
    // queue first, then discard an already-pending ignored signal.
    pub fn set_signal_action(&self, signo: u32, action: SigAction) -> bool {
        let mut sig_queue = self.sig_queue.lock().unwrap();
        let should_discard = action.resolve(signo) == SignalDeliveryAction::Ignore;
        let mut sig_state = self.sig_state.lock().unwrap();
        if !sig_state.set_action(signo, action) {
            return false;
        }
        drop(sig_state);
        if should_discard {
            sig_queue.retain(|(pending, _)| *pending != signo as i32);
        }
        true
    }

    // AGENT: move droppable resources out of locks before process teardown and
    // reclaim address-space frames without forwarding a meaningless count.
    pub fn release_exit_resources(&self) {
        let old_resources = (
            take_mutex_default(&self.fd_table),
            take_mutex_default(&self.sig_queue),
            replace_mutex_value(&self.sig_state, SigSet::new()),
        );
        let _woken_futex_waiters = self.futex.wake_all();
        self.addr_space.lock().unwrap().release_all_pages();
        drop(old_resources);
    }
}

// AGENT: move a defaultable resource out of a mutex so Drop runs unlocked.
fn take_mutex_default<T: Default>(slot: &Mutex<T>) -> T {
    let mut guard = slot.lock().unwrap();
    mem::take(&mut *guard)
}

// AGENT: replace a non-Default mutex value and drop the old value unlocked.
fn replace_mutex_value<T>(slot: &Mutex<T>, value: T) -> T {
    let mut guard = slot.lock().unwrap();
    mem::replace(&mut *guard, value)
}
