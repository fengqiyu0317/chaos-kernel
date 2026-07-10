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

// AGENT: keep this enum limited to scheduler placement; job-control stop state
// lives separately on ProcessState so signal semantics do not pollute run state.
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
    // AGENT: Initialize the runtime countdown from the priority-derived time slice.
    pub fn new() -> Self {
        let policy = SchedulePolicy::new();
        let slice_left = policy.time_slice();
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
    // AGENT: job-control stop is process-wide state, separate from scheduler
    // placement so stopped runnable tasks do not enter the run queue.
    pub job_stopped: AtomicBool,
    pub threads: Mutex<Vec<Tid>>,
    pub ev: Arc<Mutex<EvBus>>,
    pub exit_reason: Mutex<Option<ExitReason>>,
    pub sig_queue: Mutex<VecDeque<(i32, isize)>>,
    pub sig_state: Mutex<SigSet>,
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
            job_stopped: AtomicBool::new(false),
            threads: Mutex::new(Vec::new()),
            ev: EvBus::make(),
            exit_reason: Mutex::new(None),
            sig_queue: Mutex::new(VecDeque::new()),
            sig_state: Mutex::new(SigSet::new()),
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
    pub restored_trap_frame: Mutex<Option<SavedTrapFrame>>,
    pub sched: Mutex<SchedEntity>,
}

// AGENT: fd close cleanup is collected under the fd-table lock and executed
// after releasing it so source unsubscribe and final Drop cannot re-enter the
// fd table while it is locked.
struct FdCloseCleanup {
    closed_entry: FdEntry,
    closed_fd_source_subs: Vec<usize>,
    epoll_source_subs: Vec<(FdEntry, usize)>,
}

impl FdCloseCleanup {
    // AGENT: detach source callbacks before dropping the closed fd entry.
    fn run(self) {
        for sub_id in self.closed_fd_source_subs {
            self.closed_entry.unregister_epoll_source(sub_id);
        }
        for (source, sub_id) in self.epoll_source_subs {
            source.unregister_epoll_source(sub_id);
        }
        drop(self.closed_entry);
    }
}

// AGENT: keep the first checkpoint fd surface intentionally narrow until the
// image format grows file-object sections for arbitrary open files.
fn checkpoint_fd_kind(fd: usize) -> Result<SavedFdKind, &'static str> {
    match fd {
        0 => Ok(SavedFdKind::Stdin),
        1 => Ok(SavedFdKind::Stdout),
        2 => Ok(SavedFdKind::Stderr),
        _ => Err("enotsup"),
    }
}

// AGENT: assign stable per-image ids to shared open-file descriptions by
// comparing the fd-table Arc identity through FdEntry.
fn checkpoint_description_id(
    entry: &FdEntry,
    descriptions: &mut Vec<(FdEntry, u32)>,
) -> Result<u32, &'static str> {
    for (known, id) in descriptions.iter() {
        if entry.same_open_description(known) {
            return Ok(*id);
        }
    }
    let id = u32::try_from(descriptions.len() + 1).map_err(|_| "einval")?;
    descriptions.push((entry.clone(), id));
    Ok(id)
}

// AGENT: rebuild the stdio-like instances supported by the first checkpoint
// restore pass; regular path files need a later file-object section.
fn checkpoint_stdio_instance(
    kind: SavedFdKind,
    status_flags: u32,
) -> Result<(FInstance, FdOpt), &'static str> {
    let mut opt = match kind {
        SavedFdKind::Stdin => FdOpt {
            rd: true,
            wr: false,
            ap: false,
            nb: false,
        },
        SavedFdKind::Stdout | SavedFdKind::Stderr => FdOpt {
            rd: false,
            wr: true,
            ap: false,
            nb: false,
        },
        SavedFdKind::RegularMemoryFile
        | SavedFdKind::Pipe
        | SavedFdKind::Epoll
        | SavedFdKind::Socket
        | SavedFdKind::Tty => return Err("enotsup"),
    };
    opt.apply_status_flags(status_flags as usize);
    let path = match kind {
        SavedFdKind::Stdin => "/dev/stdin",
        SavedFdKind::Stdout => "/dev/stdout",
        SavedFdKind::Stderr => "/dev/stderr",
        _ => return Err("enotsup"),
    };
    Ok((FInstance::new(path), opt))
}

