use super::*;

impl Kernel {
    // AGENT: create the simulator init task and install it as CPU0's current task.
    pub fn proc_init(&self) {
        let root = self.tasks.spawn_root();
        let rid = root.id();
        root.process.threads.lock().unwrap().push(rid);
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

    pub(crate) fn exit_task(&self, cpu: usize, task: &Arc<Task>, reason: ExitReason) {
        let thread_ids = task.process.threads.lock().unwrap().clone();
        if !task.exit_proc(reason) {
            return;
        }
        let parent = task.process.parent.lock().unwrap().clone();
        let process_owner = task.process.clone();
        for tid in thread_ids {
            if let Some(thread) = self.tasks.find(tid) {
                if Arc::ptr_eq(&thread.process, &process_owner) {
                    thread.release_thread_exit_resources();
                    self.run_queue.remove(thread.id());
                }
            }
        }
        task.release_thread_exit_resources();
        let _released_pages = task.release_process_exit_resources(&self.pool);
        self.tasks.reparent_children_to_init(task);
        self.run_queue.remove(task.id());

        if cpu == 0
            && self
                .cur_task(cpu)
                .as_ref()
                .is_some_and(|current| current.id() == task.id())
        {
            self.run_queue.clear_current();
            self.set_cur(cpu, None);
            self.schedule_next_runnable(cpu);
        }

        if let Some(parent) = parent {
            self.send_signal_to_task(&parent, SIGCHLD as i32, task.id() as isize);
        }
    }

    pub fn reclaim_zombies(&self) -> usize {
        let zombies = self.tasks.zombie_tasks();
        let count = zombies.len();
        let mut _reclaimed_pages = 0usize;
        for id in &zombies {
            if let Some(t) = self.tasks.find(*id) {
                let fd_count = t.fd_count();
                _reclaimed_pages += fd_count;
            }
        }
        for id in zombies {
            self.run_queue.remove(id);
            self.tasks.reap(id);
        }
        count
    }

    // AGENT: fork keeps descriptor state while estimating shared file-node pressure.
    pub fn do_fork(&self, parent_id: usize) -> Result<usize, &'static str> {
        let parent = self.tasks.find(parent_id).ok_or("esrch")?;
        let child = self.tasks.fork_task(&parent)?;
        let child_id = child.id();
        child.set_sched_state(TaskRunState::Runnable);
        child.reset_slice();
        self.run_queue.enqueue(child_id, child.sched_policy());
        let _est_pages = {
            let files = parent.process.files.lock().unwrap();
            let mut total = 0usize;
            for (_, entry) in files.iter() {
                total += entry.metadata_pages();
            }
            total
        };
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
                self.tasks.reap(id);
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
