use super::*;

const ROBUST_LIST_LIMIT: usize = 2048;
const FUTEX_TID_MASK: u32 = 0x3fff_ffff;
const FUTEX_OWNER_DIED: u32 = 0x4000_0000;
const FUTEX_WAITERS: u32 = 0x8000_0000;

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

    // AGENT: start group termination once, override job-control stop, cancel
    // each sibling's concrete wait, and retire only the calling thread.
    pub(crate) fn exit_thread_group(
        &self,
        cpu: usize,
        current: &Arc<Task>,
        reason: ExitReason,
    ) -> Result<(), &'static str> {
        let process = current.process.clone();
        let start = process.begin_group_exit(current.id(), reason)?;

        process.set_job_stopped(false);
        if let GroupExitStart::Started(thread_ids) = start {
            for tid in thread_ids {
                if tid == current.id() {
                    continue;
                }
                let Some(task) = self.tasks.find_task(tid) else {
                    continue;
                };
                if !Arc::ptr_eq(&task.process, &process) {
                    continue;
                }
                task.cancel_active_wait_for_group_exit();
                self.make_task_runnable_for_group_exit(&task);
            }
        }

        self.retire_current_thread(cpu, current, None)
    }

    // AGENT: retire the currently executing thread for both ordinary and group
    // exit, then let only the last member commit process-owned teardown.
    pub(crate) fn retire_current_thread(
        &self,
        cpu: usize,
        current: &Arc<Task>,
        reason_if_running: Option<ExitReason>,
    ) -> Result<(), &'static str> {
        let process = current.process.clone();
        self.prepare_current_thread_retirement(cpu, current)?;
        let decision = process.complete_thread_exit(current.id(), reason_if_running)?;
        debug_assert!(
            !current.has_active_wait(),
            "thread exited before its active wait stack cleaned up"
        );
        current.set_sched_state(TaskRunState::Zombie);
        self.run_queue.remove(current.id());
        let removed = self.tasks.remove_exited_thread(current.id(), &process);
        assert!(removed, "retired current thread was absent from task table");
        if decision == ThreadExitDecision::Last {
            self.commit_process_exit(&process);
        }
        Ok(())
    }

    // AGENT: validate stack ownership, complete Task-owned ABI cleanup, and
    // discard thread-local resources before lifecycle acknowledgement.
    pub(crate) fn prepare_current_thread_retirement(
        &self,
        cpu: usize,
        current: &Arc<Task>,
    ) -> Result<(), &'static str> {
        if self
            .cur_task(cpu)
            .is_none_or(|task| !Arc::ptr_eq(&task, current))
        {
            return Err("esrch");
        }
        if current.has_active_wait() {
            return Err("ebusy");
        }

        let clear_child_tid = current.take_clear_child_tid();
        if clear_child_tid != 0
            && current
                .process
                .addr_space
                .lock()
                .unwrap()
                .write_user_bytes(clear_child_tid, &0u32.to_ne_bytes(), &self.pool)
                .is_ok()
        {
            current.process.futex.wake(clear_child_tid, 1);
        }
        self.release_current_robust_futexes(current);
        if let Some(timer_wheel) = TIMER_WHEEL.get() {
            timer_wheel.lock().cancel_task_targets(current.id());
        }
        current.release_exit_resources();
        Ok(())
    }

    // AGENT: walk the registered RV64 robust list under the single-hart address
    // space lock, publish OWNER_DIED for words owned by this TID, then wake each
    // futex only after releasing the address-space lock.
    fn release_current_robust_futexes(&self, current: &Arc<Task>) {
        let head_addr = current.take_robust_list_head();
        if head_addr == 0 {
            return;
        }

        let mut wake_addrs = Vec::new();
        let mut visited = BTreeSet::new();
        let mut addr_space = current.process.addr_space.lock().unwrap();
        let mut head = [0u8; ROBUST_LIST_HEAD_SIZE];
        if addr_space.read_user_bytes(head_addr, &mut head).is_err() {
            return;
        }
        let first = usize::from_ne_bytes(head[0..8].try_into().unwrap());
        let futex_offset = isize::from_ne_bytes(head[8..16].try_into().unwrap());
        let pending = usize::from_ne_bytes(head[16..24].try_into().unwrap());

        let mut node = first;
        while node != 0
            && node != head_addr
            && visited.len() < ROBUST_LIST_LIMIT
            && visited.insert(node)
        {
            if let Some(futex_addr) = robust_futex_addr(node, futex_offset) {
                if mark_robust_futex_owner_dead(
                    &mut addr_space,
                    &self.pool,
                    futex_addr,
                    current.id(),
                ) {
                    wake_addrs.push(futex_addr);
                }
            }
            let mut next = [0u8; mem::size_of::<usize>()];
            if addr_space.read_user_bytes(node, &mut next).is_err() {
                break;
            }
            node = usize::from_ne_bytes(next);
        }

        if pending != 0 && visited.insert(pending) {
            if let Some(futex_addr) = robust_futex_addr(pending, futex_offset) {
                if mark_robust_futex_owner_dead(
                    &mut addr_space,
                    &self.pool,
                    futex_addr,
                    current.id(),
                ) {
                    wake_addrs.push(futex_addr);
                }
            }
        }
        drop(addr_space);

        for futex_addr in wake_addrs {
            current.process.futex.wake(futex_addr, 1);
        }
    }

    // AGENT: commit process-owned teardown only after its lifecycle set proves
    // every Task has confirmed and left the task table.
    pub(crate) fn commit_process_exit(&self, process: &Arc<Process>) {
        if self
            .tasks
            .init_process()
            .is_some_and(|init| Arc::ptr_eq(&init, process))
        {
            if let Err(error) = self.vfs.root_fs().flush() {
                crate::println!("[kernel-qemu] root filesystem flush failed: {}", error);
            }
            crate::println!("[kernel-qemu] init process exited");
            crate::sbi::shutdown();
        }

        let parent = process.parent();
        let child_pid = process.pid();

        self.record_locks.release_process(child_pid);
        process.release_exit_resources();
        let adopted_zombie_pids = self.tasks.reparent_children_to_init(process);
        assert!(
            process.finish_process_exit(),
            "last thread failed to publish process Zombie"
        );

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
}

// AGENT: apply one signed robust-list offset without wrapping the user address.
fn robust_futex_addr(node: usize, offset: isize) -> Option<usize> {
    if offset >= 0 {
        node.checked_add(offset as usize)
    } else {
        node.checked_sub(offset.unsigned_abs())
    }
}

// AGENT: perform the Linux robust-futex owner-death word transition while the
// single-hart address-space lock excludes another kernel writer.
fn mark_robust_futex_owner_dead(
    addr_space: &mut AddrSpace,
    pool: &FramePool,
    futex_addr: usize,
    tid: usize,
) -> bool {
    if futex_addr % mem::size_of::<u32>() != 0 {
        return false;
    }
    let mut bytes = [0u8; mem::size_of::<u32>()];
    if addr_space.read_user_bytes(futex_addr, &mut bytes).is_err() {
        return false;
    }
    let old = u32::from_ne_bytes(bytes);
    if old & FUTEX_TID_MASK != (tid as u32 & FUTEX_TID_MASK) {
        return false;
    }
    let owner_dead = (old & FUTEX_WAITERS) | FUTEX_OWNER_DIED;
    addr_space
        .write_user_bytes(futex_addr, &owner_dead.to_ne_bytes(), pool)
        .is_ok()
}
