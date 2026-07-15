// AGENT: define task/process state and task-local lifecycle behavior separately
// from descriptor management and the global task registry.
use super::*;

// AGENT: keep process identifiers as a small typed wrapper shared by task code.
#[derive(Clone)]
pub struct Pid(pub usize);

// AGENT: centralize pid construction and init-process checks.
impl Pid {
    pub const INIT: usize = 1;

    // AGENT: construct the unregistered pid sentinel used by fresh processes.
    pub fn new() -> Self {
        Pid(0)
    }

    // AGENT: expose the numeric pid at process-table boundaries.
    pub fn get(&self) -> usize {
        self.0
    }

    // AGENT: identify the distinguished init process without duplicating pid 1.
    pub fn is_init(&self) -> bool {
        self.0 == Self::INIT
    }
}

// AGENT: format pids using their numeric userspace representation.
impl fmt::Display for Pid {
    // AGENT: delegate pid display to the wrapped integer.
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

// AGENT: store scheduler-visible task identity and its diagnostic tag together.
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

// AGENT: group the mutable scheduler placement, policy, and remaining slice.
pub struct SchedEntity {
    pub state: TaskRunState,
    pub policy: SchedulePolicy,
    pub slice_left: usize,
}

// AGENT: initialize scheduler state from one canonical scheduling policy.
impl SchedEntity {
    // AGENT: initialize the runtime countdown from the priority-derived slice.
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

// AGENT: own resources shared by every thread in one process, including the fd
// allocator, address space, signal state, IPC contexts, and family links.
pub struct ProcessState {
    // AGENT: debug-only descriptor names used by smoke tests; real descriptors
    // live in the files table below.
    pub debug_fds: Mutex<Vec<String>>,
    pub parent: Mutex<Option<Arc<Task>>>,
    pub subtasks: Mutex<Vec<Arc<Task>>>,
    pub files: Mutex<BTreeMap<usize, FdEntry>>,
    pub free_fds: Mutex<BTreeSet<usize>>,
    pub cwd: Mutex<String>,
    pub exec_path: Mutex<String>,
    // AGENT: distinguish process futex words by waiter address in one bucket.
    pub futex: Arc<FutexBucket>,
    pub sem_ctx: Mutex<SemCtx>,
    pub shm_ctx: Mutex<ShmCtx>,
    pub pid: Mutex<Pid>,
    pub pgid: Mutex<Pgid>,
    // AGENT: retain the session id used by setpgid/setsid validation.
    pub sid: Mutex<usize>,
    // AGENT: close the parent's setpgid window after the child execs.
    pub did_exec: AtomicBool,
    // AGENT: keep process-wide job-control stop separate from run-queue state.
    pub job_stopped: AtomicBool,
    pub threads: Mutex<Vec<Tid>>,
    pub ev: Arc<Mutex<EvBus>>,
    pub exit_reason: Mutex<Option<ExitReason>>,
    pub sig_queue: Mutex<VecDeque<(i32, isize)>>,
    pub sig_state: Mutex<SigSet>,
    pub addr_space: Arc<Mutex<AddrSpace>>,
}

// AGENT: initialize and tear down process-owned shared state in one place.
impl ProcessState {
    // AGENT: start every process with all descriptor numbers available.
    pub(super) fn initial_free_fds() -> BTreeSet<usize> {
        let mut free_fds = BTreeSet::new();
        for fd in 0..MAX_FD {
            free_fds.insert(fd);
        }
        free_fds
    }

    // AGENT: initialize process state around the supplied address space.
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

    // AGENT: allocate a fresh process with an empty address space.
    pub fn new_shared() -> Arc<Self> {
        Arc::new(Self::new(Arc::new(Mutex::new(AddrSpace::new()))))
    }

    // AGENT: allocate a fresh process around a prepared or forked address space.
    pub fn new_with_addr_space(addr_space: Arc<Mutex<AddrSpace>>) -> Arc<Self> {
        Arc::new(Self::new(addr_space))
    }