struct InitialUserImage {
    addr_space: AddrSpace,
    thd_ctx: ThdCtx,
}

// AGENT: build the first user image from explicit ELF bytes so task creation
// does not depend on a fake hard-coded header or hidden filesystem state.
fn prepare_initial_user_image(
    elf_data: &[u8],
    args: Vec<String>,
    envs: Vec<String>,
    pool: &FramePool,
) -> Result<InitialUserImage, &'static str> {
    let (entry, load_segments) = parse_elf_load_segments(elf_data)?;
    let mut addr_space = AddrSpace::new();
    match populate_initial_user_image(
        &mut addr_space,
        elf_data,
        load_segments,
        entry,
        args,
        envs,
        pool,
    ) {
        Ok(thd_ctx) => Ok(InitialUserImage {
            addr_space,
            thd_ctx,
        }),
        Err(err) => {
            addr_space.release_all_pages(pool);
            Err(err)
        }
    }
}

fn populate_initial_user_image(
    addr_space: &mut AddrSpace,
    elf_data: &[u8],
    load_segments: Vec<ElfLoadSegment>,
    entry: usize,
    args: Vec<String>,
    envs: Vec<String>,
    pool: &FramePool,
) -> Result<ThdCtx, &'static str> {
    let mut image_end = 0usize;
    for segment in load_segments {
        let region = segment.vm_region()?;
        let region_base = region.base;
        let region_len = region.len;
        let region_flags = region.flags;
        let region_end = region.end();
        addr_space.map_region(
            VmRegion {
                flags: region_flags | VM_WRITE,
                ..region
            },
            pool,
        )?;

        let file_end = segment
            .offset
            .checked_add(segment.file_size)
            .ok_or("ph_overflow")?;
        if file_end > elf_data.len() {
            return Err("ph_overflow");
        }
        addr_space.write_user_bytes(segment.vaddr, &elf_data[segment.offset..file_end], pool)?;
        addr_space.protect(region_base, region_len, region_flags)?;
        image_end = max(image_end, region_end);
    }

    let init = ProcInit {
        args,
        envs,
        auxv: BTreeMap::from([(AT_PAGESZ, PAGE_SZ), (AT_ENTRY, entry)]),
    };
    if init.checked_total_size()? > USR_STK_SZ {
        return Err("e2big");
    }

    let stack = VmRegion::new(USR_STK_OFF, USR_STK_SZ, VM_READ | VM_WRITE | VM_GROWSDOWN);
    addr_space.map_region(stack, pool)?;
    let sp = init.push_at(addr_space, pool, USR_STK_OFF + USR_STK_SZ)?;
    if sp < USR_STK_OFF || sp > USR_STK_OFF + USR_STK_SZ {
        return Err("e2big");
    }

    addr_space.vm_map.brk = (image_end + PAGE_SZ - 1) & !(PAGE_SZ - 1);
    let mut thd_ctx = ThdCtx::default();
    thd_ctx.uctx.set_sp(sp as u64);
    thd_ctx.uctx.set_ip(entry as u64);
    Ok(thd_ctx)
}

