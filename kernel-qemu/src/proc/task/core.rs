// AGENT: define the schedulable Task and its task-local lifecycle behavior
// separately from the state types it composes.
use super::*;
use crate::trap::TrapFrame;

// AGENT: represent one schedulable thread with an immutable id and link it to
// process-wide state; executable identity lives in ProcessState::exec_path.
pub struct Task {
    id: usize,
    pub process: Arc<ProcessState>,
    pub sig_mask: Mutex<u64>,
    pub kstk: Mutex<Option<KStk>>,
    pub thd_ctx: Mutex<Option<ThdCtx>>,
    pub sched: Mutex<SchedEntity>,
}

// AGENT: implement task identity, scheduling, exit teardown, and signal queues;
// descriptor-specific methods live in task/fd.rs.
impl Task {
    // AGENT: construct a standalone task with a fresh process and a fallible
    // FramePool-backed kernel stack.
    pub fn make(id: usize, pool: &FramePool) -> Result<Arc<Self>, &'static str> {
        Self::make_with_process(id, ProcessState::new_shared(), pool)
    }

    // AGENT: construct a new process task around a prepared address space while
    // allocating its thread-private kernel stack from the shared frame pool.
    pub(super) fn make_with_addr_space(
        id: usize,
        addr_space: Arc<Mutex<AddrSpace>>,
        pool: &FramePool,
    ) -> Result<Arc<Self>, &'static str> {
        Self::make_with_process(id, ProcessState::new_with_addr_space(addr_space), pool)
    }

    // AGENT: give every schedulable task a directly frame-backed kernel stack
    // and propagate physical-memory exhaustion to the task-creation boundary.
    pub(super) fn make_with_process(
        id: usize,
        process: Arc<ProcessState>,
        pool: &FramePool,
    ) -> Result<Arc<Self>, &'static str> {
        let kstk = KStk::new(pool)?;
        let task = Arc::new(Self {
            id,
            process,
            sig_mask: Mutex::new(0),
            kstk: Mutex::new(Some(kstk)),
            thd_ctx: Mutex::new(Some(ThdCtx::default())),
            sched: Mutex::new(SchedEntity::new()),
        });
        task.install_user_trap_frame(TrapFrame::new())?;
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

    // AGENT: report process death from the shared exit reason.
    pub fn done(&self) -> bool {
        self.process.exit_reason.lock().unwrap().is_some()
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
        let sig_state = self.process.sig_state.lock().unwrap();
        if sig_state.is_ignored(signo as u32) {
            return false;
        }
        drop(sig_state);
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
            sig_state.get_action(signo).resolve(signo) != SignalDeliveryAction::Ignore
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