    // AGENT: move droppable resources out of locks before process teardown and
    // rely on address-space RAII rather than forwarding an unused frame pool.
    pub fn release_exit_resources(&self) -> usize {
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
        let released_pages = self.addr_space.lock().unwrap().release_all_pages();
        drop(old_resources);
        released_pages
    }
}

// AGENT: move a defaultable resource out of a mutex so Drop runs unlocked.
fn take_mutex_default<T: Default>(slot: &Mutex<T>) -> T {
    let mut guard = slot.lock().unwrap();
    mem::take(&mut *guard)
}

// AGENT: replace a non-Default mutex value and drop the old value unlocked.
fn replace_mutex_value<T>(slot: &Mutex<T>, value: T) -> T {
    let mut guard = slot.lock().unwrap();
    mem::replace(&mut *guard, value)
}

// AGENT: retain the Linux-compatible distinction between normal and signaled exit.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExitReason {
    Code(u8),
    Signal(u8),
}

// AGENT: translate internal exit reasons into wait-compatible status words.
impl ExitReason {
    // AGENT: encode normal exit codes and terminating signals for wait syscalls.
    pub fn wait_status(self) -> usize {
        match self {
            ExitReason::Code(code) => (code as usize) << 8,
            ExitReason::Signal(sig) => (sig as usize) & 0x7f,
        }
    }
}

// AGENT: keep thread-private user context and signal-frame state together.
#[derive(Clone)]
pub struct ThdCtx {
    pub uctx: Context,
    pub clear_tid: usize,
    pub smask: u64,
    // AGENT: stack interrupted contexts while simulated signal handlers run.
    pub sig_frames: Vec<SigFrame>,
}

// AGENT: construct the initial blank user context for a new schedulable task.
impl Default for ThdCtx {
    // AGENT: initialize thread-local context, masks, and signal frames.
    fn default() -> Self {
        Self {
            uctx: Context::new(),
            clear_tid: 0,
            smask: 0,
            sig_frames: Vec::new(),
        }
    }
}

// AGENT: represent one schedulable thread and link it to process-wide state.
pub struct Task {
    pub info: Mutex<TaskInfo>,
    pub process: Arc<ProcessState>,
    pub sig_mask: Mutex<u64>,
    pub kstk: Mutex<Option<KStk>>,
    pub thd_ctx: Mutex<Option<ThdCtx>>,
    pub restored_trap_frame: Mutex<Option<SavedTrapFrame>>,
    pub sched: Mutex<SchedEntity>,
}

// AGENT: implement task identity, scheduling, exit teardown, and signal queues;
// descriptor-specific methods live in task/fd.rs.
impl Task {
    // AGENT: construct a standalone task with a fresh process and address space.
    pub fn make(id: usize, tag: &str) -> Arc<Self> {
        Self::make_with_process(id, tag, ProcessState::new_shared())
    }

    // AGENT: construct a new process task around a prepared address space.
    pub(super) fn make_with_addr_space(
        id: usize,
        tag: &str,
        addr_space: Arc<Mutex<AddrSpace>>,
    ) -> Arc<Self> {
        Self::make_with_process(id, tag, ProcessState::new_with_addr_space(addr_space))
    }

    // AGENT: give every schedulable task a kernel stack and thread context.
    pub(super) fn make_with_process(id: usize, tag: &str, process: Arc<ProcessState>) -> Arc<Self> {
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

    // AGENT: expose the schedulable thread id.
    pub fn id(&self) -> usize {
        self.info.lock().unwrap().id
    }

    // AGENT: report the shared address-space switch token for trap handling.
    pub fn vm_token(&self) -> Result<usize, &'static str> {
        self.process.addr_space.lock().unwrap().vm_token()
    }

    // AGENT: clone the diagnostic task tag without leaking the info lock.
    pub fn tag(&self) -> String {
        self.info.lock().unwrap().tag.clone()
    }

    // AGENT: expose the owning process id separately from the schedulable id.
    pub fn process_pid(&self) -> usize {
        self.process.pid.lock().unwrap().get()
    }

    // AGENT: expose session identity for process-group checks.
    pub fn process_sid(&self) -> usize {
        *self.process.sid.lock().unwrap()
    }

    // AGENT: identify session leaders for setpgid restrictions.
    pub fn is_session_leader(&self) -> bool {
        self.process_sid() == self.process_pid()
    }

    // AGENT: expose only the kernel stack top needed by trap setup.
    pub fn kernel_stack_top(&self) -> Option<usize> {
        self.kstk.lock().unwrap().as_ref().map(KStk::top)
    }

    // AGENT: store a full checkpoint frame for the first restored user entry.
    pub fn set_restored_trap_frame(&self, frame: SavedTrapFrame) {
        *self.restored_trap_frame.lock().unwrap() = Some(frame);
    }

    // AGENT: consume a restored frame once it is materialized on the kernel stack.
    pub fn take_restored_trap_frame(&self) -> Option<SavedTrapFrame> {
        self.restored_trap_frame.lock().unwrap().take()
    }

    // AGENT: link a process to its parent representative.
    pub fn link_parent(&self, parent: &Arc<Task>) {
        *self.process.parent.lock().unwrap() = Some(parent.clone());
    }

    // AGENT: link a child process representative into this process's child list.
    pub fn link_child(&self, child: &Arc<Task>) {
        self.process.subtasks.lock().unwrap().push(child.clone());
    }

    // AGENT: report process death from the shared exit reason.
    pub fn done(&self) -> bool {
        self.process.exit_reason.lock().unwrap().is_some()
    }

