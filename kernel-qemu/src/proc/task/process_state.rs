// AGENT: keep process-wide shared resources separate from schedulable Task state.
use super::*;

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

    // AGENT: update one process-wide disposition while holding the pending
    // queue first, then discard an already-pending signal when its new
    // disposition ignores it, even if a thread currently blocks that signal.
    pub fn set_signal_action(&self, signo: u32, action: SigAction) -> bool {
        let mut sig_queue = self.sig_queue.lock().unwrap();
        let should_discard = action.resolve(signo) == SignalDeliveryAction::Ignore;
        let mut sig_state = self.sig_state.lock().unwrap();
        if !sig_state.set_action(signo, action) {
            return false;
        }
        drop(sig_state);
        if should_discard {
            sig_queue.retain(|(pending, _)| *pending != signo as i32);
        }
        true
    }

    // AGENT: move droppable resources out of locks before process teardown and
    // let address-space RAII reclaim frames without forwarding a bogus count.
    pub fn release_exit_resources(&self) {
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
        self.addr_space.lock().unwrap().release_all_pages();
        drop(old_resources);
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
