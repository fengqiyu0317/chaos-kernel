// AGENT: isolate task and process registries, process-family relations, and
// process/thread construction from the state stored by each entity.
use super::*;
use crate::trap::TrapFrame;

// AGENT: index schedulable threads and first-class processes separately while
// keeping job-control membership in its existing single authority.
pub struct TaskTable {
    tasks: RwLock<BTreeMap<usize, Arc<Task>>>,
    processes: RwLock<BTreeMap<usize, Arc<Process>>>,
    job_control: Mutex<JobControl>,
    pub seq: AtomicUsize,
    // AGENT: record only the designated init identity; processes remains the
    // authoritative owner and lookup table for every Process allocation.
    init_pid: Mutex<Option<usize>>,
    // AGENT: count registered and provisionally reserved task slots in one
    // atomic authority so reservation-to-publication cannot lose capacity.
    occupied_task_slots: AtomicUsize,
    // AGENT: share the Kernel-owned physical-frame state with every task stack
    // allocation without introducing a global FramePool singleton.
    pool: FramePool,
}

// AGENT: keep task/process indexes and lifecycle transitions in one module.
impl TaskTable {
    // AGENT: group task-table initialization and fallible registration plumbing.

    // AGENT: bind task construction to the caller-provided physical-frame pool.
    pub fn new(pool: FramePool) -> Self {
        Self {
            tasks: RwLock::new(BTreeMap::new()),
            processes: RwLock::new(BTreeMap::new()),
            job_control: Mutex::new(JobControl::default()),
            seq: AtomicUsize::new(1),
            init_pid: Mutex::new(None),
            occupied_task_slots: AtomicUsize::new(0),
            pool,
        }
    }

