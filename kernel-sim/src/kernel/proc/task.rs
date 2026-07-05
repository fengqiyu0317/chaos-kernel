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

pub struct ProcessState {
    // AGENT: debug-only descriptor names used by smoke tests; real descriptors
    // live in ProcessState::files below.
    pub debug_fds: Mutex<Vec<String>>,
    pub parent: Mutex<Option<Arc<Task>>>,
    pub subtasks: Mutex<Vec<Arc<Task>>>,
    pub files: Mutex<BTreeMap<usize, FdEntry>>,
    pub cwd: Mutex<String>,
    pub exec_path: Mutex<String>,
    // AGENT: one futex wait bucket per process; individual futex words are
    // distinguished by FutexWaiter.addr inside the bucket.
    pub futex: Arc<FutexBucket>,
    pub sem_ctx: Mutex<SemCtx>,
    pub shm_ctx: Mutex<ShmCtx>,
    pub pid: Mutex<Pid>,
    pub pgid: Mutex<Pgid>,
    pub threads: Mutex<Vec<Tid>>,
    pub ev: Arc<Mutex<EvBus>>,
    pub exit_reason: Mutex<Option<ExitReason>>,
    pub sig_queue: Mutex<VecDeque<(i32, isize)>>,
    pub sig_state: Mutex<SigSet>,
    pub ep_inst: Mutex<BTreeMap<usize, EpInst>>,
    pub addr_space: Arc<Mutex<AddrSpace>>,
}

