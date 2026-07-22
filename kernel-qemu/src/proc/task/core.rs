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
    pub kstk: Mutex<Option<KStk>>,
    // AGENT: keep the switch context at a stable address outside every lock so
    // the single-hart scheduler never carries a MutexGuard across __switch.
    kernel_context: KernelContextCell,
    pub sched: Mutex<SchedEntity>,
}

// AGENT: implement task identity, scheduling, exit teardown, and signal queues;
// descriptor-specific methods live in task/fd.rs.
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

    // AGENT: report the shared address-space switch token for trap handling.
    pub fn vm_token(&self) -> Result<usize, &'static str> {
        self.process.addr_space.lock().unwrap().vm_token()
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

    // AGENT: replace an off-CPU task's first kernel entry only for the focused
    // QEMU scheduler round-trip test.
    #[cfg(any(test, feature = "qemu-sched-selftest"))]
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

    // AGENT: compute the fixed trap-frame slot while its owning stack is still
    // protected by the caller's kstk guard.
    fn trap_frame_ptr_in(kstk: &KStk) -> Result<*mut TrapFrame, &'static str> {
        let frame_addr = kstk
            .top()
            .checked_sub(mem::size_of::<TrapFrame>())
            .ok_or("ekstk")?;
        if frame_addr % mem::align_of::<TrapFrame>() != 0 {
            return Err("ekstk");
        }
        Ok(frame_addr as *mut TrapFrame)
    }

    // AGENT: locate the architecture frame trap.S owns at the fixed top slot of
    // this task's kernel stack. Callers must not create a second mutable access
    // while the live trap path already holds &mut TrapFrame for the same slot.
    pub(crate) fn user_trap_frame_ptr(&self) -> Result<*mut TrapFrame, &'static str> {
        let kstk = self.kstk.lock().unwrap();
        Self::trap_frame_ptr_in(kstk.as_ref().ok_or("ekstk")?)
    }

    // AGENT: return the physical page backing the authoritative TrapFrame so
    // CPU0 can rebind the fixed supervisor-only TRAP_CONTEXT alias before sret.
    pub(crate) fn user_trap_frame_page_paddr(&self) -> Result<usize, &'static str> {
        let kstk = self.kstk.lock().unwrap();
        Ok(kstk.as_ref().ok_or("ekstk")?.top_page_paddr())
    }

    // AGENT: initialize or replace an off-CPU task's complete user return frame.
    pub fn install_user_trap_frame(&self, frame: TrapFrame) -> Result<(), &'static str> {
        let kstk = self.kstk.lock().unwrap();
        let ptr = Self::trap_frame_ptr_in(kstk.as_ref().ok_or("ekstk")?)?;
        unsafe {
            ptr.write(frame);
        }
        Ok(())
    }

    // AGENT: clone an off-CPU task's complete user return frame for fork,
    // checkpoint, tests, or scheduler-side signal delivery.
    pub fn snapshot_user_trap_frame(&self) -> Result<TrapFrame, &'static str> {
        let kstk = self.kstk.lock().unwrap();
        let ptr = Self::trap_frame_ptr_in(kstk.as_ref().ok_or("ekstk")?)?;
        Ok(unsafe { (&*ptr).clone() })
    }

    // AGENT: report only this thread's terminal scheduler state; one sibling's
    // SYS_EXIT must never make every Task in the Process appear dead.
    pub fn done(&self) -> bool {
        self.sched_state() == TaskRunState::Zombie
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
        self.process.is_job_stopped()
    }

    // AGENT: update process job-control stop independently of scheduler placement.
    pub fn set_job_stopped(&self, stopped: bool) {
        self.process.set_job_stopped(stopped);
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

    // AGENT: drop the execution resources owned by one task during teardown.
    pub fn release_thread_exit_resources(&self) {
        self.mark_thread_exited();
        self.release_kernel_stack();
    }

    // AGENT: publish exit state, release saved signal-frame backing storage, and
    // retain a live kernel stack only until CPU0 switches back to idle.
    pub(crate) fn mark_thread_exited(&self) {
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
