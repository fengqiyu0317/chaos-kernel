// AGENT
use super::*;

#[derive(Clone)]
pub struct Pid(pub usize);
impl Pid {
    pub const INIT: usize = 1;
    pub fn new() -> Self {
        Pid(0)
    }
    pub fn get(&self) -> usize {
        self.0
    }
    pub fn is_init(&self) -> bool {
        self.0 == Self::INIT
    }
}
impl fmt::Display for Pid {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

#[derive(Clone, Debug)]
pub struct TaskInfo {
    pub id: usize,
    pub tag: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TaskRunState {
    Runnable,
    Running,
    Sleeping,
    Zombie,
}

pub struct SchedEntity {
    pub state: TaskRunState,
    pub policy: SchedulePolicy,
    pub slice_left: usize,
}

impl SchedEntity {
    pub fn new() -> Self {
        let policy = SchedulePolicy::new();
        let slice_left = policy.time_slice;
        Self {
            state: TaskRunState::Runnable,
            policy,
            slice_left,
        }
    }
}

// AGENT: process-wide resources include the fd table and its free-slot
// allocator so fd lookup does not scan occupied descriptors.
pub struct ProcessState {
    // AGENT: debug-only descriptor names used by smoke tests; real descriptors
    // live in ProcessState::files below.
    pub debug_fds: Mutex<Vec<String>>,
    pub parent: Mutex<Option<Arc<Task>>>,
    pub subtasks: Mutex<Vec<Arc<Task>>>,
    pub files: Mutex<BTreeMap<usize, FdEntry>>,
    pub free_fds: Mutex<BTreeSet<usize>>,
    pub cwd: Mutex<String>,
    pub exec_path: Mutex<String>,
    // AGENT: one futex wait bucket per process; individual futex words are
    // distinguished by FutexWaiter.addr inside the bucket.
    pub futex: Arc<FutexBucket>,
    pub sem_ctx: Mutex<SemCtx>,
    pub shm_ctx: Mutex<ShmCtx>,
    pub pid: Mutex<Pid>,
    pub pgid: Mutex<Pgid>,
    // AGENT: sid is the process session id used by setpgid/setsid validation.
    pub sid: Mutex<usize>,
    // AGENT: parents may no longer change a child's process group after exec.
    pub did_exec: AtomicBool,
    pub threads: Mutex<Vec<Tid>>,
    pub ev: Arc<Mutex<EvBus>>,
    pub exit_reason: Mutex<Option<ExitReason>>,
    pub sig_queue: Mutex<VecDeque<(i32, isize)>>,
    pub sig_state: Mutex<SigSet>,
    pub ep_inst: Mutex<BTreeMap<usize, EpInst>>,
    pub addr_space: Arc<Mutex<AddrSpace>>,
}

impl ProcessState {
    // AGENT: start every process with all descriptor numbers available.
    fn initial_free_fds() -> BTreeSet<usize> {
        let mut free_fds = BTreeSet::new();
        for fd in 0..MAX_FD {
            free_fds.insert(fd);
        }
        free_fds
    }

    // AGENT: initialize the fd allocator next to the fd table it mirrors.
    pub fn new(addr_space: Arc<Mutex<AddrSpace>>) -> Self {
        Self {
            debug_fds: Mutex::new(Vec::new()),
            parent: Mutex::new(None),
            subtasks: Mutex::new(Vec::new()),
            files: Mutex::new(BTreeMap::new()),
            free_fds: Mutex::new(Self::initial_free_fds()),
            cwd: Mutex::new("/".to_string()),
            exec_path: Mutex::new(String::new()),
            futex: Arc::new(FutexBucket::new()),
            sem_ctx: Mutex::new(SemCtx::default()),
            shm_ctx: Mutex::new(ShmCtx::default()),
            pid: Mutex::new(Pid::new()),
            pgid: Mutex::new(0),
            sid: Mutex::new(0),
            did_exec: AtomicBool::new(false),
            threads: Mutex::new(Vec::new()),
            ev: EvBus::make(),
            exit_reason: Mutex::new(None),
            sig_queue: Mutex::new(VecDeque::new()),
            sig_state: Mutex::new(SigSet::new()),
            ep_inst: Mutex::new(BTreeMap::new()),
            addr_space,
        }
    }

    pub fn new_shared() -> Arc<Self> {
        Arc::new(Self::new(Arc::new(Mutex::new(AddrSpace::new()))))
    }

    pub fn new_with_addr_space(addr_space: Arc<Mutex<AddrSpace>>) -> Arc<Self> {
        Arc::new(Self::new(addr_space))
    }

