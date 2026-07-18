// AGENT: isolate process identity, family links, thread membership, and
// process-wide scheduling state from lifecycle and image-building behavior.
use super::super::task::fd::FdTable;
use super::*;

// AGENT: represent one process independently from any schedulable thread;
// Task owns thread-local execution state and points at this shared entity.
pub struct Process {
    pid: usize,
    parent: Mutex<Option<Weak<Process>>>,
    children: Mutex<BTreeMap<usize, Arc<Process>>>,
    pub(in crate::kernel::proc) fd_table: Mutex<FdTable>,
    pub exec_path: Mutex<String>,
    // AGENT: distinguish process futex words by waiter address in one bucket.
    pub futex: Arc<FutexBucket>,
    // AGENT: close the parent's setpgid window after the child execs.
    pub did_exec: AtomicBool,
    // AGENT: keep process-wide job-control stop separate from run-queue state.
    job_stopped: AtomicBool,
    threads: Mutex<BTreeSet<Tid>>,
    pub ev: Mutex<EvBus>,
    pub(super) exit_reason: Mutex<Option<ExitReason>>,
    pub sig_queue: Mutex<VecDeque<(i32, isize)>>,
    pub sig_state: Mutex<SigSet>,
    pub addr_space: Mutex<AddrSpace>,
}

// AGENT: own process identity, family links, shared resources, and thread
// membership without depending on one representative Task.
impl Process {
    // AGENT: construct a registered-identity process before creating its leader
    // thread, so Process never passes through an invalid placeholder pid.
    pub(in crate::kernel::proc) fn new(pid: usize, addr_space: AddrSpace) -> Self {
        Self {
            pid,
            parent: Mutex::new(None),
            children: Mutex::new(BTreeMap::new()),
            fd_table: Mutex::new(FdTable::default()),
            exec_path: Mutex::new(String::new()),
            futex: Arc::new(FutexBucket::new()),
            did_exec: AtomicBool::new(false),
            job_stopped: AtomicBool::new(false),
            threads: Mutex::new(BTreeSet::new()),
            ev: Mutex::new(EvBus::default()),
            exit_reason: Mutex::new(None),
            sig_queue: Mutex::new(VecDeque::new()),
            sig_state: Mutex::new(SigSet::new()),
            addr_space: Mutex::new(addr_space),
        }
    }

    // AGENT: expose immutable process identity without a registration-state lock.
    pub fn pid(&self) -> usize {
        self.pid
    }

    // AGENT: upgrade the non-owning parent link only while returning a snapshot.
    pub fn parent(&self) -> Option<Arc<Process>> {
        self.parent.lock().unwrap().as_ref().and_then(Weak::upgrade)
    }

    // AGENT: return a stable child-process snapshot for wait and reparent paths.
    pub fn children_snapshot(&self) -> Vec<Arc<Process>> {
        self.children.lock().unwrap().values().cloned().collect()
    }

    // AGENT: report whether no direct child process remains attached.
    pub fn has_no_children(&self) -> bool {
        self.children.lock().unwrap().is_empty()
    }

    // AGENT: install a weak parent link so process family ownership cannot cycle.
    pub(in crate::kernel::proc) fn set_parent(&self, parent: Option<&Arc<Process>>) {
        *self.parent.lock().unwrap() = parent.map(Arc::downgrade);
    }

    // AGENT: retain a child process by pid until wait/reap detaches it.
    pub(in crate::kernel::proc) fn insert_child(&self, child: Arc<Process>) {
        self.children.lock().unwrap().insert(child.pid(), child);
    }

    // AGENT: detach one child during reap without scanning unrelated children.
    pub(in crate::kernel::proc) fn remove_child(&self, pid: usize) -> Option<Arc<Process>> {
        self.children.lock().unwrap().remove(&pid)
    }

    // AGENT: move all children out in pid order before orphan reparenting.
    pub(in crate::kernel::proc) fn take_children(&self) -> Vec<Arc<Process>> {
        mem::take(&mut *self.children.lock().unwrap())
            .into_values()
            .collect()
    }

    // AGENT: add one schedulable thread id without duplicating membership.
    pub(in crate::kernel::proc) fn add_thread(&self, tid: Tid) -> bool {
        self.threads.lock().unwrap().insert(tid)
    }

    // AGENT: snapshot thread membership for process-wide exit cleanup.
    pub fn thread_ids(&self) -> Vec<Tid> {
        self.threads.lock().unwrap().iter().copied().collect()
    }

    // AGENT: expose thread count to checkpoint validation without leaking the set.
    pub fn thread_count(&self) -> usize {
        self.threads.lock().unwrap().len()
    }

    // AGENT: report exact thread membership for focused lifecycle tests.
    pub fn has_thread(&self, tid: Tid) -> bool {
        self.threads.lock().unwrap().contains(&tid)
    }

    // AGENT: drain every thread id exactly once during process reap.
    pub(in crate::kernel::proc) fn take_threads(&self) -> Vec<Tid> {
        mem::take(&mut *self.threads.lock().unwrap())
            .into_iter()
            .collect()
    }

    // AGENT: expose process-wide job-control stop independently of Task state.
    pub fn is_job_stopped(&self) -> bool {
        self.job_stopped.load(Ordering::Relaxed)
    }

    // AGENT: update process-wide job-control stop independently of Task state.
    pub fn set_job_stopped(&self, stopped: bool) {
        self.job_stopped.store(stopped, Ordering::Relaxed);
    }
}