impl ProcessState {
    pub fn new(addr_space: Arc<Mutex<AddrSpace>>) -> Self {
        Self {
            debug_fds: Mutex::new(Vec::new()),
            parent: Mutex::new(None),
            subtasks: Mutex::new(Vec::new()),
            files: Mutex::new(BTreeMap::new()),
            cwd: Mutex::new("/".to_string()),
            exec_path: Mutex::new(String::new()),
            futex: Arc::new(FutexBucket::new()),
            sem_ctx: Mutex::new(SemCtx::default()),
            shm_ctx: Mutex::new(ShmCtx::default()),
            pid: Mutex::new(Pid::new()),
            pgid: Mutex::new(0),
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
    std::mem::take(&mut *guard)
}

// AGENT: replace a non-Default mutex value while still dropping the old value
// outside the mutex guard.
fn replace_mutex_value<T>(slot: &Mutex<T>, value: T) -> T {
    let mut guard = slot.lock().unwrap();
    std::mem::replace(&mut *guard, value)
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

    fn make_with_process(id: usize, tag: &str, process: Arc<ProcessState>) -> Arc<Self> {
        let _kobj_stamp = CLK.load(Ordering::Relaxed);
        Arc::new(Self {
            info: Mutex::new(TaskInfo {
                id,
                tag: tag.to_string(),
            }),
            process,
            sig_mask: Mutex::new(0),
            kstk: Mutex::new(None),
            thd_ctx: Mutex::new(Some(ThdCtx::default())),
            sched: Mutex::new(SchedEntity::new()),
        })
    }
    pub fn id(&self) -> usize {
        self.info.lock().unwrap().id
    }
    pub fn vm_token(&self) -> usize {
        self.process.addr_space.lock().unwrap().vm_token()
    }
    pub fn tag(&self) -> String {
        self.info.lock().unwrap().tag.clone()
    }
    pub fn process_pid(&self) -> usize {
        self.process.pid.lock().unwrap().get()
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
    pub fn get_free_fd(&self) -> usize {
        let f = self.process.files.lock().unwrap();
        (0..).find(|i| !f.contains_key(i)).unwrap()
    }
    pub fn get_free_fd_from(&self, arg: usize) -> usize {
        let f = self.process.files.lock().unwrap();
        (arg..).find(|i| !f.contains_key(i)).unwrap()
    }
    // AGENT: install a new fd entry with a fresh shared open-file description.
    pub fn add_file(&self, fl: FLike) -> usize {
        self.add_file_with_cloexec(fl, false)
    }

    // AGENT: install a new fd entry and record per-fd close-on-exec state.
    pub fn add_file_with_cloexec(&self, fl: FLike, cloexec: bool) -> usize {
        let fd = self.get_free_fd();
        self.process
            .files
            .lock()
            .unwrap()
            .insert(fd, FdEntry::with_cloexec(fl, cloexec));
        fd
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
    // AGENT: record process death once; resource teardown is driven by Kernel::exit_task.
    pub fn exit_proc(&self, reason: ExitReason) -> bool {
        {
            let mut exit_reason = self.process.exit_reason.lock().unwrap();
            if exit_reason.is_some() {
                return false;
            }
            *exit_reason = Some(reason);
        }
        {
            self.process.ev.lock().unwrap().set(EvFlag::PROC_QUIT);
        } // AGENT: use EvBus::set instead of manual inline
        {
            let pg = self.process.parent.lock().unwrap();
            if let Some(ref p) = *pg {
                p.process.ev.lock().unwrap().set(EvFlag::CHILD_QUIT);
            } // AGENT: use EvBus::set instead of manual inline
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
        let mut g = self.process.files.lock().unwrap();
        match g.remove(&fd) {
            Some(entry) => {
                let (r, w, e) = entry.poll();
                let _fd_state = (r, w, e);
                Ok(())
            }
            None => Err("ebadf"),
        }
    }

    // AGENT: dup creates a new fd entry that shares the same open-file description.
    pub fn dup_fd(&self, old_fd: usize, cloexec: bool) -> Result<usize, &'static str> {
        let entry = {
            let g = self.process.files.lock().unwrap();
            g.get(&old_fd).cloned().ok_or("ebadf")?
        };
        let new_entry = entry.dup(cloexec);
        // HUMAN
        let nfd = self.get_free_fd();
        self.process.files.lock().unwrap().insert(nfd, new_entry);
        Ok(nfd)
    }

    // AGENT: dup2 replaces only the target fd entry and shares old_fd's open
    // file description.
    pub fn dup2_fd(&self, old_fd: usize, new_fd: usize) -> Result<usize, &'static str> {
        if old_fd == new_fd {
            return Ok(new_fd);
        }
        let entry = {
            let g = self.process.files.lock().unwrap();
            g.get(&old_fd).cloned().ok_or("ebadf")?
        };
        let new_entry = entry.dup(false);
        let mut g = self.process.files.lock().unwrap();
        let _prev = g.remove(&new_fd);
        g.insert(new_fd, new_entry);
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
            seq: AtomicUsize::new(1),
            root: Mutex::new(None),
            fork_reservations: AtomicUsize::new(0),
        }
    }
    pub fn spawn(&self, tag: &str) -> Arc<Task> {
        let id = self.seq.fetch_add(1, Ordering::SeqCst);
        let t = Task::make(id, tag);
        *t.process.pid.lock().unwrap() = Pid(id);
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
    pub fn pgid_group(&self, pgid: Pgid) -> Vec<Arc<Task>> {
        let mut seen = BTreeSet::new();
        self.map
            .read()
            .unwrap()
            .values()
            .filter(|t| *t.process.pgid.lock().unwrap() == pgid)
            .filter(|t| seen.insert(t.process_pid()))
            .cloned()
            .collect()
    }
    pub fn register(&self, task: &Arc<Task>, pid: Pid) {
        *task.process.pid.lock().unwrap() = pid.clone();
        self.map.write().unwrap().insert(pid.get(), task.clone());
    }
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
    pub fn fork_task(&self, src: &Arc<Task>) -> Result<Arc<Task>, &'static str> {
        let fork_slot = self.reserve_fork_slot()?;
        let proc_src = self.process_of_tid(src.id()).unwrap_or_else(|| src.clone());
        let nid = self.seq.fetch_add(1, Ordering::SeqCst);
        let ns = proc_src.tag();
        let child_addr_space = {
            let src_addr_space = proc_src.process.addr_space.lock().unwrap();
            Arc::new(Mutex::new(AddrSpace::fork_from(&src_addr_space)))
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
        {
            let sf = proc_src.process.files.lock().unwrap();
            let mut tf = tgt.process.files.lock().unwrap();
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
        let pg = { *proc_src.process.pgid.lock().unwrap() };
        *tgt.process.pgid.lock().unwrap() = pg;
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
        *tgt.kstk.lock().unwrap() = Some(KStk::new());
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
        {
            let mut fl = t.process.files.lock().unwrap();
            fl.insert(0, FdEntry::new(FLike::File(fd0)));
            fl.insert(1, FdEntry::new(FLike::File(fd1)));
            fl.insert(2, FdEntry::new(FLike::File(fd2)));
        }
        self.register(&t, Pid(t.id()));
        t.process.threads.lock().unwrap().push(t.id());
        t
    }

    pub fn terminate_and_collect(&self, id: usize, code: usize) -> bool {
        let t = { self.map.read().unwrap().get(&id).cloned() };
        if let Some(t) = t {
            t.exit_proc(ExitReason::Code((code & 0xFF) as u8));
            self.reap(id);
            true
        } else {
            false
        }
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

// AGENT: legacy chaos-tests task-info shape moved out of the crate root.
#[derive(Clone, Debug)]
pub struct LegacyTaskInfo {
    pub id: usize,
    pub tag: String,
    pub status: Option<usize>,
}

// AGENT: compatibility wrapper that keeps the old public fields used by
// chaos-tests while carrying the real kernel-sim task internally.
pub struct LegacyTask {
    inner: Arc<Task>,
    pub info: Mutex<LegacyTaskInfo>,
    pub parent: Mutex<Option<Arc<LegacyTask>>>,
}

// AGENT: bridge legacy Task methods to the real simulator task.
impl LegacyTask {
    pub fn make(id: usize, tag: &str) -> Arc<Self> {
        Self::wrap(Task::make(id, tag), None)
    }

    fn wrap(inner: Arc<Task>, parent: Option<Arc<LegacyTask>>) -> Arc<Self> {
        Arc::new(Self {
            info: Mutex::new(LegacyTaskInfo {
                id: inner.id(),
                tag: inner.tag(),
                status: None,
            }),
            inner,
            parent: Mutex::new(parent),
        })
    }

    pub fn id(&self) -> usize {
        self.info.lock().unwrap().id
    }

    fn mark_reaped(&self) {
        self.info.lock().unwrap().status = Some(0);
    }
}

// AGENT: bridge the legacy infallible fork_task API to kernel-sim's fallible
// fork implementation without changing the existing basic tests.
pub struct LegacyTaskTable {
    inner: TaskTable,
    map: RwLock<BTreeMap<usize, Arc<LegacyTask>>>,
    pub root: Mutex<Option<Arc<LegacyTask>>>,
}

// AGENT: expose the legacy task-table surface while delegating storage to the
// real simulator task table.
impl LegacyTaskTable {
    pub fn new() -> Self {
        Self {
            inner: TaskTable::new(),
            map: RwLock::new(BTreeMap::new()),
            root: Mutex::new(None),
        }
    }

    pub fn spawn(&self, tag: &str) -> Arc<LegacyTask> {
        let task = LegacyTask::wrap(self.inner.spawn(tag), None);
        self.map.write().unwrap().insert(task.id(), task.clone());
        task
    }

    pub fn spawn_root(&self) -> Arc<LegacyTask> {
        let task = LegacyTask::wrap(self.inner.spawn_root(), None);
        self.map.write().unwrap().insert(task.id(), task.clone());
        *self.root.lock().unwrap() = Some(task.clone());
        task
    }

    pub fn fork_task(&self, src: &Arc<LegacyTask>) -> Arc<LegacyTask> {
        let child_inner = self
            .inner
            .fork_task(&src.inner)
            .expect("kernel-sim fork_task should succeed for basic tests");
        let child = LegacyTask::wrap(child_inner, Some(src.clone()));
        self.map.write().unwrap().insert(child.id(), child.clone());
        child
    }

    pub fn find(&self, id: usize) -> Option<Arc<LegacyTask>> {
        self.map.read().unwrap().get(&id).cloned()
    }

    pub fn reap(&self, id: usize) {
        if let Some(task) = self.map.write().unwrap().remove(&id) {
            task.mark_reaped();
        }
        self.inner.reap(id);
    }

    pub fn count(&self) -> usize {
        self.map.read().unwrap().len()
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
