// AGENT: isolate task registry, process-group membership, and process/thread
// construction from the state stored by each Task.
use super::*;
use crate::trap::TrapFrame;

// AGENT: track schedulable task ids and keep job control in one authority.
pub struct TaskTable {
    pub map: RwLock<BTreeMap<usize, Arc<Task>>>,
    job_control: Mutex<JobControl>,
    pub seq: AtomicUsize,
    pub root: Mutex<Option<Arc<Task>>>,
    // AGENT: reserve capacity before registration so concurrent creators cannot
    // all pass the N_PROC check at once.
    task_reservations: AtomicUsize,
    // AGENT: share the Kernel-owned physical-frame state with every task stack
    // allocation without introducing a global FramePool singleton.
    pool: FramePool,
}

// AGENT: keep all task-table and process-lifecycle transitions in one module.
impl TaskTable {
    // AGENT: bind task construction to the caller-provided physical-frame pool.
    pub fn new(pool: FramePool) -> Self {
        Self {
            map: RwLock::new(BTreeMap::new()),
            job_control: Mutex::new(JobControl::default()),
            seq: AtomicUsize::new(1),
            root: Mutex::new(None),
            task_reservations: AtomicUsize::new(0),
            pool,
        }
    }

    // AGENT: update both sides of the process-family relation under one helper
    // so fork and reparenting cannot forget either link.
    fn attach_child(parent: &Arc<Task>, child: &Arc<Task>) {
        let mut child_parent = child.process.parent.lock().unwrap();
        let mut children = parent.process.subtasks.lock().unwrap();
        *child_parent = Some(parent.clone());
        children.push(child.clone());
    }

