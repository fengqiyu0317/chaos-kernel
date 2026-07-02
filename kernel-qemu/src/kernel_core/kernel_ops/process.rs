use super::*;

impl Kernel {
    // AGENT: create the simulator init task and install it as CPU0's current task.
    pub fn proc_init(&self) {
        let root = self
            .tasks
            .spawn_root()
            .expect("proc_init should create the single init task");
        let rid = root.id();
        root.set_sched_state(TaskRunState::Running);
        root.reset_slice();
        self.set_cur(0, Some(root));
        self.run_queue.set_current(rid);
    }

    pub fn do_exit_current(&self, cpu: usize, code: usize) -> Result<(), &'static str> {
        let task = self.cur_task(cpu).ok_or("esrch")?;
        self.exit_task(cpu, &task, ExitReason::Code((code & 0xFF) as u8));
        Ok(())
    }

    // AGENT: keep process-exit teardown centralized: record death once, drop
    // thread/process resources, switch away from the dead current task, and
    // notify the parent with the child process id.
    pub(crate) fn exit_task(&self, cpu: usize, task: &Arc<Task>, reason: ExitReason) {
        let thread_ids = task.process.threads.lock().unwrap().clone();
        if !task.exit_proc(reason) {
            return;
        }

        let parent = task.process.parent.lock().unwrap().clone();
        let child_pid = task.process_pid();

        self.release_exit_thread_resources(task, thread_ids);
        task.release_process_exit_resources(&self.pool);
        self.tasks.reparent_children_to_init(task);
        self.switch_away_from_exited_current(cpu, task.id());

        if let Some(parent) = parent {
            self.send_signal_to_task(&parent, SIGCHLD as i32, child_pid as isize);
        }
    }

    // AGENT: release each same-process thread exactly once and detach it from
    // runnable scheduler state; the requested task is handled even if the thread
    // list was stale or incomplete.
    fn release_exit_thread_resources(&self, task: &Arc<Task>, thread_ids: Vec<usize>) {
        let process = task.process.clone();
        let mut released_requested_task = false;

        for tid in thread_ids {
            if let Some(thread) = self.tasks.find(tid) {
                if !Arc::ptr_eq(&thread.process, &process) {
                    continue;
                }
                if thread.id() == task.id() {
                    released_requested_task = true;
                }
                thread.release_thread_exit_resources(&self.pool);
                self.run_queue.remove(thread.id());
            }
        }

        if !released_requested_task {
            task.release_thread_exit_resources(&self.pool);
            self.run_queue.remove(task.id());
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
            self.run_queue.clear_current();
            self.set_cur(cpu, None);
            self.schedule_next_runnable(cpu);
        }
    }

    // AGENT: force-reap zombies for maintenance paths; normal wait4 handling
    // still goes through do_wait so parents can observe the wait status first.
    pub fn reclaim_zombies(&self) -> usize {
        let zombies = self.tasks.zombie_tasks();
        let mut count = 0;
        for id in zombies {
            self.run_queue.remove(id);
            if self.tasks.reap(id).is_ok() {
                count += 1;
            }
        }
        count
    }

    // AGENT: keep fork as a small orchestration layer; TaskTable::fork_task owns
    // state copying, while Kernel only publishes the child to the scheduler.
    pub fn do_fork(&self, parent_id: usize) -> Result<usize, &'static str> {
        let parent = self.tasks.find(parent_id).ok_or("esrch")?;
        if parent.done() {
            return Err("esrch");
        }

        let child = self.tasks.fork_task(&parent, &self.pool)?;
        let child_id = child.id();
        child.set_sched_state(TaskRunState::Runnable);
        child.reset_slice();
        self.run_queue.enqueue(child_id, child.sched_policy());
        Ok(child_id)
    }

    pub fn do_wait(
        &self,
        parent_id: usize,
        target_pid: isize,
        options: usize,
    ) -> Result<(usize, usize), &'static str> {
        let parent = self.tasks.find(parent_id).ok_or("esrch")?;
        let wnohang = (options & 1) != 0;
        let children: Vec<Arc<Task>> = parent.process.subtasks.lock().unwrap().clone();
        if children.is_empty() {
            return Err("echild");
        }
        let mut matched_child = false;
        let mut found_zombie: Option<(usize, usize)> = None;
        for child in &children {
            let matches = match target_pid {
                -1 => true,
                0 => *child.process.pgid.lock().unwrap() == *parent.process.pgid.lock().unwrap(),
                p if p > 0 => child.id() == p as usize,
                p => *child.process.pgid.lock().unwrap() == (-p) as Pgid,
            };
            matched_child |= matches;
            if matches && child.done() {
                found_zombie = Some((child.id(), child.wait_status()));
                break;
            }
        }
        match found_zombie {
            Some((id, status)) => {
                self.run_queue.remove(id);
                self.tasks.reap(id)?;
                Ok((id, status))
            }
            None => {
                if !matched_child {
                    return Err("echild");
                }
                if wnohang {
                    Ok((0, 0))
                } else {
                    Err("echild")
                }
            }
        }
    }
}
