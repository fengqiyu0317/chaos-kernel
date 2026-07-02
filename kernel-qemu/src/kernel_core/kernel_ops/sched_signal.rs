use super::*;

impl Kernel {
    // AGENT: wait-token scheduling is still single-hart, so "current" means the
    // CPU0 task whose kernel stack is actively executing.
    fn is_current_task_on_cpu0(&self, task_id: usize) -> bool {
        self.cur_task(0)
            .map(|current| current.id() == task_id)
            .unwrap_or(false)
    }

    // AGENT: enqueue only runnable tasks that are not held by job-control stop.
    fn enqueue_task_if_ready(&self, task: &Arc<Task>) {
        if task.sched_state() == TaskRunState::Runnable
            && !task.is_job_stopped()
            && !self.is_current_task_on_cpu0(task.id())
        {
            self.run_queue.enqueue(task.id(), task.sched_policy());
        }
    }

    // AGENT: record that a wait/event completed; job-stopped tasks become
    // runnable in scheduler state but stay off the run queue until SIGCONT.
    fn make_task_runnable(&self, task: &Arc<Task>) {
        task.set_sched_state(TaskRunState::Runnable);
        self.enqueue_task_if_ready(task);
    }

    // AGENT: common QEMU wait-token wake path. Event, futex, epoll, and timer
    // wakeups should make a sleeping task runnable through the run queue instead
    // of unparking a host thread.
    pub(crate) fn wake_task_for_wait(&self, task_id: usize) -> bool {
        let Some(task) = self.tasks.find(task_id) else {
            return false;
        };
        if task.done() {
            return false;
        }
        if task.sched_state() == TaskRunState::Sleeping {
            self.make_task_runnable(&task);
        }
        true
    }

    // AGENT: common QEMU wait-token block path. This records the task as
    // sleeping and removes it from runnable/current scheduler state; the later
    // context-switch milestone will make this transition actually suspend the
    // current kernel stack.
    pub(crate) fn block_task_for_wait(&self, task_id: usize) -> bool {
        let Some(task) = self.tasks.find(task_id) else {
            return false;
        };
        if task.done() {
            return false;
        }
        if task.is_job_stopped() {
            return false;
        }
        if task.sched_state() == TaskRunState::Sleeping {
            return true;
        }
        task.set_sched_state(TaskRunState::Sleeping);
        self.run_queue.remove(task_id);
        if self.is_current_task_on_cpu0(task_id) {
            self.run_queue.clear_current();
        }
        true
    }

    // AGENT: restore the current task after the temporary spin-based wait bridge
    // observes a token completion. A later real context-switch path can remove
    // this current-stack repair step.
    pub(crate) fn finish_task_wait(&self, task_id: usize) -> bool {
        let Some(task) = self.tasks.find(task_id) else {
            return false;
        };
        if task.done() {
            return false;
        }
        if task.is_job_stopped() {
            return false;
        }
        if self.is_current_task_on_cpu0(task_id) {
            self.run_queue.remove(task_id);
            task.set_sched_state(TaskRunState::Running);
            task.reset_slice();
            self.run_queue.set_current(task_id);
            return true;
        }

        if task.sched_state() == TaskRunState::Sleeping {
            task.set_sched_state(TaskRunState::Runnable);
            self.enqueue_task_if_ready(&task);
        }
        true
    }

    // AGENT: update the task-owned priority and refresh the run queue only for
    // tasks that are already runnable.
    pub fn boost_task_priority(&self, task_id: usize, amount: i32) -> bool {
        let Some(task) = self.tasks.find(task_id) else {
            return false;
        };
        if task.done() {
            return false;
        }

        let policy = task.boost_priority(amount);
        if task.sched_state() == TaskRunState::Runnable && !task.is_job_stopped() {
            self.run_queue.enqueue(task_id, policy);
        }
        true
    }