    // AGENT: move a process between process groups as one state transition.
    pub fn move_process_to_group(
        &self,
        task: &Arc<Task>,
        new_pgid: i32,
    ) -> Result<(), &'static str> {
        if new_pgid <= 0 {
            return Err("einval");
        }
        let pid = task.process_pid();
        self.job_control.lock().unwrap().move_process(pid, new_pgid)
    }

    // AGENT: make a process a session leader and sole member of its new group.
    pub fn start_new_session(&self, task: &Arc<Task>) -> Result<usize, &'static str> {
        let pid = task.process_pid();
        self.job_control.lock().unwrap().start_new_session(pid)?;
        Ok(pid)
    }

    // AGENT: spawn a standalone process as its own session/process-group leader.
    pub fn spawn(&self) -> Result<Arc<Task>, &'static str> {
        let slot = self.reserve_task_slot()?;
        let id = self.seq.fetch_add(1, Ordering::SeqCst);
        let task = self.insert_new_process(id)?;
        slot.release();
        Ok(task)
    }

    // AGENT: create init as the singleton pid 1 before any ordinary process.
    pub fn spawn_root(&self) -> Result<Arc<Task>, &'static str> {
        let mut root = self.root.lock().unwrap();
        if root.is_some() {
            return Err("eexist");
        }
        if self.count() != 0 {
            return Err("ebusy");
        }

        let slot = self.reserve_task_slot()?;
        if self
            .seq
            .compare_exchange(INIT_PID, INIT_PID + 1, Ordering::SeqCst, Ordering::SeqCst)
            .is_err()
        {
            return Err("ebusy");
        }

        let task = self.insert_new_process(INIT_PID)?;
        *root = Some(task.clone());
        slot.release();
        Ok(task)
    }

    // AGENT: look up a schedulable task/thread id without treating it as a pid.
    pub fn find(&self, id: usize) -> Option<Arc<Task>> {
        self.map.read().unwrap().get(&id).cloned()
    }

    // AGENT: resolve a thread id to its explicit process leader.
    pub fn process_of_tid(&self, tid: usize) -> Option<Arc<Task>> {
        let map = self.map.read().unwrap();
        let process_pid = map.get(&tid)?.process_pid();
        map.get(&process_pid).cloned()
    }

    // AGENT: resolve authoritative process-group membership to process leaders.
    pub fn pgid_group(&self, pgid: i32) -> Vec<Arc<Task>> {
        let members = self.job_control.lock().unwrap().members(pgid);
        let map = self.map.read().unwrap();
        members
            .into_iter()
            .filter_map(|pid| map.get(&pid).cloned())
            .collect()
    }

    // AGENT: expose the authoritative process-group id without mirrored task state.
    pub fn process_pgid(&self, pid: usize) -> Option<i32> {
        self.job_control
            .lock()
            .unwrap()
            .membership(pid)
            .map(|(pgid, _)| pgid)
    }

    // AGENT: derive session identity from the process's authoritative group.
    pub fn process_sid(&self, pid: usize) -> Option<usize> {
        self.job_control
            .lock()
            .unwrap()
            .membership(pid)
            .map(|(_, sid)| sid)
    }

    // AGENT: publish a process once while synchronizing pid/group/thread indexes.
    pub fn register(&self, task: &Arc<Task>, pid: usize) -> Result<(), &'static str> {
        self.register_process(task, pid, None)
    }

    // AGENT: publish a process and optionally inherit the parent's job-control membership.
    fn register_process(
        &self,
        task: &Arc<Task>,
        pid: usize,
        parent_pid: Option<usize>,
    ) -> Result<(), &'static str> {
        if pid == UNREGISTERED_PID || task.id() != pid {
            return Err("einval");
        }

        let default_pgid = i32::try_from(pid).map_err(|_| "einval")?;
        let mut job_control = self.job_control.lock().unwrap();
        let mut map = self.map.write().unwrap();
        if map.contains_key(&pid) || *task.process.pid.lock().unwrap() != UNREGISTERED_PID {
            return Err("eexist");
        }

        let (pgid, sid) = match parent_pid {
            Some(parent_pid) => job_control.membership(parent_pid).ok_or("esrch")?,
            None => (default_pgid, pid),
        };
        job_control.add_process(pid, pgid, sid)?;

        *task.process.pid.lock().unwrap() = pid;

        {
            let mut threads = task.process.threads.lock().unwrap();
            if !threads.contains(&pid) {
                threads.push(pid);
            }
        }

        map.insert(pid, task.clone());
        Ok(())
    }

    // AGENT: reap a zombie process from parent, group, thread, and task indexes.
    pub fn reap(&self, id: usize) -> Result<(), &'static str> {
        let task = self.find(id).ok_or("esrch")?;
        if !task.done() {
            return Err("ebusy");
        }

        let process = task.process.clone();
        let process_pid = task.process_pid();
        if let Some(parent) = task.process.parent.lock().unwrap().clone() {
            parent
                .process
                .subtasks
                .lock()
                .unwrap()
                .retain(|child| !Arc::ptr_eq(&child.process, &process));
        }
        self.reparent_children_to_init(&task);

        self.job_control.lock().unwrap().remove_process(process_pid);

        let thread_ids: Vec<usize> = task.process.threads.lock().unwrap().drain(..).collect();
        let mut map = self.map.write().unwrap();
        for tid in thread_ids {
            if map
                .get(&tid)
                .is_some_and(|thread| Arc::ptr_eq(&thread.process, &process))
            {
                map.remove(&tid);
            }
        }
        map.remove(&process_pid);
        map.remove(&id);
        Ok(())
    }

    // AGENT: transfer orphaned children to init or clear their parent link.
    pub fn reparent_children_to_init(&self, task: &Arc<Task>) {
        let children: Vec<Arc<Task>> = task.process.subtasks.lock().unwrap().drain(..).collect();
        if children.is_empty() {
            return;
        }
        let init = self.root.lock().unwrap().clone();
        match init {
            Some(init_task) if init_task.id() != task.id() => {
                for child in children {
                    Self::attach_child(&init_task, &child);
                }
            }
            _ => {
                for child in children {
                    *child.process.parent.lock().unwrap() = None;
                }
            }
        }
    }

    // AGENT: report the number of registered schedulable task ids.
    pub fn count(&self) -> usize {
        self.map.read().unwrap().len()
    }

    // AGENT: share process construction and registration across spawn paths.
    fn insert_new_process(&self, id: usize) -> Result<Arc<Task>, &'static str> {
        let task = Task::make(id, &self.pool)?;
        self.register(&task, id)?;
        Ok(task)
    }

    // AGENT: reserve task-table capacity across fallible construction work.
    fn reserve_task_slot(&self) -> Result<TaskSlotReservation<'_>, &'static str> {
        loop {
            let live = self.count();
            let reserved = self.task_reservations.load(Ordering::SeqCst);
            if live.saturating_add(reserved) >= N_PROC {
                return Err("eagain");
            }
            if self
                .task_reservations
                .compare_exchange(reserved, reserved + 1, Ordering::SeqCst, Ordering::SeqCst)
                .is_ok()
            {
                return Ok(TaskSlotReservation {
                    table: self,
                    active: true,
                });
            }
        }
    }

    // AGENT: preserve the direct semantic helper by snapshotting an off-CPU
    // source task before delegating to the live-frame-aware fork path.
    pub fn fork_task(&self, src: &Arc<Task>, pool: &FramePool) -> Result<Arc<Task>, &'static str> {
        let caller_frame = src.snapshot_user_trap_frame()?;
        self.fork_task_from_frame(src, &caller_frame, pool)
    }

    // AGENT: fork inherited process state from the caller's complete live
    // RISC-V frame instead of a lossy simulator Context mirror.
    pub fn fork_task_from_frame(
        &self,
        src: &Arc<Task>,
        caller_frame: &TrapFrame,
        pool: &FramePool,
    ) -> Result<Arc<Task>, &'static str> {
        let task_slot = self.reserve_task_slot()?;
        let proc_src = self.process_of_tid(src.id()).unwrap_or_else(|| src.clone());
        let child_id = self.seq.fetch_add(1, Ordering::SeqCst);
        let child_addr_space = {
            let mut parent_addr_space = proc_src.process.addr_space.lock().unwrap();
            AddrSpace::fork_from(&mut parent_addr_space, pool)?
        };
        let child = Task::make_with_addr_space(child_id, child_addr_space, &self.pool)?;

        let exec_path = proc_src.process.exec_path.lock().unwrap().clone();
        *child.process.exec_path.lock().unwrap() = exec_path;

        // AGENT: inherit descriptor entries through the unified table snapshot.
        child.inherit_fds_from(&proc_src);

        let child_sig_frames = src.sig_frames.lock().unwrap().clone();
        *child.sig_frames.lock().unwrap() = child_sig_frames;
        let mut child_frame = caller_frame.clone();
        child_frame.set_return_value(0);
        child.install_user_trap_frame(child_frame)?;

        // AGENT: inherit thread-local state while job control is copied at registration.
        let sig_mask = *src.sig_mask.lock().unwrap();

        child.process.did_exec.store(false, Ordering::SeqCst);
        *child.sig_mask.lock().unwrap() = sig_mask;

        // AGENT: inherit signal dispositions but never pending signals.
        let sig_state = { proc_src.process.sig_state.lock().unwrap().fork_copy() };
        *child.process.sig_state.lock().unwrap() = sig_state;
        {
            let parent_policy = src.sched.lock().unwrap().policy.clone();
            let mut child_sched = child.sched.lock().unwrap();
            child_sched.policy = parent_policy;
            child_sched.slice_left = child_sched.policy.time_slice();
        }
        self.register_process(&child, child_id, Some(proc_src.process_pid()))?;
        Self::attach_child(&proc_src, &child);
        task_slot.release();
        Ok(child)
    }

    // AGENT: preserve the direct semantic helper by snapshotting an off-CPU
    // source task before delegating to the live-frame-aware clone path.
    pub fn clone_thread(
        &self,
        src: &Arc<Task>,
        stack_top: u64,
        tls: u64,
    ) -> Result<Arc<Task>, &'static str> {
        let caller_frame = src.snapshot_user_trap_frame()?;
        self.clone_thread_from_frame(src, &caller_frame, stack_top, tls)
    }

    // AGENT: clone one complete user frame while sharing the owning process state.
    pub fn clone_thread_from_frame(
        &self,
        src: &Arc<Task>,
        caller_frame: &TrapFrame,
        stack_top: u64,
        tls: u64,
    ) -> Result<Arc<Task>, &'static str> {
        let task_slot = self.reserve_task_slot()?;
        let proc_src = self.process_of_tid(src.id()).ok_or("esrch")?;
        let id = self.seq.fetch_add(1, Ordering::SeqCst);
        let task = Task::make_with_process(id, proc_src.process.clone(), &self.pool)?;
        let sig_frames = src.sig_frames.lock().unwrap().clone();
        let caller_mask = *src.sig_mask.lock().unwrap();
        *task.sig_mask.lock().unwrap() = caller_mask;
        *task.sig_frames.lock().unwrap() = sig_frames;
        let mut child_frame = caller_frame.clone();
        child_frame.set_return_value(0);
        child_frame.regs[2] = stack_top as usize;
        child_frame.regs[4] = tls as usize;
        task.install_user_trap_frame(child_frame)?;
        {
            let mut map = self.map.write().unwrap();
            if map.contains_key(&id) {
                return Err("eexist");
            }
            map.insert(id, task.clone());
        }
        proc_src.process.threads.lock().unwrap().push(id);
        task_slot.release();
        Ok(task)
    }

    // AGENT: create an initial user task through the shared exec image builder.
    pub fn new_user_task(
        &self,
        path: &str,
        elf_data: &[u8],
        args: Vec<String>,
        envs: Vec<String>,
        pool: &FramePool,
    ) -> Result<Arc<Task>, &'static str> {
        let mut image = prepare_user_image(elf_data, args, envs, pool)?;
        let task = match self.spawn() {
            Ok(task) => task,
            Err(err) => {
                image.addr_space.release_all_pages();
                return Err(err);
            }
        };

        *task.process.exec_path.lock().unwrap() = path.to_string();
        task.install_user_trap_frame(TrapFrame::for_user_entry(
            image.user_entry.entry,
            image.user_entry.stack_pointer,
        ))?;
        {
            let mut addr_space = task.process.addr_space.lock().unwrap();
            *addr_space = image.addr_space;
        }
        super::fd::install_initial_stdio(&task)?;
        // AGENT: spawn already registered pid/pgid/sid and the main thread.
        Ok(task)
    }

    // AGENT: list task ids whose owning processes have not exited.
    pub fn active_tasks(&self) -> Vec<usize> {
        self.map
            .read()
            .unwrap()
            .iter()
            .filter(|(_, task)| !task.done())
            .map(|(id, _)| *id)
            .collect()
    }

    // AGENT: report one task id per zombie process for one-time reaping.
    pub fn zombie_tasks(&self) -> Vec<usize> {
        let map = self.map.read().unwrap();
        let mut seen = BTreeSet::new();
        map.iter()
            .filter(|(_, task)| task.done())
            .filter_map(|(id, task)| {
                let pid = task.process_pid();
                seen.insert(pid).then_some(*id)
            })
            .collect()
    }
}

// AGENT: release a provisional task-table reservation on every early return.
struct TaskSlotReservation<'a> {
    table: &'a TaskTable,
    active: bool,
}

// AGENT: centralize explicit and automatic task-slot release.
impl TaskSlotReservation<'_> {
    // AGENT: consume and release a successfully used reservation exactly once.
    fn release(mut self) {
        self.release_inner();
    }

    // AGENT: decrement the shared reservation counter idempotently.
    fn release_inner(&mut self) {
        if self.active {
            self.active = false;
            self.table.task_reservations.fetch_sub(1, Ordering::SeqCst);
        }
    }
}

// AGENT: make fallible task construction automatically release reserved capacity.
impl Drop for TaskSlotReservation<'_> {
    // AGENT: delegate drop cleanup to the idempotent release helper.
    fn drop(&mut self) {
        self.release_inner();
    }
}

// AGENT: expose the synchronous carrier-thread yield used by migrated callers.
pub fn yield_now_sync() {
    thread::yield_now();
}