// AGENT: seed stdio through the normal fd allocator so the descriptor table and
// free-fd set stay consistent with every later open/dup/close path.
fn install_initial_stdio(task: &Arc<Task>) -> Result<(), &'static str> {
    let stdin_opt = FdOpt {
        rd: true,
        wr: false,
        ap: false,
        nb: false,
    };
    let stdout_opt = FdOpt {
        rd: false,
        wr: true,
        ap: false,
        nb: false,
    };
    let stdin_instance = FInstance::new("/dev/tty");
    let stdout_instance = FInstance::new("/dev/tty");
    let stderr_instance = stdout_instance.dup();
    let stdin = task.add_file_with_status(FLike::File(FHandle::new(stdin_instance)), stdin_opt)?;
    let stdout =
        task.add_file_with_status(FLike::File(FHandle::new(stdout_instance)), stdout_opt)?;
    let stderr =
        task.add_file_with_status(FLike::File(FHandle::new(stderr_instance)), stdout_opt)?;
    if (stdin, stdout, stderr) != (0, 1, 2) {
        return Err("ebadf");
    }
    Ok(())
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
        Arc::new(Self {
            info: Mutex::new(TaskInfo {
                id,
                tag: tag.to_string(),
            }),
            process,
            sig_mask: Mutex::new(0),
            kstk: Mutex::new(Some(KStk::new())),
            thd_ctx: Mutex::new(Some(ThdCtx::default())),
            restored_trap_frame: Mutex::new(None),
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
    // AGENT: store a complete checkpoint trap frame for the first restored
    // user-mode entry, separate from the lossy simulator ThdCtx.
    pub fn set_restored_trap_frame(&self, frame: SavedTrapFrame) {
        *self.restored_trap_frame.lock().unwrap() = Some(frame);
    }
    // AGENT: consume the restored frame once it has been materialized on the
    // task's kernel stack for trap return.
    pub fn take_restored_trap_frame(&self) -> Option<SavedTrapFrame> {
        self.restored_trap_frame.lock().unwrap().take()
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
    // AGENT: expose process-level job-control stop without overloading
    // TaskRunState; the scheduler checks this before enqueueing runnable work.
    pub fn is_job_stopped(&self) -> bool {
        self.process.job_stopped.load(Ordering::Relaxed)
    }
    // AGENT: SIGSTOP/SIGCONT own this flag; scheduler state remains the task's
    // runnable/sleeping/running placement.
    pub fn set_job_stopped(&self, stopped: bool) {
        self.process.job_stopped.store(stopped, Ordering::Relaxed);
    }
    pub fn sched_policy(&self) -> SchedulePolicy {
        self.sched.lock().unwrap().policy.clone()
    }
    // AGENT: update the task-owned scheduling policy so boosts survive running,
    // sleeping, and later requeue transitions.
    pub fn boost_priority(&self, amount: i32) -> SchedulePolicy {
        let mut sched = self.sched.lock().unwrap();
        let amount = amount.max(0);
        let prio = sched.policy.prio.saturating_sub(amount);
        sched.policy = SchedulePolicy::with_prio(prio);
        sched.slice_left = sched.slice_left.min(sched.policy.time_slice());
        sched.policy.clone()
    }
    // AGENT: Recompute the reset slice from priority so SchedulePolicy stores
    // only the source priority.
    pub fn reset_slice(&self) {
        let mut sched = self.sched.lock().unwrap();
        sched.slice_left = sched.policy.time_slice();
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

    // AGENT: install a new fd entry with explicit open-file-description state for
    // regular files whose FInstance only stores the backing object.
    pub fn add_file_with_status(&self, fl: FLike, status: FdOpt) -> Result<usize, &'static str> {
        self.add_file_with_cloexec_and_status(fl, status, false)
    }

    // AGENT: install a new fd entry and record per-fd close-on-exec state.
    pub fn add_file_with_cloexec(&self, fl: FLike, cloexec: bool) -> Result<usize, &'static str> {
        let mut files = self.process.files.lock().unwrap();
        let mut free_fds = self.process.free_fds.lock().unwrap();
        let fd = Self::reserve_fd_from_locked(&mut free_fds, 0)?;
        files.insert(fd, FdEntry::with_cloexec(fl, cloexec));
        Ok(fd)
    }

    // AGENT: install a new fd entry while supplying the open-file-description
    // status explicitly instead of reading it from the file object.
    pub fn add_file_with_cloexec_and_status(
        &self,
        fl: FLike,
        status: FdOpt,
        cloexec: bool,
    ) -> Result<usize, &'static str> {
        let mut files = self.process.files.lock().unwrap();
        let mut free_fds = self.process.free_fds.lock().unwrap();
        let fd = Self::reserve_fd_from_locked(&mut free_fds, 0)?;
        files.insert(fd, FdEntry::with_status(fl, status, cloexec));
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

    // AGENT: serialize the first checkpoint fd subset: stdio descriptors only,
    // preserving per-fd cloexec and shared open-file-description identity.
    pub fn snapshot_checkpoint_fds(&self) -> Result<Vec<SavedFdEntry>, &'static str> {
        let files = self.process.files.lock().unwrap();
        let mut descriptions: Vec<(FdEntry, u32)> = Vec::new();
        let mut saved = Vec::with_capacity(files.len());
        for (&fd, entry) in files.iter() {
            let kind = checkpoint_fd_kind(fd)?;
            if !entry.is_regular_file() {
                return Err("enotsup");
            }
            let description_id = checkpoint_description_id(entry, &mut descriptions)?;
            saved.push(SavedFdEntry {
                fd: u32::try_from(fd).map_err(|_| "einval")?,
                description_id,
                cloexec: entry.is_cloexec(),
                status_flags: u32::try_from(entry.status_flags_bits()).map_err(|_| "einval")?,
                kind,
                offset: entry.offset(),
            });
        }
        Ok(saved)
    }

    // AGENT: restore the first checkpoint fd subset by rebuilding stdio-like
    // memory instances and sharing duplicated open-file-descriptions by id.
    pub fn restore_checkpoint_fds(&self, fds: &[SavedFdEntry]) -> Result<(), &'static str> {
        let mut restored = BTreeMap::new();
        let mut descriptions: BTreeMap<u32, FdEntry> = BTreeMap::new();
        for saved in fds {
            let fd = usize::try_from(saved.fd).map_err(|_| "einval")?;
            if fd >= MAX_FD || restored.contains_key(&fd) {
                return Err("ebadf");
            }
            let entry = if let Some(template) = descriptions.get(&saved.description_id) {
                template.dup(saved.cloexec)
            } else {
                let (instance, status) = checkpoint_stdio_instance(saved.kind, saved.status_flags)?;
                let entry = FdEntry::with_status(
                    FLike::File(FHandle::new(instance)),
                    status,
                    saved.cloexec,
                );
                entry.seek(FSeek::Start(saved.offset))?;
                descriptions.insert(saved.description_id, entry.clone());
                entry
            };
            restored.insert(fd, entry);
        }

        let mut free_fds = ProcessState::initial_free_fds();
        for fd in restored.keys() {
            free_fds.remove(fd);
        }
        *self.process.files.lock().unwrap() = restored;
        *self.process.free_fds.lock().unwrap() = free_fds;
        Ok(())
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

    // AGENT: clear CLONE_CHILD_CLEARTID's user word before dropping thread
    // context, then wake one futex waiter on that address.
    fn clear_child_tid_and_wake(&self, clear_tid: usize, pool: &FramePool) {
        if clear_tid == 0 {
            return;
        }

        let zero = 0u32.to_ne_bytes();
        let cleared = self
            .process
            .addr_space
            .lock()
            .unwrap()
            .write_user_bytes(clear_tid, &zero, pool)
            .is_ok();
        if cleared {
            self.process.futex.wake(clear_tid, 1);
        }
    }

    // AGENT: drop thread-private execution resources once the process is dead,
    // preserving clear-child-tid teardown before discarding ThdCtx.
    pub fn release_thread_exit_resources(&self, pool: &FramePool) {
        *self.sig_mask.lock().unwrap() = 0;
        self.kstk.lock().unwrap().take();
        let old_ctx = self.thd_ctx.lock().unwrap().take();
        if let Some(ctx) = old_ctx {
            self.clear_child_tid_and_wake(ctx.clear_tid, pool);
        }
        self.set_sched_state(TaskRunState::Zombie);
    }
    pub fn wait_status(&self) -> usize {
        match *self.process.exit_reason.lock().unwrap() {
            Some(reason) => reason.wait_status(),
            None => 0,
        }
    }
    // AGENT: enqueue a standard pending signal for this process. Scheduler
    // wakeups belong to Kernel::send_signal_to_task, so direct callers cannot
    // accidentally bypass the run-queue transition.
    pub(crate) fn enqueue_signal(&self, signo: i32, sender_tid: isize) -> bool {
        if signo <= 0 || signo as u32 >= NSIG {
            return false;
        }
        let mut sq = self.process.sig_queue.lock().unwrap();
        if sq.iter().any(|(s, _)| *s == signo) {
            return false;
        }
        sq.push_back((signo, sender_tid));
        drop(sq);
        self.process.ev.lock().unwrap().set(EvFlag::RECV_SIG);
        true
    }

    // AGENT: put back a signal that could not be delivered because the task
    // currently has no user context to receive a handler frame.
    pub(crate) fn requeue_signal_front(&self, signo: i32, sender_tid: isize) {
        self.process
            .sig_queue
            .lock()
            .unwrap()
            .push_front((signo, sender_tid));
        self.process.ev.lock().unwrap().set(EvFlag::RECV_SIG);
    }

    // AGENT: interruptible waits need a non-destructive pending-signal check;
    // ignored and default-ignored signals should not break a blocking syscall.
    pub fn has_interrupting_signal(&self) -> bool {
        let mask = *self.sig_mask.lock().unwrap();
        let sq = self.process.sig_queue.lock().unwrap();
        let sig_state = self.process.sig_state.lock().unwrap();
        sq.iter().any(|(sig, _)| {
            if *sig <= 0 || (*sig as u32) >= NSIG {
                return false;
            }
            let signo = *sig as u32;
            if (mask & (1u64 << signo)) != 0 {
                return false;
            }
            let action = sig_state.get_action(signo);
            action.handler != SIG_IGN && !(action.handler == SIG_DFL && signo == SIGCHLD)
        })
    }

    // AGENT: ProcessState.sig_queue is the pending source of truth; SigSet
    // stores dispositions, including which pending signals can be discarded as
    // ignored before delivery.
    pub fn take_deliverable_signal(&self) -> Option<PendingSignal> {
        let mask = *self.sig_mask.lock().unwrap();
        loop {
            let (signo, sender_tid) = {
                let mut sq = self.process.sig_queue.lock().unwrap();
                let pos = sq.iter().position(|(sig, _)| {
                    *sig > 0 && (*sig as u32) < NSIG && (mask & (1u64 << (*sig as u64))) == 0
                })?;
                sq.remove(pos)?
            };
            let action = {
                let sig_state = self.process.sig_state.lock().unwrap();
                if sig_state.is_ignored(signo as u32) {
                    continue;
                }
                sig_state.get_action(signo as u32).clone()
            };
            return Some(PendingSignal {
                signo: signo as u32,
                sender_tid,
                action,
            });
        }
    }

    // AGENT: remove one fd from the table and collect epoll subscriptions that
    // must be cancelled after the fd-table lock is released.
    fn remove_fd_locked(
        files: &mut BTreeMap<usize, FdEntry>,
        fd: usize,
    ) -> Result<FdCloseCleanup, &'static str> {
        let closed_entry = files.remove(&fd).ok_or("ebadf")?;

        let mut closed_fd_source_subs = Vec::new();
        for entry in files.values() {
            if let Some(epoll) = entry.epoll_instance() {
                if let Some(sub_id) = epoll.remove_closed_fd(fd) {
                    closed_fd_source_subs.push(sub_id);
                }
            }
        }

        let mut epoll_source_subs = Vec::new();
        let still_open = files
            .values()
            .any(|entry| entry.same_open_description(&closed_entry));
        if !still_open {
            if let Some(epoll) = closed_entry.epoll_instance() {
                for (watched_fd, sub_id) in epoll.drain_source_subs_on_close() {
                    if let Some(source) = files.get(&watched_fd).cloned() {
                        epoll_source_subs.push((source, sub_id));
                    }
                }
            }
        }

        Ok(FdCloseCleanup {
            closed_entry,
            closed_fd_source_subs,
            epoll_source_subs,
        })
    }

    // AGENT: close removes fd-table state, detaches epoll registrations, then
    // drops the open-file description outside the fd-table lock.
    pub fn close_fd(&self, fd: usize) -> Result<(), &'static str> {
        if fd >= MAX_FD {
            return Err("ebadf");
        }

        let cleanup = {
            let mut files = self.process.files.lock().unwrap();
            let cleanup = Self::remove_fd_locked(&mut files, fd)?;
            self.process.free_fds.lock().unwrap().insert(fd);
            cleanup
        };

        cleanup.run();
        Ok(())
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

    // AGENT: dup2 replaces the target through the same close cleanup path as
    // close_fd so watched-fd and epoll-instance lifetimes stay consistent.
    pub fn dup2_fd(&self, old_fd: usize, new_fd: usize) -> Result<usize, &'static str> {
        if old_fd >= MAX_FD || new_fd >= MAX_FD {
            return Err("ebadf");
        }
        let cleanup = {
            let mut files = self.process.files.lock().unwrap();
            let entry = files.get(&old_fd).cloned().ok_or("ebadf")?;
            if old_fd == new_fd {
                return Ok(new_fd);
            }
            let cleanup = if files.contains_key(&new_fd) {
                Some(Self::remove_fd_locked(&mut files, new_fd)?)
            } else {
                None
            };
            files.insert(new_fd, entry.dup(false));
            self.process.free_fds.lock().unwrap().remove(&new_fd);
            cleanup
        };
        if let Some(cleanup) = cleanup {
            cleanup.run();
        }
        Ok(new_fd)
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

