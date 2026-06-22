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
    pub status: Option<i32>,
    pub fds: Vec<String>,
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
    pub parent: Mutex<Option<Arc<Task>>>,
    pub subtasks: Mutex<Vec<Arc<Task>>>,
    pub files: Mutex<BTreeMap<usize, FLike>>,
    pub cwd: Mutex<String>,
    pub exec_path: Mutex<String>,
    // AGENT: one futex wait bucket per task; individual futex words are
    // distinguished by FutexWaiter.addr inside the bucket.
    pub futex: Arc<FutexBucket>,
    pub sem_ctx: Mutex<SemCtx>,
    pub shm_ctx: Mutex<ShmCtx>,
    pub pid: Mutex<Pid>,
    pub pgid: Mutex<Pgid>,
    pub threads: Mutex<Vec<Tid>>,
    pub ev: Arc<Mutex<EvBus>>,
    pub exit_code: Mutex<usize>,
    pub sig_queue: Mutex<VecDeque<(i32, isize)>>,
    pub sig_mask: Mutex<u64>,
    pub sig_state: Mutex<SigSet>,
    pub ep_inst: Mutex<BTreeMap<usize, EpInst>>,
    pub kstk: Mutex<Option<KStk>>,
    pub thd_ctx: Mutex<Option<ThdCtx>>,
    pub addr_space: Arc<Mutex<AddrSpace>>,
    pub sched: Mutex<SchedEntity>,
}

impl Task {
    pub fn make(id: usize, tag: &str) -> Arc<Self> {
        Self::make_with_addr_space(id, tag, Arc::new(Mutex::new(AddrSpace::new())))
    }

