// AGENT: define the schedulable Task and its task-local lifecycle behavior
// separately from the state types it composes.
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

// AGENT: prove the fixed top-of-stack TrapFrame fits both its owning kernel
// stack and the single page rebound through the TRAP_CONTEXT alias.
const _: () = {
    assert!(mem::size_of::<TrapFrame>() <= KSTK_SZ);
    assert!(mem::size_of::<TrapFrame>() <= PAGE_SZ);
    assert!(PAGE_SZ % mem::align_of::<TrapFrame>() == 0);
};

// AGENT: keep task methods ordered by construction, identity/context, user
// TrapFrame access, scheduling, and exit teardown; descriptor-specific methods
// live in task/fd.rs.
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

    // AGENT: run one operation against a live kernel stack while keeping its
    // ownership guard held for the complete operation.
    fn with_kstk<R>(&self, f: impl FnOnce(&KStk) -> R) -> Result<R, &'static str> {
        let kstk = self.kstk.lock().unwrap();
        Ok(f(kstk.as_ref().ok_or("ekstk")?))
    }

    // AGENT: derive the fixed trap-frame slot from the statically checked stack
    // and frame layout while the caller keeps the owning kstk guard held.
    fn trap_frame_ptr_in(kstk: &KStk) -> *mut TrapFrame {
        (kstk.top() - mem::size_of::<TrapFrame>()) as *mut TrapFrame
    }

    // AGENT: locate the architecture frame trap.S owns at the fixed top slot of
    // this task's kernel stack. Callers must not create a second mutable access
    // while the live trap path already holds &mut TrapFrame for the same slot.
    pub(crate) fn user_trap_frame_ptr(&self) -> Result<*mut TrapFrame, &'static str> {
        self.with_kstk(Self::trap_frame_ptr_in)
    }

    // AGENT: return the physical page backing the authoritative TrapFrame so
    // CPU0 can rebind the fixed supervisor-only TRAP_CONTEXT alias before sret.
    pub(crate) fn user_trap_frame_page_paddr(&self) -> Result<usize, &'static str> {
        self.with_kstk(KStk::top_page_paddr)
    }

    // AGENT: initialize or replace an off-CPU task's complete user return frame.
    pub fn install_user_trap_frame(&self, frame: TrapFrame) -> Result<(), &'static str> {
        self.with_kstk(|kstk| unsafe {
            Self::trap_frame_ptr_in(kstk).write(frame);
        })
    }

    // AGENT: clone an off-CPU task's complete user return frame for fork,
    // checkpoint, tests, or scheduler-side signal delivery.
    pub fn snapshot_user_trap_frame(&self) -> Result<TrapFrame, &'static str> {
        self.with_kstk(|kstk| unsafe { (&*Self::trap_frame_ptr_in(kstk)).clone() })
    }

    // AGENT: read this task's scheduler placement state.
    pub fn sched_state(&self) -> TaskRunState {
        self.sched.lock().unwrap().state
    }

    // AGENT: update this task's scheduler placement state.
    pub fn set_sched_state(&self, state: TaskRunState) {
        self.sched.lock().unwrap().state = state;
    }

    // AGENT: publish the token and Sleeping state atomically at the scheduler's
    // final lost-wakeup check; only the currently Running owner may install it.
    pub(crate) fn install_active_wait(&self, token: WaitToken) -> bool {
        let mut sched = self.sched.lock().unwrap();
        if sched.state != TaskRunState::Running || sched.active_wait.is_some() {
            return false;
        }
        sched.active_wait = Some(token);
        sched.state = TaskRunState::Sleeping;
        true
    }

    // AGENT: clear one matching active wait after its kernel stack resumes; a
    // wake path may already have cleared it while making the task runnable.
    pub(crate) fn clear_active_wait(&self, token: &WaitToken) -> bool {
        let mut sched = self.sched.lock().unwrap();
        if sched
            .active_wait
            .as_ref()
            .is_none_or(|active| !active.same(token))
        {
            return false;
        }
        sched.active_wait = None;
        if sched.state == TaskRunState::Sleeping {
            sched.state = TaskRunState::Runnable;
        }
        true
    }

    // AGENT: atomically detach the wait that justified Sleeping and publish the
    // runnable state before the kernel enqueues this task.
    pub(crate) fn wake_active_wait(&self) -> bool {
        let mut sched = self.sched.lock().unwrap();
        if sched.state != TaskRunState::Sleeping || sched.active_wait.is_none() {
            return false;
        }
        sched.active_wait = None;
        sched.state = TaskRunState::Runnable;
        true
    }

    // AGENT: snapshot and cancel the concrete blocking point without retaining
    // the sched lock while WaitToken wakes through the global kernel backend.
    pub(crate) fn cancel_active_wait_for_group_exit(&self) -> bool {
        let active = self.sched.lock().unwrap().active_wait.clone();
        active.is_some_and(|token| token.cancel_for_group_exit())
    }

    // AGENT: expose whether a focused lifecycle test or exit path still has a
    // registered blocking point without leaking the token itself.
    pub(crate) fn has_active_wait(&self) -> bool {
        self.sched.lock().unwrap().active_wait.is_some()
    }

    // AGENT: report only this thread's terminal scheduler state; one sibling's
    // SYS_EXIT must never make every Task in the Process appear dead.
    pub fn done(&self) -> bool {
        self.sched_state() == TaskRunState::Zombie
    }

    // AGENT: clone the task-owned scheduling policy for queue operations.
    pub fn sched_policy(&self) -> SchedulePolicy {
        self.sched.lock().unwrap().policy.clone()
    }

    // AGENT: update the task-owned priority in place so every queued reference
    // observes the change without copying or returning a policy snapshot.
    pub fn boost_priority(&self, amount: i32) {
        let mut sched = self.sched.lock().unwrap();
        let amount = amount.max(0);
        let prio = sched.policy.prio.saturating_sub(amount);
        sched.policy = SchedulePolicy::with_prio(prio);
        sched.slice_left = sched.slice_left.min(sched.policy.time_slice());
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

    // AGENT: publish exit state, release saved signal-frame backing storage, and
    // retain a live kernel stack only until CPU0 switches back to idle.
    pub(crate) fn mark_thread_exited(&self) {
        debug_assert!(
            !self.has_active_wait(),
            "thread exited before its active wait stack cleaned up"
        );
        *self.sig_mask.lock().unwrap() = 0;
        let old_sig_frames = {
            let mut sig_frames = self.sig_frames.lock().unwrap();
            mem::take(&mut *sig_frames)
        };
        drop(old_sig_frames);
        self.set_sched_state(TaskRunState::Zombie);
    }

    // AGENT: release an exited task's stack only after __switch has returned to
    // the idle stack; dropping the currently executing stack is never allowed.
    pub(crate) fn release_kernel_stack(&self) {
        self.kstk.lock().unwrap().take();
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
