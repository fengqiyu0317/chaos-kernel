// AGENT: keep live scheduling and wait transitions together with terminal task
// teardown so the complete scheduler-visible lifecycle has one implementation.
use super::*;

// AGENT: centralize Task transitions from runnable execution through blocking,
// wakeup, terminal Zombie publication, and post-switch stack reclamation.
impl Task {
    // AGENT: read this task's scheduler placement state.
    pub fn sched_state(&self) -> TaskRunState {
        self.sched.lock().unwrap().state
    }

    // AGENT: update this task's scheduler placement state.
    pub fn set_sched_state(&self, state: TaskRunState) {
        self.sched.lock().unwrap().state = state;
    }

    // AGENT: publish the token and Sleeping state atomically at the scheduler's
    // final lost-wakeup check; only the currently Running owner may install it.
    pub(crate) fn install_active_wait(&self, token: WaitToken) -> bool {
        let mut sched = self.sched.lock().unwrap();
        if sched.state != TaskRunState::Running || sched.active_wait.is_some() {
            return false;
        }
        sched.active_wait = Some(token);
        sched.state = TaskRunState::Sleeping;
        true
    }

    // AGENT: clear one matching active wait after its kernel stack resumes; a
    // wake path may already have cleared it while making the task runnable.
    pub(crate) fn clear_active_wait(&self, token: &WaitToken) -> bool {
        let mut sched = self.sched.lock().unwrap();
        if sched
            .active_wait
            .as_ref()
            .is_none_or(|active| !active.same(token))
        {
            return false;
        }
        sched.active_wait = None;
        if sched.state == TaskRunState::Sleeping {
            sched.state = TaskRunState::Runnable;
        }
        true
    }

    // AGENT: atomically detach the wait that justified Sleeping and publish the
    // runnable state before the kernel enqueues this task.
    pub(crate) fn wake_active_wait(&self) -> bool {
        let mut sched = self.sched.lock().unwrap();
        if sched.state != TaskRunState::Sleeping || sched.active_wait.is_none() {
            return false;
        }
        sched.active_wait = None;
        sched.state = TaskRunState::Runnable;
        true
    }

    // AGENT: snapshot and cancel the concrete blocking point without retaining
    // the sched lock while WaitToken wakes through the global kernel backend.
    pub(crate) fn cancel_active_wait_for_group_exit(&self) -> bool {
        let active = self.sched.lock().unwrap().active_wait.clone();
        active.is_some_and(|token| token.cancel_for_group_exit())
    }

    // AGENT: expose whether a focused lifecycle test or exit path still has a
    // registered blocking point without leaking the token itself.
    pub(crate) fn has_active_wait(&self) -> bool {
        self.sched.lock().unwrap().active_wait.is_some()
    }

    // AGENT: report only this thread's terminal scheduler state; one sibling's
    // SYS_EXIT must never make every Task in the Process appear dead.
    pub fn done(&self) -> bool {
        self.sched_state() == TaskRunState::Zombie
    }

    // AGENT: clone the task-owned scheduling policy for queue operations.
    pub fn sched_policy(&self) -> SchedulePolicy {
        self.sched.lock().unwrap().policy.clone()
    }

    // AGENT: update the task-owned priority in place so every queued reference
    // observes the change without copying or returning a policy snapshot.
    pub fn boost_priority(&self, amount: i32) {
        let mut sched = self.sched.lock().unwrap();
        let amount = amount.max(0);
        let prio = sched.policy.prio.saturating_sub(amount);
        sched.policy = SchedulePolicy::with_prio(prio);
        sched.slice_left = sched.slice_left.min(sched.policy.time_slice());
    }

    // AGENT: reset the runtime slice from the current priority-derived policy.
    pub fn reset_slice(&self) {
        let mut sched = self.sched.lock().unwrap();
        sched.slice_left = sched.policy.time_slice();
    }

    // AGENT: consume one scheduler tick and report slice exhaustion.
    pub fn tick_slice(&self) -> bool {
        let mut sched = self.sched.lock().unwrap();
        if sched.slice_left > 0 {
            sched.slice_left -= 1;
        }
        sched.slice_left == 0
    }

    // AGENT: publish exit state, release saved signal-frame backing storage, and
    // retain a live kernel stack only until CPU0 switches back to idle.
    pub(crate) fn mark_thread_exited(&self) {
        debug_assert!(
            !self.has_active_wait(),
            "thread exited before its active wait stack cleaned up"
        );
        *self.sig_mask.lock().unwrap() = 0;
        let old_sig_frames = {
            let mut sig_frames = self.sig_frames.lock().unwrap();
            mem::take(&mut *sig_frames)
        };
        drop(old_sig_frames);
        self.set_sched_state(TaskRunState::Zombie);
    }

    // AGENT: release an exited task's stack only after __switch has returned to
    // the idle stack; dropping the currently executing stack is never allowed.
    pub(crate) fn release_kernel_stack(&self) {
        self.kstk.lock().unwrap().take();
    }
}