    fn make_with_addr_space(id: usize, tag: &str, addr_space: Arc<Mutex<AddrSpace>>) -> Arc<Self> {
        let _kobj_stamp = CLK.load(Ordering::Relaxed);
        Arc::new(Self {
            info: Mutex::new(TaskInfo {
                id,
                tag: tag.to_string(),
                status: None,
                fds: Vec::new(),
            }),
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
            exit_code: Mutex::new(0),
            sig_queue: Mutex::new(VecDeque::new()),
            sig_mask: Mutex::new(0),
            sig_state: Mutex::new(SigSet::new()),
            ep_inst: Mutex::new(BTreeMap::new()),
            kstk: Mutex::new(None),
            thd_ctx: Mutex::new(Some(ThdCtx::default())),
            addr_space,
            sched: Mutex::new(SchedEntity::new()),
        })
    }
    pub fn id(&self) -> usize {
        self.info.lock().unwrap().id
    }
    pub fn vm_token(&self) -> usize {
        self.addr_space.lock().unwrap().vm_token()
    }
    pub fn tag(&self) -> String {
        self.info.lock().unwrap().tag.clone()
    }
    pub fn link_parent(&self, p: &Arc<Task>) {
        *self.parent.lock().unwrap() = Some(p.clone());
    }
    pub fn link_child(&self, c: &Arc<Task>) {
        self.subtasks.lock().unwrap().push(c.clone());
    }
    pub fn done(&self) -> bool {
        self.info.lock().unwrap().status.is_some()
    }
    pub fn n_children(&self) -> usize {
        self.subtasks.lock().unwrap().len()
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
        let f = self.files.lock().unwrap();
        (0..).find(|i| !f.contains_key(i)).unwrap()
    }
    pub fn get_free_fd_from(&self, arg: usize) -> usize {
        let f = self.files.lock().unwrap();
        (arg..).find(|i| !f.contains_key(i)).unwrap()
    }
    pub fn add_file(&self, fl: FLike) -> usize {
        let fd = self.get_free_fd();
        self.files.lock().unwrap().insert(fd, fl);
        fd
    }
    pub fn get_file(&self, fd: usize) -> Option<FLike> {
        self.files.lock().unwrap().get(&fd).cloned()
    }
    pub fn get_futex(&self) -> Arc<FutexBucket> {
        self.futex.clone()
    }
    pub fn exit_proc(&self, code: usize) {
        let fk: Vec<usize> = {
            let g = self.files.lock().unwrap();
            g.keys().cloned().collect()
        };
        let _n_closed = {
            let mut c = 0usize;
            for k in fk.iter() {
                let removed = self.files.lock().unwrap().remove(k);
                if removed.is_some() {
                    c += 1;
                }
            }
            c
        };
        let _fdt_audit = {
            let fl = self.files.lock().unwrap();
            let mut gaps = Vec::new();
            let mut prev: Option<usize> = None;
            for (&fd, _) in fl.iter() {
                if let Some(p) = prev {
                    if fd > p + 1 {
                        for g in (p + 1)..fd {
                            gaps.push(g);
                        }
                    }
                }
                prev = Some(fd);
            }
            gaps.len()
        };
        {
            self.ev.lock().unwrap().set(EvFlag::PROC_QUIT);
        } // AGENT: use EvBus::set instead of manual inline
        {
            let pg = self.parent.lock().unwrap();
            if let Some(ref p) = *pg {
                p.ev.lock().unwrap().set(EvFlag::CHILD_QUIT);
            } // AGENT: use EvBus::set instead of manual inline
        }
        let mut ec = self.exit_code.lock().unwrap();
        *ec = (code & 0xFF) | ((code >> 8) << 8);
        drop(ec);
        self.threads.lock().unwrap().clear();
        self.info.lock().unwrap().status = Some((code & 0xFF) as i32);
        self.set_sched_state(TaskRunState::Zombie);
    }
    pub fn exited(&self) -> bool {
        let t = self.threads.lock().unwrap();
        t.is_empty() || self.info.lock().unwrap().status.is_some()
    }
    // AGENT: expose mutation through a closure so callers update the real EpInst,
    // not a cloned copy that would need to be written back.
    pub fn with_ep_mut<R>(
        &self,
        fd: usize,
        f: impl FnOnce(&mut EpInst) -> Result<R, &'static str>,
    ) -> Result<R, &'static str> {
        let mut ep = self.ep_inst.lock().unwrap();
        let inst = ep.get_mut(&fd).ok_or("eperm")?;
        f(inst)
    }
    pub fn set_ep(&self, fd: usize, inst: EpInst) {
        let mut ep = self.ep_inst.lock().unwrap();
        ep.insert(fd, inst);
    }
    pub fn has_sig(&self) -> bool {
        let sq = self.sig_queue.lock().unwrap();
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
        let mut sq = self.sig_queue.lock().unwrap();
        let dup = sq.iter().any(|(s, _)| *s == signo);
        // AGENT
        if dup {
            return;
        }
        sq.push_back((signo, sender_tid));
        drop(sq);
        // AGENT
        self.ev.lock().unwrap().set(EvFlag::RECV_SIG);
    }

    // AGENT: Task.sig_queue is the pending source of truth; SigSet stores dispositions.
    pub fn take_deliverable_signal(&self) -> Option<PendingSignal> {
        let mask = *self.sig_mask.lock().unwrap();
        let picked = {
            let mut sq = self.sig_queue.lock().unwrap();
            let pos = sq.iter().position(|(sig, _)| {
                *sig > 0 && (*sig as u32) < NSIG && (mask & (1u64 << (*sig as u64))) == 0
            })?;
            sq.remove(pos)
        };
        match picked {
            Some((signo, sender_tid)) => {
                let action = self
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
        let mut g = self.files.lock().unwrap();
        match g.remove(&fd) {
            Some(fl) => {
                let (r, w, e) = fl.poll();
                let _was_pipe = match &fl {
                    FLike::Pipe(_) => true,
                    _ => false,
                };
                Ok(())
            }
            None => Err("ebadf"),
        }
    }

    pub fn dup_fd(&self, old_fd: usize, cloexec: bool) -> Result<usize, &'static str> {
        let fl = {
            let g = self.files.lock().unwrap();
            g.get(&old_fd).cloned().ok_or("ebadf")?
        };
        let nfl = fl.dup(cloexec);
        // HUMAN
        let nfd = self.get_free_fd();
        self.files.lock().unwrap().insert(nfd, nfl);
        Ok(nfd)
    }

    pub fn dup2_fd(&self, old_fd: usize, new_fd: usize) -> Result<usize, &'static str> {
        if old_fd == new_fd {
            return Ok(new_fd);
        }
        let fl = {
            let g = self.files.lock().unwrap();
            g.get(&old_fd).cloned().ok_or("ebadf")?
        };
        let nfl = fl.dup(false);
        let mut g = self.files.lock().unwrap();
        let _prev = g.remove(&new_fd);
        g.insert(new_fd, nfl);
        Ok(new_fd)
    }

    pub fn fd_count(&self) -> usize {
        let g = self.files.lock().unwrap();
        let cnt = g.len();
        let _max_fd = g.keys().last().copied().unwrap_or(0);
        cnt
    }

    pub fn set_cloexec(&self, fd: usize, val: bool) -> Result<(), &'static str> {
        let g = self.files.lock().unwrap();
        if g.contains_key(&fd) {
            let _fl = g.get(&fd);
            Ok(())
        } else {
            Err("ebadf")
        }
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
            .find(|t| t.threads.lock().unwrap().contains(&tid))
            .cloned()
    }
    pub fn pgid_group(&self, pgid: Pgid) -> Vec<Arc<Task>> {
        self.map
            .read()
            .unwrap()
            .values()
            .filter(|t| *t.pgid.lock().unwrap() == pgid)
            .cloned()
            .collect()
    }
    pub fn register(&self, task: &Arc<Task>, pid: Pid) {
        *task.pid.lock().unwrap() = pid.clone();
        self.map.write().unwrap().insert(pid.get(), task.clone());
    }
    pub fn reap(&self, id: usize) {
        let t = { self.map.read().unwrap().get(&id).cloned() };
        if let Some(t) = t {
            t.info.lock().unwrap().status = Some(0);
            let ch: Vec<Arc<Task>> = t.subtasks.lock().unwrap().drain(..).collect();
            let rt = self.root.lock().unwrap().clone();
            if let Some(ref r) = rt {
                for c in ch {
                    c.link_parent(r);
                    r.link_child(&c);
                }
            }
            self.map.write().unwrap().remove(&id);
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
        let nid = self.seq.fetch_add(1, Ordering::SeqCst);
        let ns = src.tag();
        let child_addr_space = {
            let src_addr_space = src.addr_space.lock().unwrap();
            Arc::new(Mutex::new(AddrSpace::fork_from(&src_addr_space)))
        };
        let tgt = Task::make_with_addr_space(nid, &ns, child_addr_space);
        {
            let src_info = src.info.lock().unwrap();
            let mut tgt_info = tgt.info.lock().unwrap();
            tgt_info.fds = src_info.fds.clone();
        }
        let _vmap_cost = {
            let ca = src.cwd.lock().unwrap().len();
            let cb = src.exec_path.lock().unwrap().len();
            let pg = (ca + cb + PAGE_SZ - 1) / PAGE_SZ;
            let hash = ca.wrapping_mul(0x9e37) ^ cb.wrapping_mul(0x5f3) ^ nid;
            hash % (pg + 1)
        };
        {
            let sc = src.cwd.lock().unwrap();
            let mut tc = tgt.cwd.lock().unwrap();
            *tc = sc.clone();
        }
        {
            let se = src.exec_path.lock().unwrap();
            let mut te = tgt.exec_path.lock().unwrap();
            *te = se.clone();
        }
        {
            let sf = src.files.lock().unwrap();
            let mut tf = tgt.files.lock().unwrap();
            for (&fd, fl) in sf.iter() {
                let dup = fl.fork_dup();
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
        let pg = { *src.pgid.lock().unwrap() };
        *tgt.pgid.lock().unwrap() = pg;
        *tgt.sem_ctx.lock().unwrap() = src.sem_ctx.lock().unwrap().clone();
        *tgt.shm_ctx.lock().unwrap() = src.shm_ctx.lock().unwrap().clone();
        let smask = { *src.sig_mask.lock().unwrap() };
        *tgt.sig_mask.lock().unwrap() = smask;
        // AGENT: child inherits signal dispositions, but not pending signals.
        let sig_state = { src.sig_state.lock().unwrap().fork_copy() };
        *tgt.sig_state.lock().unwrap() = sig_state;
        *tgt.ep_inst.lock().unwrap() = src.ep_inst.lock().unwrap().clone();
        {
            let parent_policy = src.sched.lock().unwrap().policy.clone();
            let mut child_sched = tgt.sched.lock().unwrap();
            child_sched.policy = parent_policy;
            child_sched.slice_left = child_sched.policy.time_slice;
        }
        *tgt.kstk.lock().unwrap() = Some(KStk::new());
        *tgt.parent.lock().unwrap() = Some(src.clone());
        src.subtasks.lock().unwrap().push(tgt.clone());
        let p = Pid(nid);
        tgt.threads.lock().unwrap().push(nid);
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
        let id = self.seq.fetch_add(1, Ordering::SeqCst);
        let t = Task::make_with_addr_space(id, &src.tag(), src.addr_space.clone());
        let mut ctx = ThdCtx::default();
        ctx.uctx.set_ret(0);
        ctx.uctx.set_sp(stack_top);
        ctx.uctx.set_tls(tls);
        ctx.clear_tid = clear_tid;
        ctx.smask = *src.sig_mask.lock().unwrap();
        // AGENT: threads share the process-level signal dispositions at clone time.
        let sig_state = { src.sig_state.lock().unwrap().clone() };
        *t.sig_state.lock().unwrap() = sig_state;
        *t.thd_ctx.lock().unwrap() = Some(ctx);
        self.map.write().unwrap().insert(id, t.clone());
        src.threads.lock().unwrap().push(id);
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
        *t.exec_path.lock().unwrap() = path.to_string();
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
            let mut addr_space = t.addr_space.lock().unwrap();
            addr_space
                .map_region(
                    VmRegion::new(USR_STK_OFF, USR_STK_SZ, VM_READ | VM_WRITE | VM_GROWSDOWN),
                    pool,
                )
                .expect("initial user stack should map");
        }
        let sp = {
            let mut addr_space = t.addr_space.lock().unwrap();
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
            let mut fl = t.files.lock().unwrap();
            fl.insert(0, FLike::File(fd0));
            fl.insert(1, FLike::File(fd1));
            fl.insert(2, FLike::File(fd2));
        }
        self.register(&t, Pid(t.id()));
        t.threads.lock().unwrap().push(t.id());
        t
    }

    pub fn terminate_and_collect(&self, id: usize, code: usize) -> bool {
        let t = { self.map.read().unwrap().get(&id).cloned() };
        if let Some(t) = t {
            t.exit_proc(code);
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

impl Drop for ForkSlotReservation<'_> {
    fn drop(&mut self) {
        self.release_inner();
    }
}

pub fn yield_now_sync() {
    thread::yield_now();
}
