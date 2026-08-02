// AGENT: define the schedulable Task together with construction, identity, and
// kernel-context access; frame and lifecycle methods live in focused modules.
use super::*;
use crate::context::{KernelContext, KernelContextCell};
use crate::trap::TrapFrame;

// AGENT: represent one schedulable thread with an immutable id and link it to
// a first-class Process; executable identity lives in Process::exec_path.
pub struct Task {
    id: usize,
    pub process: Arc<Process>,
    pub sig_mask: Mutex<u64>,
    pub sig_frames: Mutex<Vec<SigFrame>>,
    // AGENT: retain the userspace clear-on-exit word independently per thread;
    // zero means no CLONE_CHILD_CLEARTID/set_tid_address registration.
    clear_child_tid: AtomicUsize,
    // AGENT: store the RV64 robust_list_head pointer per thread; zero means no
    // robust futex list was registered through set_robust_list.
    robust_list_head: AtomicUsize,
    pub kstk: Mutex<Option<KStk>>,
    // AGENT: keep the switch context at a stable address outside every lock so
    // the single-hart scheduler never carries a MutexGuard across __switch.
    kernel_context: KernelContextCell,
    pub sched: Mutex<SchedEntity>,
}

// AGENT: keep construction and identity/kernel-context methods with the Task
// definition; descriptor, frame, signal, and lifecycle behavior live elsewhere.
impl Task {
    // AGENT: construct only thread-local state around an already-created
    // Process so task construction never implicitly invents process identity.
    pub(super) fn make(
        id: usize,
        process: Arc<Process>,
        pool: &FramePool,
    ) -> Result<Arc<Self>, &'static str> {
        let kstk = KStk::new(pool)?;
        let kernel_context = KernelContext::for_new_task(kstk.top())?;
        let task = Arc::new(Self {
            id,
            process,
            sig_mask: Mutex::new(0),
            sig_frames: Mutex::new(Vec::new()),
            clear_child_tid: AtomicUsize::new(0),
            robust_list_head: AtomicUsize::new(0),
            kstk: Mutex::new(Some(kstk)),
            kernel_context: KernelContextCell::new(kernel_context),
            sched: Mutex::new(SchedEntity::new()),
        });
        task.install_user_trap_frame(TrapFrame::new())?;
        Ok(task)
    }

    // AGENT: derive caller-thread state for a forked process leader or cloned
    // thread around the Process selected by TaskTable; identity and publication
    // remain outside Task.
    pub(super) fn make_child_from_frame(
        id: usize,
        process: Arc<Process>,
        parent: &Task,
        mut child_frame: TrapFrame,
        pool: &FramePool,
    ) -> Result<Arc<Self>, &'static str> {
        let task = Self::make(id, process, pool)?;
        *task.sig_frames.lock().unwrap() = parent.sig_frames.lock().unwrap().clone();
        *task.sig_mask.lock().unwrap() = *parent.sig_mask.lock().unwrap();

        child_frame.set_return_value(0);
        task.install_user_trap_frame(child_frame)?;

        let parent_policy = parent.sched.lock().unwrap().policy.clone();
        let mut child_sched = task.sched.lock().unwrap();
        child_sched.policy = parent_policy;
        child_sched.slice_left = child_sched.policy.time_slice();
        drop(child_sched);
        Ok(task)
    }

    // AGENT: expose the schedulable thread id.
    pub fn id(&self) -> usize {
        self.id
    }

    // AGENT: install one thread-owned clear_child_tid address and return the
    // caller's TID as required by set_tid_address(2).
    pub(crate) fn set_clear_child_tid(&self, addr: usize) -> usize {
        self.clear_child_tid.store(addr, Ordering::Release);
        self.id
    }

    // AGENT: consume the registration exactly once before process address-space
    // teardown, so repeated retirement cannot write or wake the futex twice.
    pub(crate) fn take_clear_child_tid(&self) -> usize {
        self.clear_child_tid.swap(0, Ordering::AcqRel)
    }

    // AGENT: replace one thread's robust list registration after the syscall
    // layer validates the fixed RV64 robust_list_head size.
    pub(crate) fn set_robust_list_head(&self, addr: usize) {
        self.robust_list_head.store(addr, Ordering::Release);
    }

    // AGENT: consume the robust list exactly once during thread retirement.
    pub(crate) fn take_robust_list_head(&self) -> usize {
        self.robust_list_head.swap(0, Ordering::AcqRel)
    }

    // AGENT: expose only the kernel stack top needed by trap setup.
    pub fn kernel_stack_top(&self) -> Option<usize> {
        self.kstk.lock().unwrap().as_ref().map(KStk::top)
    }

    // AGENT: give the scheduler direct access to this stable context cell so no
    // lock guard can accidentally survive while execution runs on another task.
    // Safety: the caller must uphold the single-hart rules documented on Task.
    pub(crate) fn kernel_context_ptr(&self) -> *mut KernelContext {
        self.kernel_context.get()
    }

    // AGENT: replace an off-CPU task's first kernel entry only for focused QEMU
    // scheduler or wait round-trip tests.
    #[cfg(any(test, feature = "qemu-sched-selftest", feature = "qemu-sync-selftest"))]
    pub(crate) fn install_test_kernel_entry(
        &self,
        entry: extern "C" fn() -> !,
    ) -> Result<(), &'static str> {
        let stack_top = self.kernel_stack_top().ok_or("ekstk")?;
        let context = KernelContext::for_test_task(stack_top, entry)?;
        unsafe {
            self.kernel_context_ptr().write(context);
        }
        Ok(())
    }
}

// AGENT: keep Task debug output compact while deriving executable identity from
// the authoritative process-wide exec_path instead of a duplicated task tag.
impl fmt::Debug for Task {
    // AGENT: render the schedulable id and the current executable path, if set.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let exec_path = self.process.exec_path.lock().unwrap();
        let mut debug = f.debug_struct("T");
        debug.field("id", &self.id);
        if !exec_path.is_empty() {
            debug.field("exec_path", &*exec_path);
        }
        debug.finish()
    }
}
