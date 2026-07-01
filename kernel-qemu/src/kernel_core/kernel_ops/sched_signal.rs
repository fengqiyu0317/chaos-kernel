use super::*;

impl Kernel {
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
            task.set_sched_state(TaskRunState::Runnable);
            self.run_queue.enqueue(task.id(), task.sched_policy());
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
        task.set_sched_state(TaskRunState::Sleeping);
        self.run_queue.remove(task_id);
        let is_current = self
            .cur_task(0)
            .map(|current| current.id() == task_id)
            .unwrap_or(false);
        if is_current {
            self.run_queue.clear_current();
            self.set_cur(0, None);
            self.schedule_next_runnable(0);
        }
        true
    }

    // AGENT: central signal send path so pending-signal enqueue and scheduler
    // wakeup stay together.
    pub fn send_signal_to_task(&self, task: &Arc<Task>, signo: i32, sender_tid: isize) {
        task.enqueue_signal(signo, sender_tid);
        if task.done() {
            return;
        }
        if task.sched_state() == TaskRunState::Sleeping {
            self.wake_task_for_wait(task.id());
        }
    }

    // AGENT: deliver pending signals at simulator scheduling/syscall boundaries.
    pub fn deliver_pending_signals(&self, cpu: usize) -> usize {
        if cpu != 0 {
            return 0;
        }
        let task = match self.cur_task(cpu) {
            Some(task) => task,
            None => return 0,
        };
        let mut delivered = 0usize;
        while let Some(sig) = task.take_deliverable_signal() {
            delivered += 1;
            match sig.action.handler {
                SIG_IGN => continue,
                SIG_DFL => match sig.signo {
                    SIGCHLD => continue,
                    SIGSTOP => {
                        task.set_sched_state(TaskRunState::Sleeping);
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
                    let old_mask = *task.sig_mask.lock().unwrap();
                    let mut thd = task.thd_ctx.lock().unwrap();
                    let Some(ctx) = thd.as_mut() else {
                        task.process
                            .sig_queue
                            .lock()
                            .unwrap()
                            .push_front((sig.signo as i32, sig.sender_tid));
                        break;
                    };
                    let saved_ctx = ctx.uctx.clone();
                    ctx.sig_frames.push(SigFrame {
                        saved_ctx,
                        saved_mask: old_mask,
                    });
                    let next_mask = (old_mask | sig.action.mask | (1u64 << sig.signo))
                        & !((1u64 << SIGKILL) | (1u64 << SIGSTOP));
                    *task.sig_mask.lock().unwrap() = next_mask;
                    ctx.smask = next_mask;
                    ctx.uctx.r[0] = sig.signo as u64;
                    ctx.uctx.r[1] = sig.sender_tid as u64;
                    ctx.uctx.r[2] = ctx.sig_frames.last().unwrap().saved_ctx.ip;
                    ctx.uctx.set_ip(handler as u64);
                    break;
                }
            }
        }
        delivered
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
            Some(t) => {
                t.set_sched_state(TaskRunState::Running);
                if t.tick_slice() {
                    if self.run_queue.len() == 0 {
                        t.reset_slice();
                    } else if self.run_queue.preemptible() {
                        t.set_sched_state(TaskRunState::Runnable);
                        self.run_queue.enqueue(t.id(), t.sched_policy());
                        self.schedule_next_runnable(cpu);
                    }
                }
            }
            None => {
                if self.run_queue.preemptible() {
                    self.schedule_next_runnable(cpu);
                }
            }
        }
    }

    pub(crate) fn schedule_next_runnable(&self, cpu: usize) -> bool {
        if cpu != 0 {
            return false;
        }
        while let Some((id, _policy)) = self.run_queue.dequeue() {
            match self.tasks.find(id) {
                Some(task) if !task.done() && task.sched_state() == TaskRunState::Runnable => {
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

    pub fn balance_load(&self) -> usize {
        let cpus = self.cpus.lock().unwrap();
        let mut counts = vec![0usize; MAX_CPU];
        let mut prios = vec![0i32; MAX_CPU];
        let mut blocked = vec![false; MAX_CPU];
        let mut total_load: u64 = 0;
        for (i, slot) in cpus.iter().enumerate() {
            if let Some(ref t) = slot {
                counts[i] = t.n_children() + 1;
                prios[i] = *t.process.pgid.lock().unwrap();
                blocked[i] = t.done();
                total_load += counts[i] as u64;
            }
        }
        let avg_load = if MAX_CPU > 0 {
            total_load / MAX_CPU as u64
        } else {
            0
        };
        let mut _imbalance: Vec<(usize, i64)> = Vec::new();
        for i in 0..MAX_CPU {
            let delta = counts[i] as i64 - avg_load as i64;
            if delta.abs() > 1 {
                _imbalance.push((i, delta));
            }
        }
        _imbalance.sort_by(|a, b| b.1.cmp(&a.1));
        compute_load_balance(&counts, &prios, &blocked)
    }
}
