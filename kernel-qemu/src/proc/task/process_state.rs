// AGENT: keep process-wide shared resources separate from schedulable Task state.
use super::fd::FdTable;
use super::*;

// AGENT: own resources shared by every thread in one process, including the fd
// allocator, address space, signal state, and family links.
pub struct ProcessState {
    pub parent: Mutex<Option<Arc<Task>>>,
    pub subtasks: Mutex<Vec<Arc<Task>>>,
    pub(super) fd_table: Mutex<FdTable>,
    pub exec_path: Mutex<String>,
    // AGENT: distinguish process futex words by waiter address in one bucket.
    pub futex: Arc<FutexBucket>,
    pub pid: Mutex<usize>,
    // AGENT: close the parent's setpgid window after the child execs.
    pub did_exec: AtomicBool,
    // AGENT: keep process-wide job-control stop separate from run-queue state.
    pub job_stopped: AtomicBool,
    pub threads: Mutex<Vec<Tid>>,
    pub ev: Mutex<EvBus>,
    pub exit_reason: Mutex<Option<ExitReason>>,
    pub sig_queue: Mutex<VecDeque<(i32, isize)>>,
    pub sig_state: Mutex<SigSet>,
    pub addr_space: Mutex<AddrSpace>,
}

// AGENT: initialize and tear down process-owned shared state in one place.
impl ProcessState {
    // AGENT: initialize process state around the supplied address space.
    pub(super) fn new(addr_space: AddrSpace) -> Self {
        Self {
            parent: Mutex::new(None),
            subtasks: Mutex::new(Vec::new()),
            fd_table: Mutex::new(FdTable::default()),
            exec_path: Mutex::new(String::new()),
            futex: Arc::new(FutexBucket::new()),
            pid: Mutex::new(UNREGISTERED_PID),
            did_exec: AtomicBool::new(false),
            job_stopped: AtomicBool::new(false),
            threads: Mutex::new(Vec::new()),
            ev: Mutex::new(EvBus::default()),
            exit_reason: Mutex::new(None),
            sig_queue: Mutex::new(VecDeque::new()),
            sig_state: Mutex::new(SigSet::new()),
            addr_space: Mutex::new(addr_space),
        }
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
            take_mutex_default(&self.fd_table),
            take_mutex_default(&self.sig_queue),
            replace_mutex_value(&self.sig_state, SigSet::new()),
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