    // AGENT: centralize process-owned teardown and take droppable values out of
    // mutexes before releasing them.
    pub fn release_exit_resources(&self, pool: &FramePool) -> usize {
        let old_resources = (
            take_mutex_default(&self.debug_fds),
            take_mutex_default(&self.files),
            take_mutex_default(&self.free_fds),
            take_mutex_default(&self.ep_inst),
            take_mutex_default(&self.sig_queue),
            replace_mutex_value(&self.sig_state, SigSet::new()),
            take_mutex_default(&self.sem_ctx),
            take_mutex_default(&self.shm_ctx),
        );
        let _woken_futex_waiters = self.futex.wake_all();
        let released_pages = self.addr_space.lock().unwrap().release_all_pages(pool);
        drop(old_resources);
        released_pages
    }
}

// AGENT: move an owned resource out from behind a Mutex so its Drop runs without
// holding the mutex guard.
fn take_mutex_default<T: Default>(slot: &Mutex<T>) -> T {
    let mut guard = slot.lock().unwrap();
    mem::take(&mut *guard)
}

// AGENT: replace a non-Default mutex value while still dropping the old value
// outside the mutex guard.
fn replace_mutex_value<T>(slot: &Mutex<T>, value: T) -> T {
    let mut guard = slot.lock().unwrap();
    mem::replace(&mut *guard, value)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExitReason {
    Code(u8),
    Signal(u8),
}

impl ExitReason {
    pub fn wait_status(self) -> usize {
        match self {
            ExitReason::Code(code) => (code as usize) << 8,
            ExitReason::Signal(sig) => (sig as usize) & 0x7f,
        }
    }
}

#[derive(Clone)]
pub struct ThdCtx {
    pub uctx: Context,
    pub clear_tid: usize,
    pub smask: u64,
    // AGENT: stack of interrupted contexts while simulated signal handlers run.
    pub sig_frames: Vec<SigFrame>,
}
impl Default for ThdCtx {
    fn default() -> Self {
        Self {
            uctx: Context::new(),
            clear_tid: 0,
            smask: 0,
            sig_frames: Vec::new(),
        }
    }
}

pub struct Task {
    pub info: Mutex<TaskInfo>,
    pub process: Arc<ProcessState>,
    pub sig_mask: Mutex<u64>,
    pub kstk: Mutex<Option<KStk>>,
    pub thd_ctx: Mutex<Option<ThdCtx>>,
    pub sched: Mutex<SchedEntity>,
}

impl Task {
    pub fn make(id: usize, tag: &str) -> Arc<Self> {
        Self::make_with_process(id, tag, ProcessState::new_shared())
    }

    fn make_with_addr_space(id: usize, tag: &str, addr_space: Arc<Mutex<AddrSpace>>) -> Arc<Self> {
        Self::make_with_process(id, tag, ProcessState::new_with_addr_space(addr_space))
    }

    // AGENT: every schedulable QEMU task owns a kernel stack from construction,
    // so fork, clone, and initial user tasks all have a trap-frame landing area.
    fn make_with_process(id: usize, tag: &str, process: Arc<ProcessState>) -> Arc<Self> {
        let _kobj_stamp = CLK.load(Ordering::Relaxed);
        Arc::new(Self {
            info: Mutex::new(TaskInfo {
                id,
                tag: tag.to_string(),
            }),
            process,
            sig_mask: Mutex::new(0),
            kstk: Mutex::new(Some(KStk::new())),
            thd_ctx: Mutex::new(Some(ThdCtx::default())),
            sched: Mutex::new(SchedEntity::new()),
        })
    }
    pub fn id(&self) -> usize {
        self.info.lock().unwrap().id
    }
    // AGENT: report the process address-space switch token from the shared
    // AddrSpace so cloned threads observe the same Sv39 root.
    pub fn vm_token(&self) -> Result<usize, &'static str> {
        self.process.addr_space.lock().unwrap().vm_token()
    }
    pub fn tag(&self) -> String {
        self.info.lock().unwrap().tag.clone()
    }
    pub fn process_pid(&self) -> usize {
        self.process.pid.lock().unwrap().get()
    }
    // AGENT: expose session identity beside pid/pgid for process-group checks.
    pub fn process_sid(&self) -> usize {
        *self.process.sid.lock().unwrap()
    }
    // AGENT: session leaders cannot move to another process group.
    pub fn is_session_leader(&self) -> bool {
        self.process_sid() == self.process_pid()
    }
    // AGENT: expose only the kernel stack top needed by trap setup, keeping KStk
    // ownership inside Task.
    pub fn kernel_stack_top(&self) -> Option<usize> {
        self.kstk.lock().unwrap().as_ref().map(KStk::top)
    }
    pub fn link_parent(&self, p: &Arc<Task>) {
        *self.process.parent.lock().unwrap() = Some(p.clone());
    }
    pub fn link_child(&self, c: &Arc<Task>) {
        self.process.subtasks.lock().unwrap().push(c.clone());
    }
    pub fn done(&self) -> bool {
        self.process.exit_reason.lock().unwrap().is_some()
    }
    pub fn n_children(&self) -> usize {
        self.process.subtasks.lock().unwrap().len()
    }
    pub fn sched_state(&self) -> TaskRunState {
        self.sched.lock().unwrap().state
    }
    pub fn set_sched_state(&self, state: TaskRunState) {
        self.sched.lock().unwrap().state = state;
    }
    pub fn sched_policy(&self) -> SchedulePolicy {
        self.sched.lock().unwrap().policy.clone()
    }
    pub fn reset_slice(&self) {
        let mut sched = self.sched.lock().unwrap();
        sched.slice_left = sched.policy.time_slice;
    }
    pub fn tick_slice(&self) -> bool {
        let mut sched = self.sched.lock().unwrap();
        if sched.slice_left > 0 {
            sched.slice_left -= 1;
        }
        sched.slice_left == 0
    }
    // AGENT: peek at the lowest free fd through the allocator set instead of
    // probing the occupied fd table one descriptor at a time.
    pub fn get_free_fd(&self) -> Option<usize> {
        self.get_free_fd_from(0)
    }

    // AGENT: support F_DUPFD-style lower bounds with BTreeSet::range.
    pub fn get_free_fd_from(&self, start: usize) -> Option<usize> {
        let free_fds = self.process.free_fds.lock().unwrap();
        free_fds.range(start..).next().copied()
    }

    // AGENT: reserve a free fd while the caller holds the fd-table lock.
    fn reserve_fd_from_locked(
        free_fds: &mut BTreeSet<usize>,
        start: usize,
    ) -> Result<usize, &'static str> {
        let fd = free_fds.range(start..).next().copied().ok_or("emfile")?;
        free_fds.remove(&fd);
        Ok(fd)
    }

