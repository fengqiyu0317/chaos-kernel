// AGENT: isolate task registry, process-group membership, and process/thread
// construction from the state stored by each Task.
use super::*;

// AGENT: track schedulable task ids and index process groups by process pid.
pub struct TaskTable {
    pub map: RwLock<BTreeMap<usize, Arc<Task>>>,
    // AGENT: process groups store process pids, not thread ids; the per-process
    // pgid field mirrors this authoritative membership map.
    pub groups: Mutex<BTreeMap<Pgid, Arc<ProcessGroup>>>,
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
            groups: Mutex::new(BTreeMap::new()),
            seq: AtomicUsize::new(1),
            root: Mutex::new(None),
            task_reservations: AtomicUsize::new(0),
            pool,
        }
    }

    // AGENT: add a process pid to a group while the groups map is already held.
    fn add_pid_to_group_locked(
        groups: &mut BTreeMap<Pgid, Arc<ProcessGroup>>,
        pgid: Pgid,
        sid: usize,
        pid: usize,
    ) -> Result<(), &'static str> {
        match groups.get(&pgid) {
            Some(group) => {
                if group.session_id != sid {
                    return Err("eperm");
                }
                group.add_member(pid);
            }
            None => {
                if pgid != pid as Pgid {
                    return Err("eperm");
                }
                groups.insert(pgid, Arc::new(ProcessGroup::new(pgid, pid, sid)));
            }
        }
        Ok(())
    }

    // AGENT: remove stale group membership and delete empty process groups.
    fn remove_pid_from_group_locked(
        groups: &mut BTreeMap<Pgid, Arc<ProcessGroup>>,
        pgid: Pgid,
        pid: usize,
    ) {
        let remove_group = groups
            .get(&pgid)
            .map(|group| {
                group.remove_member(pid);
                group.is_empty()
            })
            .unwrap_or(false);
        if remove_group {
            groups.remove(&pgid);
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
        new_pgid: Pgid,
    ) -> Result<(), &'static str> {
        if new_pgid <= 0 {
            return Err("einval");
        }
        let pid = task.process_pid();
        let sid = task.process_sid();
        let old_pgid = *task.process.pgid.lock().unwrap();

        let mut groups = self.groups.lock().unwrap();
        Self::add_pid_to_group_locked(&mut groups, new_pgid, sid, pid)?;
        if old_pgid == new_pgid {
            return Ok(());
        }

        Self::remove_pid_from_group_locked(&mut groups, old_pgid, pid);
        *task.process.pgid.lock().unwrap() = new_pgid;
        Ok(())
    }

    // AGENT: make a process a session leader and sole member of its new group.
    pub fn start_new_session(&self, task: &Arc<Task>) -> Result<usize, &'static str> {
        let pid = task.process_pid();
        let old_pgid = *task.process.pgid.lock().unwrap();
        if old_pgid as usize == pid {
            return Err("eperm");
        }
        let new_pgid = pid as Pgid;
        let mut groups = self.groups.lock().unwrap();
        if groups
            .get(&new_pgid)
            .is_some_and(|group| !group.is_empty() || group.session_id != pid)
        {
            return Err("eperm");
        }

        Self::remove_pid_from_group_locked(&mut groups, old_pgid, pid);
        *task.process.sid.lock().unwrap() = pid;
        *task.process.pgid.lock().unwrap() = new_pgid;
        Self::add_pid_to_group_locked(&mut groups, new_pgid, pid, pid)?;
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
            .compare_exchange(Pid::INIT, Pid::INIT + 1, Ordering::SeqCst, Ordering::SeqCst)
            .is_err()
        {
            return Err("ebusy");
        }

        let task = self.insert_new_process(Pid::INIT)?;
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
    pub fn pgid_group(&self, pgid: Pgid) -> Vec<Arc<Task>> {
        let members = self
            .groups
            .lock()
            .unwrap()
            .get(&pgid)
            .map(|group| group.members_snapshot())
            .unwrap_or_default();
        let map = self.map.read().unwrap();
        members
            .into_iter()
            .filter_map(|pid| map.get(&pid).cloned())
            .collect()
    }

    // AGENT: publish a process once while synchronizing pid/group/thread indexes.
    pub fn register(&self, task: &Arc<Task>, pid: Pid) -> Result<(), &'static str> {
        let pid_value = pid.get();
        if pid_value == 0 || task.id() != pid_value {
            return Err("einval");
        }

        let default_pgid = Pgid::try_from(pid_value).map_err(|_| "einval")?;
        let pgid = match *task.process.pgid.lock().unwrap() {
            0 => default_pgid,
            existing if existing > 0 => existing,
            _ => return Err("einval"),
        };
        let sid = match *task.process.sid.lock().unwrap() {
            0 => pid_value,
            existing => existing,
        };

        let mut map = self.map.write().unwrap();
        if map.contains_key(&pid_value) || task.process.pid.lock().unwrap().get() != 0 {
            return Err("eexist");
        }

        {
            let mut groups = self.groups.lock().unwrap();
            Self::add_pid_to_group_locked(&mut groups, pgid, sid, pid_value)?;
        }

        *task.process.pid.lock().unwrap() = pid;
        *task.process.pgid.lock().unwrap() = pgid;
        *task.process.sid.lock().unwrap() = sid;

        {
            let mut threads = task.process.threads.lock().unwrap();
            if !threads.contains(&pid_value) {
                threads.push(pid_value);
            }
        }

        map.insert(pid_value, task.clone());
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
        let process_pgid = *task.process.pgid.lock().unwrap();
        if let Some(parent) = task.process.parent.lock().unwrap().clone() {
            parent
                .process
                .subtasks
                .lock()
                .unwrap()
                .retain(|child| !Arc::ptr_eq(&child.process, &process));
        }
        self.reparent_children_to_init(&task);

        {
            let mut groups = self.groups.lock().unwrap();
            Self::remove_pid_from_group_locked(&mut groups, process_pgid, process_pid);
        }

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
        self.register(&task, Pid(id))?;
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

    // AGENT: fork inherited process state and the caller's child-side context.
    pub fn fork_task(&self, src: &Arc<Task>, pool: &FramePool) -> Result<Arc<Task>, &'static str> {
        let task_slot = self.reserve_task_slot()?;
        let proc_src = self.process_of_tid(src.id()).unwrap_or_else(|| src.clone());
        let child_id = self.seq.fetch_add(1, Ordering::SeqCst);
        let child_addr_space = {
            let mut parent_addr_space = proc_src.process.addr_space.lock().unwrap();
            let forked_addr_space = AddrSpace::fork_from(&mut parent_addr_space, pool)?;
            Arc::new(Mutex::new(forked_addr_space))
        };
        let child = Task::make_with_addr_space(child_id, child_addr_space, &self.pool)?;

        let debug_fds = proc_src.process.debug_fds.lock().unwrap().clone();
        let cwd = proc_src.process.cwd.lock().unwrap().clone();
        let exec_path = proc_src.process.exec_path.lock().unwrap().clone();
        *child.process.debug_fds.lock().unwrap() = debug_fds;
        *child.process.cwd.lock().unwrap() = cwd;
        *child.process.exec_path.lock().unwrap() = exec_path;

        // AGENT: copy fd entries and free-slot state as one inherited snapshot.
        {
            let parent_files = proc_src.process.files.lock().unwrap();
            let child_files: BTreeMap<usize, FdEntry> = parent_files
                .iter()
                .map(|(&fd, entry)| (fd, entry.fork_dup()))
                .collect();
            let child_free_fds = proc_src.process.free_fds.lock().unwrap().clone();
            *child.process.files.lock().unwrap() = child_files;
            *child.process.free_fds.lock().unwrap() = child_free_fds;
        }

        let child_ctx = src.thd_ctx.lock().unwrap().clone().map(|mut ctx| {
            ctx.uctx.set_ret(0);
            ctx
        });
        *child.thd_ctx.lock().unwrap() = child_ctx;

        // AGENT: inherit the parent group/session with a fresh pre-exec window.
        let pgid = *proc_src.process.pgid.lock().unwrap();
        let sid = *proc_src.process.sid.lock().unwrap();
        let sem_ctx = proc_src.process.sem_ctx.lock().unwrap().clone();
        let shm_ctx = proc_src.process.shm_ctx.lock().unwrap().clone();
        let sig_mask = *src.sig_mask.lock().unwrap();

        *child.process.pgid.lock().unwrap() = pgid;
        *child.process.sid.lock().unwrap() = sid;
        child.process.did_exec.store(false, Ordering::SeqCst);
        *child.process.sem_ctx.lock().unwrap() = sem_ctx;
        *child.process.shm_ctx.lock().unwrap() = shm_ctx;
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
        self.register(&child, Pid(child_id))?;
        Self::attach_child(&proc_src, &child);
        task_slot.release();
        Ok(child)
    }

    // AGENT: clone one thread context while sharing the owning process state.
    pub fn clone_thread(
        &self,
        src: &Arc<Task>,
        stack_top: u64,
        tls: u64,
        clear_tid: usize,
    ) -> Result<Arc<Task>, &'static str> {
        let task_slot = self.reserve_task_slot()?;
        let proc_src = self.process_of_tid(src.id()).ok_or("esrch")?;
        let id = self.seq.fetch_add(1, Ordering::SeqCst);
        let task = Task::make_with_process(id, proc_src.process.clone(), &self.pool)?;
        let mut ctx = src.thd_ctx.lock().unwrap().clone().ok_or("enoctx")?;
        ctx.uctx.set_ret(0);
        ctx.uctx.set_sp(stack_top);
        ctx.uctx.set_tls(tls);
        ctx.clear_tid = clear_tid;
        let caller_mask = *src.sig_mask.lock().unwrap();
        ctx.smask = caller_mask;
        *task.sig_mask.lock().unwrap() = caller_mask;
        *task.thd_ctx.lock().unwrap() = Some(ctx);
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
        *task.thd_ctx.lock().unwrap() = Some(image.thd_ctx);
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
