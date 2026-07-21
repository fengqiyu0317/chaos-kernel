// AGENT: keep process exit, signal-disposition, and teardown transitions
// separate from identity and process-family bookkeeping.
use super::*;

// AGENT: implement process-wide lifecycle transitions on the shared Process.
impl Process {
    // AGENT: atomically remove a non-last exiting thread, or reserve the final
    // thread while moving the whole process from Running to Exiting.
    pub fn begin_thread_exit(
        &self,
        tid: Tid,
        reason: ExitReason,
    ) -> Result<ThreadExitDecision, &'static str> {
        let mut lifecycle = self.lifecycle.lock().unwrap();
        if lifecycle.phase != ProcessPhase::Running || !lifecycle.threads.contains(&tid) {
            return Err("esrch");
        }
        if lifecycle.threads.len() == 1 {
            lifecycle.phase = ProcessPhase::Exiting(reason);
            return Ok(ThreadExitDecision::Last);
        }
        lifecycle.threads.remove(&tid);
        Ok(ThreadExitDecision::NonLast)
    }

    // AGENT: start one process-wide exit exactly once and snapshot every retained
    // TID while the same lock prevents any later thread clone from joining.
    pub fn begin_group_exit(&self, reason: ExitReason) -> Option<Vec<Tid>> {
        let mut lifecycle = self.lifecycle.lock().unwrap();
        if lifecycle.phase != ProcessPhase::Running {
            return None;
        }
        lifecycle.phase = ProcessPhase::Exiting(reason);
        Some(lifecycle.threads.iter().copied().collect())
    }

    // AGENT: publish Zombie only after process-level teardown, dispatch the final
    // process event once, and discard subscriptions that no zombie can service.
    pub fn finish_process_exit(&self) {
        let became_zombie = {
            let mut lifecycle = self.lifecycle.lock().unwrap();
            match lifecycle.phase {
                ProcessPhase::Exiting(reason) => {
                    lifecycle.phase = ProcessPhase::Zombie(reason);
                    true
                }
                ProcessPhase::Running | ProcessPhase::Zombie(_) => false,
            }
        };
        if !became_zombie {
            return;
        }

        let old_event_subscriptions = {
            let mut ev = self.ev.lock().unwrap();
            ev.set(EvFlag::PROC_QUIT);
            ev.detach_subscriptions()
        };
        drop(old_event_subscriptions);
        if let Some(parent) = self.parent() {
            parent.ev.lock().unwrap().set(EvFlag::CHILD_QUIT);
        }
    }

    // AGENT: expose only the in-progress teardown phase; Zombie remains a
    // separate wait/reap-visible state instead of being folded into this query.
    pub fn is_terminating(&self) -> bool {
        matches!(
            self.lifecycle.lock().unwrap().phase,
            ProcessPhase::Exiting(_)
        )
    }

    // AGENT: expose the only process phase wait4 and reap may observe as dead.
    pub fn is_zombie(&self) -> bool {
        matches!(
            self.lifecycle.lock().unwrap().phase,
            ProcessPhase::Zombie(_)
        )
    }

    // AGENT: return a wait status only after process teardown has committed the
    // Zombie transition; Exiting is deliberately invisible to wait4.
    pub fn zombie_wait_status(&self) -> Option<usize> {
        match self.lifecycle.lock().unwrap().phase {
            ProcessPhase::Zombie(reason) => Some(reason.wait_status()),
            ProcessPhase::Running | ProcessPhase::Exiting(_) => None,
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

    // AGENT: move droppable process resources out of locks before teardown,
    // reclaim address-space storage, and retain only zombie/reap metadata.
    pub fn release_exit_resources(&self) {
        let old_signal_actions = self.sig_state.lock().unwrap().release_for_exit();
        let old_resources = (
            take_mutex_default(&self.fd_table),
            take_mutex_default(&self.exec_path),
            take_mutex_default(&self.sig_queue),
            old_signal_actions,
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