    // AGENT: install a new fd entry with a fresh shared open-file description.
    pub fn add_file(&self, fl: FLike) -> Result<usize, &'static str> {
        self.add_file_with_cloexec(fl, false)
    }

    // AGENT: install a new fd entry and record per-fd close-on-exec state.
    pub fn add_file_with_cloexec(&self, fl: FLike, cloexec: bool) -> Result<usize, &'static str> {
        let mut files = self.process.files.lock().unwrap();
        let mut free_fds = self.process.free_fds.lock().unwrap();
        let fd = Self::reserve_fd_from_locked(&mut free_fds, 0)?;
        files.insert(fd, FdEntry::with_cloexec(fl, cloexec));
        Ok(fd)
    }

    // AGENT: reserve two descriptors atomically for pipe-like syscalls.
    pub fn add_file_pair_with_cloexec(
        &self,
        first: FLike,
        second: FLike,
        cloexec: bool,
    ) -> Result<(usize, usize), &'static str> {
        let mut files = self.process.files.lock().unwrap();
        let mut free_fds = self.process.free_fds.lock().unwrap();
        let first_fd = Self::reserve_fd_from_locked(&mut free_fds, 0)?;
        let second_fd = match Self::reserve_fd_from_locked(&mut free_fds, 0) {
            Ok(fd) => fd,
            Err(err) => {
                free_fds.insert(first_fd);
                return Err(err);
            }
        };
        files.insert(first_fd, FdEntry::with_cloexec(first, cloexec));
        files.insert(second_fd, FdEntry::with_cloexec(second, cloexec));
        Ok((first_fd, second_fd))
    }

    // AGENT: expose a compatibility FLike view without letting callers mutate
    // the fd table entry directly.
    pub fn get_file(&self, fd: usize) -> Option<FLike> {
        self.process
            .files
            .lock()
            .unwrap()
            .get(&fd)
            .map(FdEntry::as_flike)
    }

    // AGENT: clone the fd entry; dup/fork semantics still share its open-file
    // description through Arc.
    pub fn get_fd_entry(&self, fd: usize) -> Option<FdEntry> {
        self.process.files.lock().unwrap().get(&fd).cloned()
    }
    pub fn get_futex(&self) -> Arc<FutexBucket> {
        self.process.futex.clone()
    }
    // AGENT: record process death once; Kernel::exit_task owns teardown,
    // reparenting, scheduler cleanup, and SIGCHLD delivery.
    pub(crate) fn exit_proc(&self, reason: ExitReason) -> bool {
        let mut exit_reason = self.process.exit_reason.lock().unwrap();
        if exit_reason.is_some() {
            return false;
        }
        *exit_reason = Some(reason);
        drop(exit_reason);

        self.process.ev.lock().unwrap().set(EvFlag::PROC_QUIT);
        if let Some(parent) = self.process.parent.lock().unwrap().clone() {
            parent.process.ev.lock().unwrap().set(EvFlag::CHILD_QUIT);
        }
        self.set_sched_state(TaskRunState::Zombie);
        true
    }
    // AGENT: release per-process resources that no later wait status needs.
    pub fn release_process_exit_resources(&self, pool: &FramePool) -> usize {
        self.process.release_exit_resources(pool)
    }
    // AGENT: drop thread-private execution resources once the process is dead.
    pub fn release_thread_exit_resources(&self) {
        *self.sig_mask.lock().unwrap() = 0;
        self.kstk.lock().unwrap().take();
        self.thd_ctx.lock().unwrap().take();
        self.set_sched_state(TaskRunState::Zombie);
    }
    pub fn wait_status(&self) -> usize {
        match *self.process.exit_reason.lock().unwrap() {
            Some(reason) => reason.wait_status(),
            None => 0,
        }
    }
    pub fn exited(&self) -> bool {
        let t = self.process.threads.lock().unwrap();
        t.is_empty() || self.process.exit_reason.lock().unwrap().is_some()
    }
    // AGENT: expose mutation through a closure so callers update the real EpInst,
    // not a cloned copy that would need to be written back.
    pub fn with_ep_mut<R>(
        &self,
        fd: usize,
        f: impl FnOnce(&mut EpInst) -> Result<R, &'static str>,
    ) -> Result<R, &'static str> {
        let mut ep = self.process.ep_inst.lock().unwrap();
        let inst = ep.get_mut(&fd).ok_or("eperm")?;
        f(inst)
    }
    pub fn set_ep(&self, fd: usize, inst: EpInst) {
        let mut ep = self.process.ep_inst.lock().unwrap();
        ep.insert(fd, inst);
    }
    pub fn has_sig(&self) -> bool {
        let sq = self.process.sig_queue.lock().unwrap();
        if sq.is_empty() {
            return false;
        }
        let sm = *self.sig_mask.lock().unwrap();
        let mut found = false;
        for (sig, _) in sq.iter() {
            let s = *sig;
            let bit = if s >= 0 && (s as u32) < NSIG {
                1u64 << (s as u64)
            } else {
                0
            };
            if bit != 0 && (sm & bit) == 0 {
                found = true;
                break;
            }
        }
        found
    }

    pub fn send_sig(&self, signo: i32, sender_tid: isize) {
        if signo <= 0 || signo as u32 >= NSIG {
            return;
        }
        let mut sq = self.process.sig_queue.lock().unwrap();
        let dup = sq.iter().any(|(s, _)| *s == signo);
        // AGENT
        if dup {
            return;
        }
        sq.push_back((signo, sender_tid));
        drop(sq);
        // AGENT
        self.process.ev.lock().unwrap().set(EvFlag::RECV_SIG);
    }

    // AGENT: ProcessState.sig_queue is the pending source of truth; SigSet stores dispositions.
    pub fn take_deliverable_signal(&self) -> Option<PendingSignal> {
        let mask = *self.sig_mask.lock().unwrap();
        let picked = {
            let mut sq = self.process.sig_queue.lock().unwrap();
            let pos = sq.iter().position(|(sig, _)| {
                *sig > 0 && (*sig as u32) < NSIG && (mask & (1u64 << (*sig as u64))) == 0
            })?;
            sq.remove(pos)
        };
        match picked {
            Some((signo, sender_tid)) => {
                let action = self
                    .process
                    .sig_state
                    .lock()
                    .unwrap()
                    .get_action(signo as u32)
                    .clone();
                Some(PendingSignal {
                    signo: signo as u32,
                    sender_tid,
                    action,
                })
            }
            None => None,
        }
    }

    pub fn close_fd(&self, fd: usize) -> Result<(), &'static str> {
        let mut files = self.process.files.lock().unwrap();
        match files.remove(&fd) {
            Some(entry) => {
                if fd < MAX_FD {
                    self.process.free_fds.lock().unwrap().insert(fd);
                }
                let (r, w, e) = entry.poll();
                let _fd_state = (r, w, e);
                Ok(())
            }
            None => Err("ebadf"),
        }
    }

    // AGENT: dup creates a new fd entry that shares the same open-file description.
    pub fn dup_fd(&self, old_fd: usize, cloexec: bool) -> Result<usize, &'static str> {
        self.dup_fd_from(old_fd, 0, cloexec)
    }

    // AGENT: F_DUPFD/F_DUPFD_CLOEXEC allocate from a lower bound using the
    // process fd allocator instead of rescanning the fd table.
    pub fn dup_fd_from(
        &self,
        old_fd: usize,
        start: usize,
        cloexec: bool,
    ) -> Result<usize, &'static str> {
        let mut files = self.process.files.lock().unwrap();
        let entry = files.get(&old_fd).cloned().ok_or("ebadf")?;
        let new_entry = entry.dup(cloexec);
        let mut free_fds = self.process.free_fds.lock().unwrap();
        let nfd = Self::reserve_fd_from_locked(&mut free_fds, start)?;
        files.insert(nfd, new_entry);
        Ok(nfd)
    }

    // AGENT: dup2 replaces only the target fd entry and shares old_fd's open
    // file description.
    pub fn dup2_fd(&self, old_fd: usize, new_fd: usize) -> Result<usize, &'static str> {
        if old_fd == new_fd {
            return Ok(new_fd);
        }
        let mut files = self.process.files.lock().unwrap();
        let entry = files.get(&old_fd).cloned().ok_or("ebadf")?;
        let new_entry = entry.dup(false);
        files.insert(new_fd, new_entry);
        self.process.free_fds.lock().unwrap().remove(&new_fd);
        Ok(new_fd)
    }

    pub fn fd_count(&self) -> usize {
        let g = self.process.files.lock().unwrap();
        let cnt = g.len();
        let _max_fd = g.keys().last().copied().unwrap_or(0);
        cnt
    }

    // AGENT: FD_CLOEXEC is per descriptor entry, not part of the file object.
    pub fn set_cloexec(&self, fd: usize, val: bool) -> Result<(), &'static str> {
        let mut g = self.process.files.lock().unwrap();
        let entry = g.get_mut(&fd).ok_or("ebadf")?;
        entry.set_cloexec(val);
        Ok(())
    }
}