    // AGENT: atomically occupy capacity shared by registered and in-flight tasks.
    fn reserve_task_slot(&self) -> Result<TaskSlotReservation<'_>, &'static str> {
        self.occupied_task_slots
            .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |occupied| {
                (occupied < N_PROC).then_some(occupied + 1)
            })
            .map_err(|_| "eagain")?;
        Ok(TaskSlotReservation {
            table: self,
            active: true,
        })
    }

    // AGENT: return capacity only after failed construction or actual removal.
    fn release_task_slots(&self, count: usize) {
        if count == 0 {
            return;
        }
        assert!(
            self.occupied_task_slots
                .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |occupied| {
                    occupied.checked_sub(count)
                })
                .is_ok(),
            "task slot accounting underflow"
        );
    }

    // AGENT: publish one new process and its leader thread while synchronizing
    // pid, tid, job-control, and thread-membership indexes.
    fn register_process(
        &self,
        process: &Arc<Process>,
        leader: &Arc<Task>,
        parent_pid: Option<usize>,
    ) -> Result<(), &'static str> {
        let pid = process.pid();
        if pid == 0 || leader.id() != pid || !Arc::ptr_eq(&leader.process, process) {
            return Err("einval");
        }

        let default_pgid = i32::try_from(pid).map_err(|_| "einval")?;
        let mut job_control = self.job_control.lock().unwrap();
        let mut processes = self.processes.write().unwrap();
        let mut tasks = self.tasks.write().unwrap();
        if processes.contains_key(&pid) || tasks.contains_key(&pid) {
            return Err("eexist");
        }

        let (pgid, sid) = match parent_pid {
            Some(parent_pid) => job_control.membership(parent_pid).ok_or("esrch")?,
            None => (default_pgid, pid),
        };
        job_control.add_process(pid, pgid, sid)?;

        if !process.add_thread(pid) {
            job_control.remove_process(pid);
            return Err("eexist");
        }
        processes.insert(pid, process.clone());
        tasks.insert(pid, leader.clone());
        Ok(())
    }

    // AGENT: complete fallible process/task construction before publishing any
    // table entry or committing a special process identity such as init pid 1.
    fn make_new_process(&self, id: usize) -> Result<(Arc<Process>, Arc<Task>), &'static str> {
        let process = Arc::new(Process::new(id, AddrSpace::new()));
        let task = Task::make(id, process.clone(), &self.pool)?;
        Ok((process, task))
    }

    // AGENT: create Process first and then its leader Task before publication.
    fn insert_new_process(&self, id: usize) -> Result<Arc<Task>, &'static str> {
        let (process, task) = self.make_new_process(id)?;
        self.register_process(&process, &task, None)?;
        Ok(task)
    }

    // AGENT: group entry points that create a process without an existing caller.

    // AGENT: spawn a standalone process as its own session/process-group leader.
    pub fn spawn(&self) -> Result<Arc<Task>, &'static str> {
        let slot = self.reserve_task_slot()?;
        let id = self.seq.fetch_add(1, Ordering::SeqCst);
        let task = self.insert_new_process(id)?;
        slot.commit();
        Ok(task)
    }

    // AGENT: create init as the singleton pid 1 before any ordinary process.
    pub fn spawn_root(&self) -> Result<Arc<Task>, &'static str> {
        let mut init_pid = self.init_pid.lock().unwrap();
        if init_pid.is_some() {
            return Err("eexist");
        }
        if self.task_count() != 0 || self.process_count() != 0 {
            return Err("ebusy");
        }

        let slot = self.reserve_task_slot()?;
        let (process, task) = self.make_new_process(INIT_PID)?;
        if self
            .seq
            .compare_exchange(INIT_PID, INIT_PID + 1, Ordering::SeqCst, Ordering::SeqCst)
            .is_err()
        {
            return Err("ebusy");
        }

        if let Err(err) = self.register_process(&process, &task, None) {
            // No task has been published and the boot-time creator is serial, so
            // restore pid 1 for a later attempt if an invariant check rejects it.
            let rolled_back = self
                .seq
                .compare_exchange(INIT_PID + 1, INIT_PID, Ordering::SeqCst, Ordering::SeqCst)
                .is_ok();
            debug_assert!(rolled_back, "init pid rollback raced with task creation");
            return Err(err);
        }
        *init_pid = Some(INIT_PID);
        slot.commit();
        Ok(task)
    }

    // AGENT: group process and thread derivation from an existing caller.

    // AGENT: preserve the direct semantic helper by snapshotting an off-CPU
    // source task before delegating to the live-frame-aware fork path.
    pub fn fork_process(&self, src: &Arc<Task>) -> Result<Arc<Task>, &'static str> {
        let caller_frame = src.snapshot_user_trap_frame()?;
        self.fork_process_from_frame(src, &caller_frame)
    }

    // AGENT: own only the global fork transaction: capacity, identity,
    // registration, job control, and family publication.
    pub fn fork_process_from_frame(
        &self,
        src: &Arc<Task>,
        caller_frame: &TrapFrame,
    ) -> Result<Arc<Task>, &'static str> {
        let task_slot = self.reserve_task_slot()?;
        let parent_process = src.process.clone();
        if self
            .find_process(parent_process.pid())
            .is_none_or(|registered| !Arc::ptr_eq(&registered, &parent_process))
        {
            return Err("esrch");
        }

        let child_id = self.seq.fetch_add(1, Ordering::SeqCst);
        let child_process = Process::fork_from(&parent_process, child_id, &self.pool)?;
        let child = Task::make_child_from_frame(
            child_id,
            child_process.clone(),
            src,
            caller_frame.clone(),
            &self.pool,
        )?;
        self.register_process(&child_process, &child, Some(parent_process.pid()))?;
        Self::attach_child(&parent_process, &child_process);
        task_slot.commit();
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

    // AGENT: clone one Task while sharing the caller's owning Process exactly.
    pub fn clone_thread_from_frame(
        &self,
        src: &Arc<Task>,
        caller_frame: &TrapFrame,
        stack_top: u64,
        tls: u64,
    ) -> Result<Arc<Task>, &'static str> {
        let task_slot = self.reserve_task_slot()?;
        let process = self.process_of_tid(src.id()).ok_or("esrch")?;
        let id = self.seq.fetch_add(1, Ordering::SeqCst);
        let mut child_frame = caller_frame.clone();
        child_frame.regs[2] = stack_top as usize;
        // AGENT TODO: wire the clone flags through the syscall ABI and replace
        // x4/tp only when CLONE_SETTLS requests a new user TLS pointer.
        child_frame.regs[4] = tls as usize;
        let task = Task::make_child_from_frame(id, process.clone(), src, child_frame, &self.pool)?;

        let mut tasks = self.tasks.write().unwrap();
        if tasks.contains_key(&id) || !process.add_thread(id) {
            return Err("eexist");
        }
        tasks.insert(id, task.clone());
        drop(tasks);
        task_slot.commit();
        Ok(task)
    }

    // AGENT: group read-only task/process lookup, counts, and lifecycle snapshots.

    // AGENT: look up one schedulable thread id without treating it as a pid.
    pub fn find_task(&self, tid: usize) -> Option<Arc<Task>> {
        self.tasks.read().unwrap().get(&tid).cloned()
    }

    // AGENT: look up one process directly by pid without resolving a leader Task.
    pub fn find_process(&self, pid: usize) -> Option<Arc<Process>> {
        self.processes.read().unwrap().get(&pid).cloned()
    }

    // AGENT: resolve a thread id directly to its owning first-class Process.
    pub fn process_of_tid(&self, tid: usize) -> Option<Arc<Process>> {
        self.find_task(tid).map(|task| task.process.clone())
    }

    // AGENT: resolve init through the authoritative process index so this role
    // marker cannot keep a removed Process allocation alive by itself.
    pub fn init_process(&self) -> Option<Arc<Process>> {
        let pid = *self.init_pid.lock().unwrap();
        pid.and_then(|pid| self.find_process(pid))
    }

    // AGENT: report the number of registered schedulable thread ids.
    pub fn task_count(&self) -> usize {
        self.tasks.read().unwrap().len()
    }

    // AGENT: report the number of independently registered processes.
    pub fn process_count(&self) -> usize {
        self.processes.read().unwrap().len()
    }

    // AGENT: snapshot active Process objects once each for process-directed signals.
    pub fn active_processes(&self) -> Vec<Arc<Process>> {
        self.processes
            .read()
            .unwrap()
            .values()
            .filter(|process| !process.is_terminating() && !process.is_zombie())
            .cloned()
            .collect()
    }

    // AGENT: snapshot zombie Process objects once each for one-time reaping.
    pub fn zombie_processes(&self) -> Vec<Arc<Process>> {
        self.processes
            .read()
            .unwrap()
            .values()
            .filter(|process| process.is_zombie())
            .cloned()
            .collect()
    }

    // AGENT: group bidirectional process-family linkage and orphan adoption.

    // AGENT: update both sides of one process-family relation through Process
    // handles so no schedulable Task becomes the family-identity authority.
    fn attach_child(parent: &Arc<Process>, child: &Arc<Process>) {
        child.set_parent(Some(parent));
        parent.insert_child(child.clone());
    }

    // AGENT: require the registered init process for ordinary orphan adoption,
    // publish adopted-zombie readiness after both family links are installed,
    // and return those pids so Kernel can deliver the corresponding SIGCHLD.
    pub fn reparent_children_to_init(&self, process: &Arc<Process>) -> Vec<usize> {
        let init_process = self
            .init_process()
            .expect("ordinary process exit requires a registered init process");
        assert!(
            !Arc::ptr_eq(&init_process, process),
            "init exit must be handled before orphan reparenting"
        );

        let children = process.take_children();
        let mut adopted_zombie_pids = Vec::new();
        for child in children {
            if child.is_zombie() {
                adopted_zombie_pids.push(child.pid());
            }
            Self::attach_child(&init_process, &child);
        }
        if !adopted_zombie_pids.is_empty() {
            init_process.ev.lock().unwrap().set(EvFlag::CHILD_QUIT);
        }
        adopted_zombie_pids
    }

    // AGENT: group authoritative process-group and session transitions and queries.

    // AGENT: move a process between process groups as one state transition.
    pub fn move_process_to_group(
        &self,
        process: &Arc<Process>,
        new_pgid: i32,
    ) -> Result<(), &'static str> {
        if new_pgid <= 0 {
            return Err("einval");
        }
        self.job_control
            .lock()
            .unwrap()
            .move_process(process.pid(), new_pgid)
    }

    // AGENT: make a process a session leader and sole member of its new group.
    pub fn start_new_session(&self, process: &Arc<Process>) -> Result<usize, &'static str> {
        let pid = process.pid();
        self.job_control.lock().unwrap().start_new_session(pid)?;
        Ok(pid)
    }

    // AGENT: resolve authoritative process-group membership to Process objects.
    pub fn pgid_group(&self, pgid: i32) -> Vec<Arc<Process>> {
        let members = self.job_control.lock().unwrap().members(pgid);
        let processes = self.processes.read().unwrap();
        members
            .into_iter()
            .filter_map(|pid| processes.get(&pid).cloned())
            .collect()
    }

    // AGENT: expose the authoritative process-group id without mirrored state.
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

    // AGENT: group thread removal and zombie-process reaping after exit.

    // AGENT: delete one non-last exited thread immediately without touching its
    // still-running Process or any sibling task-table entries.
    pub(crate) fn remove_exited_thread(&self, tid: Tid, process: &Arc<Process>) -> bool {
        let mut tasks = self.tasks.write().unwrap();
        let removed = if tasks
            .get(&tid)
            .is_some_and(|task| Arc::ptr_eq(&task.process, process))
        {
            tasks.remove(&tid);
            true
        } else {
            false
        };
        drop(tasks);
        if removed {
            self.release_task_slots(1);
        }
        removed
    }

    // AGENT: reap only a zombie whose exit path already detached every child,
    // then remove it from family, group, process, and thread indexes.
    pub fn reap(&self, pid: usize) -> Result<(), &'static str> {
        let process = self.find_process(pid).ok_or("esrch")?;
        if !process.is_zombie() || !process.has_no_children() {
            return Err("ebusy");
        }

        if let Some(parent) = process.parent() {
            parent.remove_child(pid);
        }
        process.set_parent(None);
        self.job_control.lock().unwrap().remove_process(pid);

        let thread_ids = process.take_threads();
        let mut tasks = self.tasks.write().unwrap();
        let mut removed_tasks = 0;
        for tid in thread_ids {
            if tasks
                .get(&tid)
                .is_some_and(|thread| Arc::ptr_eq(&thread.process, &process))
            {
                tasks.remove(&tid);
                removed_tasks += 1;
            }
        }
        drop(tasks);
        self.release_task_slots(removed_tasks);

        let mut processes = self.processes.write().unwrap();
        if processes
            .get(&pid)
            .is_some_and(|registered| Arc::ptr_eq(registered, &process))
        {
            processes.remove(&pid);
        }
        Ok(())
    }
}

// AGENT: release a provisional task-table reservation on every early return.
struct TaskSlotReservation<'a> {
    table: &'a TaskTable,
    active: bool,
}

// AGENT: distinguish successful publication from automatic failed-construction release.
impl TaskSlotReservation<'_> {
    // AGENT: consume a reservation into a registered task without reopening capacity.
    fn commit(mut self) {
        self.active = false;
    }

    // AGENT: return an uncommitted slot exactly once on an error path.
    fn release_inner(&mut self) {
        if self.active {
            self.active = false;
            self.table.release_task_slots(1);
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
