use super::*;
use crate::trap::TrapFrame;

const WAIT4_WNOHANG: usize = 1;

impl Kernel {
    // AGENT: create the simulator init task and install it as CPU0's current task.
    pub fn proc_init(&self) {
        let root = self
            .tasks
            .spawn_root()
            .expect("proc_init should create the single init task");
        root.set_sched_state(TaskRunState::Running);
        root.reset_slice();
        self.set_cur(0, Some(root));
    }

    // AGENT: route SYS_EXIT through the thread-local path; only the final member
    // is allowed to commit process-level teardown and parent notification.
    pub fn do_exit_current_thread(&self, cpu: usize, code: usize) -> Result<(), &'static str> {
        let task = self.cur_task(cpu).ok_or("esrch")?;
        self.exit_current_thread(cpu, &task, ExitReason::Code((code & 0xFF) as u8))
    }

    // AGENT: route SYS_EXIT_GROUP through the process-wide path and retain one
    // NoReturn control-flow result for the trap owner.
    pub fn do_exit_group_current(&self, cpu: usize, code: usize) -> Result<(), &'static str> {
        let task = self.cur_task(cpu).ok_or("esrch")?;
        self.exit_thread_group(cpu, &task, ExitReason::Code((code & 0xFF) as u8));
        Ok(())
    }

    // AGENT: retire one Task without touching shared resources unless the
    // lifecycle lock proves that this caller is the process's final thread.
    pub(crate) fn exit_current_thread(
        &self,
        cpu: usize,
        task: &Arc<Task>,
        reason: ExitReason,
    ) -> Result<(), &'static str> {
        let process = task.process.clone();
        match process.begin_thread_exit(task.id(), reason)? {
            ThreadExitDecision::NonLast => {
                self.release_exited_thread(cpu, task);
                let removed = self.tasks.remove_exited_thread(task.id(), &process);
                debug_assert!(removed);
                if !self.scheduler_active(cpu) {
                    self.switch_away_from_exited_current(cpu, task.id());
                }
            }
            ThreadExitDecision::Last => {
                self.finish_process_exit(cpu, task, &process, vec![task.id()]);
            }
        }
        Ok(())
    }

    // AGENT: make exit_group and default-fatal signals share the exact same
    // Running -> Exiting transition and all-thread teardown sequence.
    pub(crate) fn exit_thread_group(&self, cpu: usize, task: &Arc<Task>, reason: ExitReason) {
        let process = task.process.clone();
        let Some(thread_ids) = process.begin_group_exit(reason) else {
            return;
        };
        self.finish_process_exit(cpu, task, &process, thread_ids);
    }

    // AGENT: own the process-level half of exit after an atomic lifecycle gate:
    // retire every Task, release shared state, reparent, publish Zombie, notify.
    fn finish_process_exit(
        &self,
        cpu: usize,
        task: &Arc<Task>,
        process: &Arc<Process>,
        thread_ids: Vec<Tid>,
    ) {
        let parent = process.parent();
        let child_pid = process.pid();

        self.release_exit_thread_resources(cpu, task, process, thread_ids);
        process.release_exit_resources();
        self.tasks.reparent_children_to_init(&process);
        process.finish_process_exit();

        if let Some(parent) = parent {
            self.send_signal_to_process(&parent, SIGCHLD as i32, child_pid as isize);
        }
        if !self.scheduler_active(cpu) {
            self.switch_away_from_exited_current(cpu, task.id());
        }
    }

    // AGENT: publish one thread's Zombie state and free its private resources,
    // deferring a live scheduler-owned kernel stack until idle resumes.
    fn release_exited_thread(&self, cpu: usize, task: &Arc<Task>) {
        let running_task_id = if self.scheduler_active(cpu) {
            self.cur_task(cpu).map(|current| current.id())
        } else {
            None
        };
        if running_task_id == Some(task.id()) {
            task.mark_thread_exited();
        } else {
            task.release_thread_exit_resources();
        }
        self.run_queue.remove(task.id());
    }

    // AGENT: release each same-process thread exactly once and detach it from
    // runnable scheduler state; the requested task is handled even if the thread
    // list was stale or incomplete.
    fn release_exit_thread_resources(
        &self,
        cpu: usize,
        task: &Arc<Task>,
        process: &Arc<Process>,
        thread_ids: Vec<Tid>,
    ) {
        let mut released_requested_task = false;

        for tid in thread_ids {
            if let Some(thread) = self.tasks.find(tid) {
                if !Arc::ptr_eq(&thread.process, process) {
                    continue;
                }
                if thread.id() == task.id() {
                    released_requested_task = true;
                }
                self.release_exited_thread(cpu, &thread);
            }
        }

        if !released_requested_task {
            self.release_exited_thread(cpu, task);
        }
    }

    // AGENT: current QEMU scheduling is CPU0-only; this keeps that policy local
    // while making the exit path read as a plain teardown sequence.
    fn switch_away_from_exited_current(&self, cpu: usize, task_id: usize) {
        if cpu == 0
            && self
                .cur_task(cpu)
                .as_ref()
                .is_some_and(|current| current.id() == task_id)
        {
            self.set_cur(cpu, None);
            self.schedule_next_runnable(cpu);
        }
    }

    // AGENT: keep fork as a small orchestration layer; TaskTable owns process
    // construction/publication, while Kernel only publishes it to the scheduler.
    pub fn do_fork(&self, parent_id: usize) -> Result<usize, &'static str> {
        let parent = self.tasks.find(parent_id).ok_or("esrch")?;
        let caller_frame = parent.snapshot_user_trap_frame()?;
        self.do_fork_from_frame(parent_id, &caller_frame)
    }

    // AGENT: fork from the complete frame captured by the active trap path so
    // the child inherits every user register at the post-ecall continuation.
    pub(crate) fn do_fork_from_frame(
        &self,
        parent_id: usize,
        caller_frame: &TrapFrame,
    ) -> Result<usize, &'static str> {
        let parent = self.tasks.find(parent_id).ok_or("esrch")?;
        if parent.done() {
            return Err("esrch");
        }

        let child = self.tasks.fork_process_from_frame(&parent, caller_frame)?;
        let child_id = child.id();
        child.set_sched_state(TaskRunState::Runnable);
        child.reset_slice();
        self.run_queue.enqueue(&child);
        Ok(child_id)
    }

    // AGENT: wait for a matching child to become reapable, but leave the final
    // zombie deletion to sys_wait4 after any userspace status copyout succeeds.
    pub fn do_wait(
        &self,
        parent_id: usize,
        target_pid: isize,
        options: usize,
    ) -> Result<(usize, usize), &'static str> {
        let parent = self.tasks.process_of_tid(parent_id).ok_or("esrch")?;
        let wnohang = (options & WAIT4_WNOHANG) != 0;

        loop {
            if let Some(child) = self.find_waitable_child(&parent, target_pid)? {
                return Ok(child);
            }

            if wnohang {
                return Ok((0, 0));
            }

            let wait = Self::prepare_child_wait(&parent, parent_id);
            if let Some(child) = self.find_waitable_child(&parent, target_pid)? {
                Self::cancel_child_wait(&parent, wait);
                return Ok(child);
            }

            let outcome = wait.0.wait_interruptible(None);
            Self::cancel_child_wait(&parent, wait);
            if outcome == WaitOutcome::Signal {
                return Err("eintr");
            }
        }
    }

    // AGENT: commit the destructive half of wait4 after the syscall layer has
    // successfully copied any wait status to userspace.
    pub fn reap_waited_child(&self, child_pid: usize) -> Result<(), &'static str> {
        self.tasks.reap(child_pid)
    }

    // AGENT: scan only the parent's current child list; blocking and reaping are
    // separate so the control flow mirrors wait4's observable phases.
    fn find_waitable_child(
        &self,
        parent: &Arc<Process>,
        target_pid: isize,
    ) -> Result<Option<(usize, usize)>, &'static str> {
        let children = parent.children_snapshot();
        if children.is_empty() {
            return Err("echild");
        }

        let mut matched = false;
        for child in &children {
            if !self.child_matches_wait_target(parent, child, target_pid) {
                continue;
            }

            matched = true;
            if let Some(status) = child.zombie_wait_status() {
                return Ok(Some((child.pid(), status)));
            }
        }

        if matched {
            Ok(None)
        } else {
            Err("echild")
        }
    }

    // AGENT: keep pid and process-group selection in one readable predicate.
    fn child_matches_wait_target(
        &self,
        parent: &Process,
        child: &Process,
        target_pid: isize,
    ) -> bool {
        match target_pid {
            -1 => true,
            0 => match (
                self.tasks.process_pgid(child.pid()),
                self.tasks.process_pgid(parent.pid()),
            ) {
                (Some(child_pgid), Some(parent_pgid)) => child_pgid == parent_pgid,
                _ => false,
            },
            pid if pid > 0 => child.pid() == pid as usize,
            pgid => self.tasks.process_pgid(child.pid()) == Some((-pgid) as i32),
        }
    }

    // AGENT: clear stale child-exit readiness before subscribing so a later
    // child exit changes the event bits and wakes this one-shot waiter.
    fn prepare_child_wait(parent: &Arc<Process>, parent_task_id: usize) -> (WaitToken, usize) {
        let token = WaitToken::for_task(parent_task_id);
        let wake_token = token.clone();
        let sub_id = {
            let mut ev = parent.ev.lock().unwrap();
            ev.clear(EvFlag::CHILD_QUIT);
            ev.sub(
                EvFlag::CHILD_QUIT,
                Box::new(move |_| {
                    wake_token.wake();
                    true
                }),
            )
        };
        (token, sub_id)
    }

    // AGENT: remove the one-shot subscription when wait4 returns or is
    // interrupted before the child-exit event fires.
    fn cancel_child_wait(parent: &Arc<Process>, (_token, sub_id): (WaitToken, usize)) {
        parent.ev.lock().unwrap().unsub(sub_id);
    }
}