// AGENT: TaskTable tracks schedulable task ids; process grouping remains
// indexed by process pid instead of thread id.
pub struct TaskTable {
    pub map: RwLock<BTreeMap<usize, Arc<Task>>>,
    // AGENT: process groups are indexed by pgid and store process pids, not
    // thread ids; Task.process.pgid mirrors this authoritative membership map.
    pub groups: Mutex<BTreeMap<Pgid, Arc<ProcessGroup>>>,
    pub seq: AtomicUsize,
    pub root: Mutex<Option<Arc<Task>>>,
    // AGENT: reserve capacity for task-table insertions before registration so
    // concurrent creators cannot all pass the N_PROC check at once.
    task_reservations: AtomicUsize,
}
impl TaskTable {
    // AGENT: initialize task-slot reservations beside the task map they protect.
    pub fn new() -> Self {
        Self {
            map: RwLock::new(BTreeMap::new()),
            groups: Mutex::new(BTreeMap::new()),
            seq: AtomicUsize::new(1),
            root: Mutex::new(None),
            task_reservations: AtomicUsize::new(0),
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
    // session/process group; callers enqueue them separately when runnable.
    pub fn spawn(&self, tag: &str) -> Result<Arc<Task>, &'static str> {
        let slot = self.reserve_task_slot()?;
        let id = self.seq.fetch_add(1, Ordering::SeqCst);
        let t = self.insert_new_process(id, tag)?;
        slot.release();
        Ok(t)
    }
    // AGENT: init is a singleton and must consume pid 1 before any other task
    // is inserted, because signal and reparenting paths treat pid 1 specially.
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

        let t = self.insert_new_process(Pid::INIT, "init")?;
        *root = Some(t.clone());
        slot.release();
        Ok(t)
    }
    // AGENT: `id` is a schedulable task/thread id; callers that need the
    // owning process should resolve through process_of_tid().
    pub fn find(&self, id: usize) -> Option<Arc<Task>> {
        self.map.read().unwrap().get(&id).cloned()
    }

