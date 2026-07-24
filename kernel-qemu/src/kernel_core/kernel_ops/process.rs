use super::*;

impl Kernel {
    // AGENT: create the simulator init task and install it as CPU0's current task.
    pub(crate) fn proc_init(&self) {
        let root = self
            .tasks
            .spawn_root()
            .expect("proc_init should create the single init task");
        root.set_sched_state(TaskRunState::Running);
        root.reset_slice();
        self.set_cur(0, Some(root));
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

    // AGENT: shut down when the designated init reaches process-wide exit;
    // otherwise retire every Task, release shared state, reparent, publish
    // Zombie, and notify both the old parent and init about adopted zombies.
    pub(crate) fn finish_process_exit(
        &self,
        cpu: usize,
        task: &Arc<Task>,
        process: &Arc<Process>,
        thread_ids: Vec<Tid>,
    ) {
        if self
            .tasks
            .init_process()
            .is_some_and(|init| Arc::ptr_eq(&init, process))
        {
            crate::println!("[kernel-qemu] init process exited");
            crate::sbi::shutdown();
        }

        let parent = process.parent();
        let child_pid = process.pid();

        self.release_exit_thread_resources(cpu, task, process, thread_ids);
        process.release_exit_resources();
        let adopted_zombie_pids = self.tasks.reparent_children_to_init(&process);
        process.finish_process_exit();

        if let Some(parent) = parent {
            self.send_signal_to_process(&parent, SIGCHLD as i32, child_pid as isize);
        }
        if !adopted_zombie_pids.is_empty() {
            if let Some(init_process) = self.tasks.init_process() {
                for adopted_pid in adopted_zombie_pids {
                    self.send_signal_to_process(
                        &init_process,
                        SIGCHLD as i32,
                        adopted_pid as isize,
                    );
                }
            }
        }
    }

    // AGENT: publish one thread's Zombie state and free its private resources,
    // deferring a live scheduler-owned kernel stack until idle resumes.
    pub(crate) fn release_exited_thread(&self, cpu: usize, task: &Arc<Task>) {
        let is_current = self
            .cur_task(cpu)
            .is_some_and(|current| current.id() == task.id());
        if is_current {
            assert!(
                self.scheduler_active(cpu),
                "current thread exited before CPU scheduler initialization"
            );
        }
        task.mark_thread_exited();
        if !is_current {
            task.release_kernel_stack();
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
            if let Some(thread) = self.tasks.find_task(tid) {
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
}