    // AGENT: central signal send path so pending-signal enqueue and scheduler
    // wakeup stay together.
    pub fn send_signal_to_task(&self, task: &Arc<Task>, signo: i32, sender_tid: isize) {
        if signo <= 0 || signo as u32 >= NSIG {
            return;
        }
        let signo = signo as u32;
        let queued = task.enqueue_signal(signo as i32, sender_tid);
        if task.done() || (!queued && signo != SIGCONT && signo != SIGKILL) {
            return;
        }
        match signo {
            SIGCONT => {
                task.set_job_stopped(false);
                self.enqueue_task_if_ready(task);
            }
            SIGKILL => {
                task.set_job_stopped(false);
                if task.sched_state() == TaskRunState::Sleeping {
                    self.make_task_runnable(task);
                } else {
                    self.enqueue_task_if_ready(task);
                }
            }
            _ if task.is_job_stopped() => {}
            _ if task.sched_state() == TaskRunState::Sleeping => {
                self.wake_task_for_wait(task.id());
            }
            _ => {}
        }
    }

    // AGENT: WaitToken interruptible waits query this instead of removing the
    // signal; actual delivery still belongs to the syscall/schedule boundary.
    pub(crate) fn task_has_interrupting_signal(&self, task_id: usize) -> bool {
        self.tasks
            .find(task_id)
            .is_some_and(|task| !task.done() && task.has_interrupting_signal())
    }

    // AGENT: deliver pending signals at simulator scheduling/syscall boundaries.
    pub fn deliver_pending_signals(&self, cpu: usize) -> usize {
        self.deliver_pending_signals_inner(cpu, None).0
    }

    // AGENT: QEMU syscall return uses the live TrapFrame as the interrupted
    // context, then receives an updated Context only when a handler is entered.
    pub(crate) fn deliver_pending_signals_from_context(
        &self,
        cpu: usize,
        interrupted: Context,
    ) -> Option<Context> {
        self.deliver_pending_signals_inner(cpu, Some(interrupted)).1
    }

    // AGENT: expose the current simulated user context to the RISC-V ABI layer
    // for sigreturn, where the restored context is the syscall result.
    pub(crate) fn current_user_context(&self, cpu: usize) -> Option<Context> {
        self.cur_task(cpu)
            .as_ref()
            .and_then(|task| task_user_context(task))
    }

    fn deliver_pending_signals_inner(
        &self,
        cpu: usize,
        mut active_context: Option<Context>,
    ) -> (usize, Option<Context>) {
        if cpu != 0 {
            return (0, None);
        }
        let task = match self.cur_task(cpu) {
            Some(task) => task,
            None => return (0, None),
        };
        let mut delivered = 0usize;
        let mut updated_context = None;
        while let Some(sig) = task.take_deliverable_signal() {
            delivered += 1;
            match sig.action.handler {
                SIG_IGN => continue,
                SIG_DFL => match sig.signo {
                    SIGCHLD => continue,
                    SIGCONT => continue,
                    SIGSTOP => {
                        task.set_job_stopped(true);
                        task.set_sched_state(TaskRunState::Runnable);
                        self.run_queue.remove(task.id());
                        self.run_queue.clear_current();
                        self.set_cur(cpu, None);
                        self.schedule_next_runnable(cpu);
                        break;
                    }
                    _ => {
                        self.exit_task(cpu, &task, ExitReason::Signal(sig.signo as u8));
                        break;
                    }
                },
                handler => {
                    let interrupted =
                        match active_context.take().or_else(|| task_user_context(&task)) {
                            Some(ctx) => ctx,
                            None => {
                                task.requeue_signal_front(sig.signo as i32, sig.sender_tid);
                                break;
                            }
                        };
                    match enter_signal_handler(&task, sig, handler, interrupted) {
                        Some(ctx) => {
                            active_context = Some(ctx.clone());
                            updated_context = Some(ctx);
                        }
                        None => {
                            break;
                        }
                    }
                    break;
                }
            }
        }
        (delivered, updated_context)
    }

    // AGENT: advance global timers after CPU0 has advanced the logical clock.
    pub(crate) fn advance_timers(&self) {
        let fired = {
            let mut timers = self.timers.lock();
            timers.advance()
        };

        for timer in fired {
            self.dispatch_timer(timer);
        }
    }