    // AGENT: return one process representative per tag so cloned threads do
    // not duplicate process-level tag searches.
    pub fn find_by_tag(&self, tag: &str) -> Vec<Arc<Task>> {
        let mut seen = BTreeSet::new();
        self.map
            .read()
            .unwrap()
            .values()
            .filter(|t| t.tag() == tag)
            .filter(|t| seen.insert(t.process_pid()))
            .cloned()
            .collect()
    }

    // AGENT: resolve a thread id to its process leader explicitly instead of
    // depending on task-table iteration order.
    pub fn process_of_tid(&self, tid: usize) -> Option<Arc<Task>> {
        let map = self.map.read().unwrap();
        let process_pid = map.get(&tid)?.process_pid();
        map.get(&process_pid).cloned()
    }
    // AGENT: resolve process-group membership through ProcessGroup instead of
    // scanning stale per-process pgid fields; liveness is a caller decision.
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
    // AGENT: register publishes a freshly-created process exactly once, keeping
    // the task table, pid, process group, session, and main-thread list in sync.
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
    // AGENT: reap only deletes processes that have already reached zombie state;
    // exit_task owns resource teardown, while reap unlinks the remaining table
    // and parent/group bookkeeping.
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
    // AGENT: share fresh-process identity registration between standalone
    // spawn and the pid-1 init path.
    fn insert_new_process(&self, id: usize, tag: &str) -> Result<Arc<Task>, &'static str> {
        let t = Task::make(id, tag);
        self.register(&t, Pid(id))?;
        Ok(t)
    }
    // AGENT: reserve one task-table slot while a creator builds and registers a
    // task, closing the old check-then-insert race for every process creator.
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
    // AGENT: keep fork readable by separating process-wide inherited state from
    // the caller thread context that receives the child-side zero return value.
    pub fn fork_task(&self, src: &Arc<Task>, pool: &FramePool) -> Result<Arc<Task>, &'static str> {
        let task_slot = self.reserve_task_slot()?;
        let proc_src = self.process_of_tid(src.id()).unwrap_or_else(|| src.clone());
        let child_id = self.seq.fetch_add(1, Ordering::SeqCst);
        let tag = proc_src.tag();
        let child_addr_space = {
            let parent_addr_space = proc_src.process.addr_space.lock().unwrap();
            let forked_addr_space = AddrSpace::fork_from(&parent_addr_space, pool)?;
            Arc::new(Mutex::new(forked_addr_space))
        };
        let child = Task::make_with_addr_space(child_id, &tag, child_addr_space);

        let debug_fds = proc_src.process.debug_fds.lock().unwrap().clone();
        let cwd = proc_src.process.cwd.lock().unwrap().clone();
        let exec_path = proc_src.process.exec_path.lock().unwrap().clone();
        *child.process.debug_fds.lock().unwrap() = debug_fds;
        *child.process.cwd.lock().unwrap() = cwd;
        *child.process.exec_path.lock().unwrap() = exec_path;

        // AGENT: fork copies both the fd entries and the free-fd allocator
        // snapshot so child allocation remains consistent with inherited fds.
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

        // AGENT: fork inherits the parent's process group and session, while
        // the child starts with a fresh pre-exec setpgid window.
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

        // AGENT: child inherits signal dispositions, but not pending signals.
        let sig_state = { proc_src.process.sig_state.lock().unwrap().fork_copy() };
        *child.process.sig_state.lock().unwrap() = sig_state;
        {
            let parent_policy = src.sched.lock().unwrap().policy.clone();
            let mut child_sched = child.sched.lock().unwrap();
            child_sched.policy = parent_policy;
            child_sched.slice_left = child_sched.policy.time_slice();
        }
        self.register(&child, Pid(child_id))?;
        *child.process.parent.lock().unwrap() = Some(proc_src.clone());
        proc_src
            .process
            .subtasks
            .lock()
            .unwrap()
            .push(child.clone());
        task_slot.release();
        Ok(child)
    }
    // AGENT: clone a schedulable thread from the caller's saved user context
    // while sharing the owning process state and address space.
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
        let t = Task::make_with_process(id, &proc_src.tag(), proc_src.process.clone());
        let mut ctx = src.thd_ctx.lock().unwrap().clone().ok_or("enoctx")?;
        ctx.uctx.set_ret(0);
        ctx.uctx.set_sp(stack_top);
        ctx.uctx.set_tls(tls);
        ctx.clear_tid = clear_tid;
        let caller_mask = *src.sig_mask.lock().unwrap();
        ctx.smask = caller_mask;
        *t.sig_mask.lock().unwrap() = caller_mask;
        *t.thd_ctx.lock().unwrap() = Some(ctx);
        {
            let mut map = self.map.write().unwrap();
            if map.contains_key(&id) {
                return Err("eexist");
            }
            map.insert(id, t.clone());
        }
        proc_src.process.threads.lock().unwrap().push(id);
        task_slot.release();
        Ok(t)
    }
    pub fn new_user_task(
        &self,
        path: &str,
        elf_data: &[u8],
        args: Vec<String>,
        envs: Vec<String>,
        pool: &FramePool,
    ) -> Result<Arc<Task>, &'static str> {
        let mut image = prepare_initial_user_image(elf_data, args, envs, pool)?;
        let t = match self.spawn(path) {
            Ok(task) => task,
            Err(err) => {
                image.addr_space.release_all_pages(pool);
                return Err(err);
            }
        };

        *t.process.exec_path.lock().unwrap() = path.to_string();
        *t.thd_ctx.lock().unwrap() = Some(image.thd_ctx);
        {
            let mut addr_space = t.process.addr_space.lock().unwrap();
            *addr_space = image.addr_space;
        }
        install_initial_stdio(&t)?;
        // AGENT: spawn() already registered pid/pgid/sid membership and the
        // main thread for this standalone user process.
        Ok(t)
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

    // AGENT: report one live task-table id per zombie process, not one per
    // thread, so reclaim calls reap exactly once for a dead thread group.
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

// AGENT: release a provisional task-table reservation automatically if task
// creation exits early.
struct TaskSlotReservation<'a> {
    table: &'a TaskTable,
    active: bool,
}

impl TaskSlotReservation<'_> {
    fn release(mut self) {
        self.release_inner();
    }

    // AGENT: return one provisional task-table slot to the shared reservation
    // counter exactly once.
    fn release_inner(&mut self) {
        if self.active {
            self.active = false;
            self.table.task_reservations.fetch_sub(1, Ordering::SeqCst);
        }
    }
}

impl Drop for TaskSlotReservation<'_> {
    fn drop(&mut self) {
        self.release_inner();
    }
}

pub fn yield_now_sync() {
    thread::yield_now();
}