    // AGENT: report the current number of child processes.
    pub fn n_children(&self) -> usize {
        self.process.subtasks.lock().unwrap().len()
    }

    // AGENT: read this task's scheduler placement state.
    pub fn sched_state(&self) -> TaskRunState {
        self.sched.lock().unwrap().state
    }

    // AGENT: update this task's scheduler placement state.
    pub fn set_sched_state(&self, state: TaskRunState) {
        self.sched.lock().unwrap().state = state;
    }

    // AGENT: expose process job-control stop without overloading run-queue state.
    pub fn is_job_stopped(&self) -> bool {
        self.process.job_stopped.load(Ordering::Relaxed)
    }

    // AGENT: update process job-control stop independently of scheduler placement.
    pub fn set_job_stopped(&self, stopped: bool) {
        self.process.job_stopped.store(stopped, Ordering::Relaxed);
    }

    // AGENT: clone the task-owned scheduling policy for queue operations.
    pub fn sched_policy(&self) -> SchedulePolicy {
        self.sched.lock().unwrap().policy.clone()
    }

    // AGENT: update task priority so boosts survive later requeue transitions.
    pub fn boost_priority(&self, amount: i32) -> SchedulePolicy {
        let mut sched = self.sched.lock().unwrap();
        let amount = amount.max(0);
        let prio = sched.policy.prio.saturating_sub(amount);
        sched.policy = SchedulePolicy::with_prio(prio);
        sched.slice_left = sched.slice_left.min(sched.policy.time_slice());
        sched.policy.clone()
    }

    // AGENT: reset the runtime slice from the current priority-derived policy.
    pub fn reset_slice(&self) {
        let mut sched = self.sched.lock().unwrap();
        sched.slice_left = sched.policy.time_slice();
    }

    // AGENT: consume one scheduler tick and report slice exhaustion.
    pub fn tick_slice(&self) -> bool {
        let mut sched = self.sched.lock().unwrap();
        if sched.slice_left > 0 {
            sched.slice_left -= 1;
        }
        sched.slice_left == 0
    }

    // AGENT: expose the process futex bucket to synchronization syscalls.
    pub fn get_futex(&self) -> Arc<FutexBucket> {
        self.process.futex.clone()
    }

    // AGENT: record process death once and notify process/parent waiters.
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

    // AGENT: release per-process resources that no later wait status needs
    // without threading an unused allocator argument through the task layer.
    pub fn release_process_exit_resources(&self) -> usize {
        self.process.release_exit_resources()
    }

    // AGENT: clear CLONE_CHILD_CLEARTID before waking one futex waiter.
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

    // AGENT: drop thread-private execution resources after clear-child-tid cleanup.
    pub fn release_thread_exit_resources(&self, pool: &FramePool) {
        *self.sig_mask.lock().unwrap() = 0;
        self.kstk.lock().unwrap().take();
        let old_ctx = self.thd_ctx.lock().unwrap().take();
        if let Some(ctx) = old_ctx {
            self.clear_child_tid_and_wake(ctx.clear_tid, pool);
        }
        self.set_sched_state(TaskRunState::Zombie);
    }

    // AGENT: expose the encoded process exit status to wait paths.
    pub fn wait_status(&self) -> usize {
        match *self.process.exit_reason.lock().unwrap() {
            Some(reason) => reason.wait_status(),
            None => 0,
        }
    }

    // AGENT: enqueue a non-duplicated standard pending signal for this process.
    pub(crate) fn enqueue_signal(&self, signo: i32, sender_tid: isize) -> bool {
        if signo <= 0 || signo as u32 >= NSIG {
            return false;
        }
        let mut sq = self.process.sig_queue.lock().unwrap();
        if sq.iter().any(|(sig, _)| *sig == signo) {
            return false;
        }
        sq.push_back((signo, sender_tid));
        drop(sq);
        self.process.ev.lock().unwrap().set(EvFlag::RECV_SIG);
        true
    }

    // AGENT: restore a signal that could not be delivered without user context.
    pub(crate) fn requeue_signal_front(&self, signo: i32, sender_tid: isize) {
        self.process
            .sig_queue
            .lock()
            .unwrap()
            .push_front((signo, sender_tid));
        self.process.ev.lock().unwrap().set(EvFlag::RECV_SIG);
    }

    // AGENT: detect pending signals that should interrupt a blocking syscall.
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

    // AGENT: select the first unblocked non-ignored pending signal for delivery.
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
}

// AGENT: keep Task debug output compact and independent of process resources.
impl fmt::Debug for Task {
    // AGENT: render only the schedulable id and diagnostic tag.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let info = self.info.lock().unwrap();
        f.debug_struct("T")
            .field("id", &info.id)
            .field("tag", &info.tag)
            .finish()
    }
}