    // AGENT: dispatch typed timer expiry targets into the existing wake/signal
    // paths after the timer wheel lock has been released.
    fn dispatch_timer(&self, timer: TimerEntry) {
        match timer.target {
            TimerTarget::Noop => {}
            TimerTarget::WakeToken { token } => {
                token.wake_timeout();
            }
            TimerTarget::WakeTask { task_id } => {
                self.wake_task_for_wait(task_id);
            }
            TimerTarget::SignalTask {
                task_id,
                signo,
                sender_tid,
            } => {
                if let Some(task) = self.tasks.find(task_id) {
                    self.send_signal_to_task(&task, signo, sender_tid);
                }
            }
        }
    }

    // AGENT: CPU0 owns logical timer progression; other CPUs only update CLK_ALL.
    pub fn schedule_tick(&self, cpu: usize) {
        dtk(cpu);
        if cpu == 0 {
            self.advance_timers();
        }
        if cpu != 0 {
            return;
        }
        match self.cur_task(cpu) {
            Some(t) if t.done() => {
                t.set_sched_state(TaskRunState::Zombie);
                self.run_queue.remove(t.id());
                self.schedule_next_runnable(cpu);
            }
            Some(t) if t.sched_state() != TaskRunState::Running => {}
            Some(t) => {
                if t.tick_slice() {
                    if self.run_queue.len() == 0 {
                        t.reset_slice();
                    } else {
                        // AGENT: A ready peer gets the CPU at slice expiry;
                        // reuse the run-queue current marker for the requeue.
                        t.set_sched_state(TaskRunState::Runnable);
                        if !self.run_queue.yield_current(t.sched_policy()) {
                            self.run_queue.enqueue(t.id(), t.sched_policy());
                        }
                        self.schedule_next_runnable(cpu);
                    }
                }
            }
            None => {
                // AGENT: An idle CPU immediately pulls runnable work.
                self.schedule_next_runnable(cpu);
            }
        }
    }

    pub(crate) fn schedule_next_runnable(&self, cpu: usize) -> bool {
        if cpu != 0 {
            return false;
        }
        while let Some((id, _policy)) = self.run_queue.dequeue() {
            match self.tasks.find(id) {
                Some(task)
                    if !task.done()
                        && !task.is_job_stopped()
                        && task.sched_state() == TaskRunState::Runnable =>
                {
                    task.set_sched_state(TaskRunState::Running);
                    task.reset_slice();
                    self.set_cur(cpu, Some(task));
                    self.run_queue.set_current(id);
                    self.deliver_pending_signals(cpu);
                    return true;
                }
                Some(task) if task.done() => {
                    task.set_sched_state(TaskRunState::Zombie);
                }
                _ => {}
            }
        }
        self.set_cur(cpu, None);
        self.run_queue.clear_current();
        false
    }
}

// AGENT: read the task's saved simulated user context without committing to a
// signal frame yet; QEMU TrapFrame delivery supplies a fresher context instead.
fn task_user_context(task: &Task) -> Option<Context> {
    task.thd_ctx
        .lock()
        .unwrap()
        .as_ref()
        .map(|ctx| ctx.uctx.clone())
}

// AGENT: install one userspace handler frame and return the context that must
// run next, keeping mask/frame mutation in one place for simulator and TrapFrame delivery.
fn enter_signal_handler(
    task: &Task,
    sig: PendingSignal,
    handler: usize,
    interrupted: Context,
) -> Option<Context> {
    let old_mask = *task.sig_mask.lock().unwrap();
    let mut thd = task.thd_ctx.lock().unwrap();
    let Some(ctx) = thd.as_mut() else {
        task.requeue_signal_front(sig.signo as i32, sig.sender_tid);
        return None;
    };
    ctx.sig_frames.push(SigFrame {
        saved_ctx: interrupted.clone(),
        saved_mask: old_mask,
    });
    let next_mask = (old_mask | sig.action.mask | (1u64 << sig.signo))
        & !((1u64 << SIGKILL) | (1u64 << SIGSTOP));
    *task.sig_mask.lock().unwrap() = next_mask;
    ctx.smask = next_mask;
    ctx.uctx = interrupted;
    ctx.uctx.r[0] = sig.signo as u64;
    ctx.uctx.r[1] = sig.sender_tid as u64;
    ctx.uctx.r[2] = ctx.sig_frames.last().unwrap().saved_ctx.ip;
    ctx.uctx.set_ip(handler as u64);
    Some(ctx.uctx.clone())
}