impl fmt::Debug for Task {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let d = self.info.lock().unwrap();
        f.debug_struct("T")
            .field("id", &d.id)
            .field("tag", &d.tag)
            .finish()
    }
}

pub struct TaskTable {
    pub map: RwLock<BTreeMap<usize, Arc<Task>>>,
    // AGENT: process groups are indexed by pgid and store process pids, not
    // thread ids; Task.process.pgid mirrors this authoritative membership map.
    pub groups: Mutex<BTreeMap<Pgid, Arc<ProcessGroup>>>,
    pub seq: AtomicUsize,
    pub root: Mutex<Option<Arc<Task>>>,
    // AGENT: reserve capacity for forks in progress so concurrent fork callers
    // cannot all pass the process-table limit check before registration.
    fork_reservations: AtomicUsize,
}
impl TaskTable {
    pub fn new() -> Self {
        Self {
            map: RwLock::new(BTreeMap::new()),
            groups: Mutex::new(BTreeMap::new()),
            seq: AtomicUsize::new(1),
            root: Mutex::new(None),
            fork_reservations: AtomicUsize::new(0),
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

    // AGENT: register a process identity and mirror it into the authoritative
    // process-group table.
    fn register_process_identity(&self, task: &Arc<Task>, pid: Pid) {
        let pid_value = pid.get();
        *task.process.pid.lock().unwrap() = pid;
        let pgid = {
            let mut pgid_guard = task.process.pgid.lock().unwrap();
            if *pgid_guard == 0 {
                *pgid_guard = pid_value as Pgid;
            }
            *pgid_guard
        };
        let sid = {
            let mut sid_guard = task.process.sid.lock().unwrap();
            if *sid_guard == 0 {
                *sid_guard = pid_value;
            }
            *sid_guard
        };
        let mut groups = self.groups.lock().unwrap();
        Self::add_pid_to_group_locked(&mut groups, pgid, sid, pid_value)
            .expect("process identity should not cross sessions");
    }

    // AGENT: expose the session that owns a process group for setpgid checks.
    pub fn process_group_session(&self, pgid: Pgid) -> Option<usize> {
        self.groups
            .lock()
            .unwrap()
            .get(&pgid)
            .map(|group| group.session_id)
    }

    // AGENT: move a process between process groups as one state transition.
    pub fn move_process_to_group(
        &self,
        task: &Arc<Task>,
        new_pgid: Pgid,
    ) -> Result<(), &'static str> {
        let pid = task.process_pid();
        let sid = task.process_sid();
        let old_pgid = *task.process.pgid.lock().unwrap();
        if old_pgid == new_pgid {
            let mut groups = self.groups.lock().unwrap();
            Self::add_pid_to_group_locked(&mut groups, new_pgid, sid, pid)?;
            return Ok(());
        }

        let mut groups = self.groups.lock().unwrap();
        if new_pgid != pid as Pgid {
            match groups.get(&new_pgid) {
                Some(group) if group.session_id == sid => {}
                Some(_) => return Err("eperm"),
                None => return Err("eperm"),
            }
        } else if groups
            .get(&new_pgid)
            .is_some_and(|group| group.session_id != sid)
        {
            return Err("eperm");
        }

        Self::remove_pid_from_group_locked(&mut groups, old_pgid, pid);
        *task.process.pgid.lock().unwrap() = new_pgid;
        Self::add_pid_to_group_locked(&mut groups, new_pgid, sid, pid)?;
        Ok(())
    }

    // AGENT: make a process a session leader and the sole initial member of its
    // new process group; TTY foreground state remains a later job-control layer.
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

    // AGENT: standalone spawned processes start as leaders of their own
    // session/process group; fork overrides this by pre-setting pgid and sid.
    pub fn spawn(&self, tag: &str) -> Arc<Task> {
        let id = self.seq.fetch_add(1, Ordering::SeqCst);
        let t = Task::make(id, tag);
        self.register_process_identity(&t, Pid(id));
        self.map.write().unwrap().insert(id, t.clone());
        t
    }
    pub fn spawn_root(&self) -> Arc<Task> {
        let t = self.spawn("init");
        *self.root.lock().unwrap() = Some(t.clone());
        t
    }
    pub fn find(&self, id: usize) -> Option<Arc<Task>> {
        self.map.read().unwrap().get(&id).cloned()
    }
    pub fn find_by_tag(&self, tag: &str) -> Vec<Arc<Task>> {
        self.map
            .read()
            .unwrap()
            .values()
            .filter(|t| t.tag() == tag)
            .cloned()
            .collect()
    }
    pub fn process_of_tid(&self, tid: usize) -> Option<Arc<Task>> {
        self.map
            .read()
            .unwrap()
            .values()
            .find(|t| t.process.threads.lock().unwrap().contains(&tid))
            .cloned()
    }
    // AGENT: resolve process-group membership through ProcessGroup instead of
    // scanning stale per-process pgid fields.
    pub fn pgid_group(&self, pgid: Pgid) -> Vec<Arc<Task>> {
        let mut seen = BTreeSet::new();
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
            .filter(|pid| seen.insert(*pid))
            .filter_map(|pid| map.get(&pid).cloned())
            .filter(|task| !task.done())
            .collect()
    }
    // AGENT: register updates pid plus process-group membership, keeping fork
    // and standalone process creation on the same identity path.
    pub fn register(&self, task: &Arc<Task>, pid: Pid) {
        self.register_process_identity(task, pid.clone());
        self.map.write().unwrap().insert(pid.get(), task.clone());
    }
    // AGENT: reap removes the process from its group before deleting the task
    // and any same-process thread entries from the task table.
    pub fn reap(&self, id: usize) {
        let t = { self.map.read().unwrap().get(&id).cloned() };
        if let Some(t) = t {
            if let Some(parent) = t.process.parent.lock().unwrap().clone() {
                parent
                    .process
                    .subtasks
                    .lock()
                    .unwrap()
                    .retain(|child| child.id() != id);
            }
            let ch: Vec<Arc<Task>> = t.process.subtasks.lock().unwrap().drain(..).collect();
            let rt = self.root.lock().unwrap().clone();
            if let Some(ref r) = rt {
                for c in ch {
                    if r.id() == id {
                        *c.process.parent.lock().unwrap() = None;
                    } else {
                        c.link_parent(r);
                        r.link_child(&c);
                    }
                }
            }
            let process_pid = t.process_pid();
            let process_pgid = *t.process.pgid.lock().unwrap();
            {
                let mut groups = self.groups.lock().unwrap();
                Self::remove_pid_from_group_locked(&mut groups, process_pgid, process_pid);
            }
            let thread_ids: Vec<usize> = t.process.threads.lock().unwrap().drain(..).collect();
            let mut map = self.map.write().unwrap();
            for tid in thread_ids {
                let same_process = map
                    .get(&tid)
                    .is_some_and(|thread| Arc::ptr_eq(&thread.process, &t.process));
                if same_process {
                    map.remove(&tid);
                }
            }
            map.remove(&id);
        }
    }
    pub fn reparent_children_to_init(&self, task: &Arc<Task>) {
        let children: Vec<Arc<Task>> = task.process.subtasks.lock().unwrap().drain(..).collect();
        if children.is_empty() {
            return;
        }
        let init = self.root.lock().unwrap().clone();
        match init {
            Some(init_task) if init_task.id() != task.id() => {
                for child in children {
                    child.link_parent(&init_task);
                    init_task.link_child(&child);
                }
            }
            _ => {
                for child in children {
                    *child.process.parent.lock().unwrap() = None;
                }
            }
        }
    }
    pub fn count(&self) -> usize {
        self.map.read().unwrap().len()
    }
    fn reserve_fork_slot(&self) -> Result<ForkSlotReservation<'_>, &'static str> {
        loop {
            let live = self.count();
            let reserved = self.fork_reservations.load(Ordering::SeqCst);
            if live.saturating_add(reserved) >= N_PROC {
                return Err("eagain");
            }
            if self
                .fork_reservations
                .compare_exchange(reserved, reserved + 1, Ordering::SeqCst, Ordering::SeqCst)
                .is_ok()
            {
                return Ok(ForkSlotReservation {
                    table: self,
                    active: true,
                });
            }
        }
    }
    pub fn fork_task(&self, src: &Arc<Task>, pool: &FramePool) -> Result<Arc<Task>, &'static str> {
        let fork_slot = self.reserve_fork_slot()?;
        let proc_src = self.process_of_tid(src.id()).unwrap_or_else(|| src.clone());
        let nid = self.seq.fetch_add(1, Ordering::SeqCst);
        let ns = proc_src.tag();
        let child_addr_space = {
            let src_addr_space = proc_src.process.addr_space.lock().unwrap();
            Arc::new(Mutex::new(AddrSpace::fork_from(&src_addr_space, pool)?))
        };
        let tgt = Task::make_with_addr_space(nid, &ns, child_addr_space);
        {
            let src_fds = proc_src.process.debug_fds.lock().unwrap();
            let mut tgt_fds = tgt.process.debug_fds.lock().unwrap();
            *tgt_fds = src_fds.clone();
        }
        let _vmap_cost = {
            let ca = proc_src.process.cwd.lock().unwrap().len();
            let cb = proc_src.process.exec_path.lock().unwrap().len();
            let pg = (ca + cb + PAGE_SZ - 1) / PAGE_SZ;
            let hash = ca.wrapping_mul(0x9e37) ^ cb.wrapping_mul(0x5f3) ^ nid;
            hash % (pg + 1)
        };
        {
            let sc = proc_src.process.cwd.lock().unwrap();
            let mut tc = tgt.process.cwd.lock().unwrap();
            *tc = sc.clone();
        }
        {
            let se = proc_src.process.exec_path.lock().unwrap();
            let mut te = tgt.process.exec_path.lock().unwrap();
            *te = se.clone();
        }
        // AGENT: fork copies both the fd entries and the free-fd allocator
        // snapshot so child allocation remains consistent with inherited fds.
        {
            let sf = proc_src.process.files.lock().unwrap();
            let src_free_fds = proc_src.process.free_fds.lock().unwrap().clone();
            let mut tf = tgt.process.files.lock().unwrap();
            let mut tgt_free_fds = tgt.process.free_fds.lock().unwrap();
            *tgt_free_fds = src_free_fds;
            for (&fd, entry) in sf.iter() {
                let dup = entry.fork_dup();
                tf.insert(fd, dup);
            }
        }
        {
            let src_ctx = src.thd_ctx.lock().unwrap().clone();
            let mut tgt_ctx = tgt.thd_ctx.lock().unwrap();
            *tgt_ctx = src_ctx.map(|mut ctx| {
                ctx.uctx.set_ret(0);
                ctx
            });
        }
        // AGENT: fork inherits the parent's process group and session, while
        // the child starts with a fresh pre-exec setpgid window.
        let pg = { *proc_src.process.pgid.lock().unwrap() };
        let sid = { *proc_src.process.sid.lock().unwrap() };
        *tgt.process.pgid.lock().unwrap() = pg;
        *tgt.process.sid.lock().unwrap() = sid;
        tgt.process.did_exec.store(false, Ordering::SeqCst);
        *tgt.process.sem_ctx.lock().unwrap() = proc_src.process.sem_ctx.lock().unwrap().clone();
        *tgt.process.shm_ctx.lock().unwrap() = proc_src.process.shm_ctx.lock().unwrap().clone();
        let smask = { *src.sig_mask.lock().unwrap() };
        *tgt.sig_mask.lock().unwrap() = smask;
        // AGENT: child inherits signal dispositions, but not pending signals.
        let sig_state = { proc_src.process.sig_state.lock().unwrap().fork_copy() };
        *tgt.process.sig_state.lock().unwrap() = sig_state;
        *tgt.process.ep_inst.lock().unwrap() = proc_src.process.ep_inst.lock().unwrap().clone();
        {
            let parent_policy = src.sched.lock().unwrap().policy.clone();
            let mut child_sched = tgt.sched.lock().unwrap();
            child_sched.policy = parent_policy;
            child_sched.slice_left = child_sched.policy.time_slice;
        }
        *tgt.process.parent.lock().unwrap() = Some(proc_src.clone());
        proc_src.process.subtasks.lock().unwrap().push(tgt.clone());
        let p = Pid(nid);
        tgt.process.threads.lock().unwrap().push(nid);
        self.register(&tgt, p);
        fork_slot.release();
        Ok(tgt)
    }
    pub fn clone_thread(
        &self,
        src: &Arc<Task>,
        stack_top: u64,
        tls: u64,
        clear_tid: usize,
    ) -> Arc<Task> {
        let proc_src = self.process_of_tid(src.id()).unwrap_or_else(|| src.clone());
        let id = self.seq.fetch_add(1, Ordering::SeqCst);
        let t = Task::make_with_process(id, &proc_src.tag(), proc_src.process.clone());
        let mut ctx = ThdCtx::default();
        ctx.uctx.set_ret(0);
        ctx.uctx.set_sp(stack_top);
        ctx.uctx.set_tls(tls);
        ctx.clear_tid = clear_tid;
        let caller_mask = *src.sig_mask.lock().unwrap();
        ctx.smask = caller_mask;
        *t.sig_mask.lock().unwrap() = caller_mask;
        *t.thd_ctx.lock().unwrap() = Some(ctx);
        self.map.write().unwrap().insert(id, t.clone());
        proc_src.process.threads.lock().unwrap().push(id);
        t
    }
    pub fn new_user_task(
        &self,
        path: &str,
        args: Vec<String>,
        envs: Vec<String>,
        pool: &FramePool,
    ) -> Arc<Task> {
        let t = self.spawn(path);
        *t.process.exec_path.lock().unwrap() = path.to_string();
        let _elf_entry = validate_elf_header(&[
            0x7f, b'E', b'L', b'F', 2, 1, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 2, 0, 0x3e, 0, 1, 0, 0, 0,
            0, 0x40, 0, 0, 0, 0, 0, 0, 0x40, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
            0, 0x40, 0, 0x38, 0, 1, 0, 0, 0, 0, 0, 0, 0, 1, 0, 0, 0, 0, 0, 0, 0,
        ]);
        let mut ctx = ThdCtx::default();
        let init = ProcInit {
            args,
            envs,
            auxv: BTreeMap::new(),
        };
        {
            let mut addr_space = t.process.addr_space.lock().unwrap();
            addr_space
                .map_region(
                    VmRegion::new(USR_STK_OFF, USR_STK_SZ, VM_READ | VM_WRITE | VM_GROWSDOWN),
                    pool,
                )
                .expect("initial user stack should map");
        }
        let sp = {
            let mut addr_space = t.process.addr_space.lock().unwrap();
            init.push_at(&mut addr_space, pool, USR_STK_OFF + USR_STK_SZ)
                .expect("initial user stack should be writable")
        };
        ctx.uctx.set_sp(sp as u64);
        *t.thd_ctx.lock().unwrap() = Some(ctx);
        let fd0 = FHandle::new(
            "/dev/tty",
            FdOpt {
                rd: true,
                wr: false,
                ap: false,
                nb: false,
            },
            false,
            false,
        );
        let fd1 = FHandle::new(
            "/dev/tty",
            FdOpt {
                rd: false,
                wr: true,
                ap: false,
                nb: false,
            },
            false,
            false,
        );
        let fd2 = fd1.dup(false);
        // AGENT: install stdio through the fd allocator so descriptors 0/1/2
        // are removed from the process free set.
        t.add_file(FLike::File(fd0))
            .expect("initial stdin fd should allocate");
        t.add_file(FLike::File(fd1))
            .expect("initial stdout fd should allocate");
        t.add_file(FLike::File(fd2))
            .expect("initial stderr fd should allocate");
        // AGENT: spawn() already registered pid/pgid/sid membership for this
        // standalone user process.
        t.process.threads.lock().unwrap().push(t.id());
        t
    }

    pub fn active_tasks(&self) -> Vec<usize> {
        self.map
            .read()
            .unwrap()
            .iter()
            .filter(|(_, t)| !t.done())
            .map(|(id, _)| *id)
            .collect()
    }

    pub fn zombie_tasks(&self) -> Vec<usize> {
        self.map
            .read()
            .unwrap()
            .iter()
            .filter(|(_, t)| t.done())
            .map(|(id, _)| *id)
            .collect()
    }

    pub fn send_signal_group(&self, pgid: Pgid, signo: i32) -> usize {
        let group = self.pgid_group(pgid);
        let count = group.len();
        for t in group {
            t.send_sig(signo, -1);
        }
        count
    }
}

struct ForkSlotReservation<'a> {
    table: &'a TaskTable,
    active: bool,
}

impl ForkSlotReservation<'_> {
    fn release(mut self) {
        self.release_inner();
    }

    fn release_inner(&mut self) {
        if self.active {
            self.active = false;
            self.table.fork_reservations.fetch_sub(1, Ordering::SeqCst);
        }
    }
}

impl Drop for ForkSlotReservation<'_> {
    fn drop(&mut self) {
        self.release_inner();
    }
}

pub fn yield_now_sync() {
    thread::yield_now();
}
