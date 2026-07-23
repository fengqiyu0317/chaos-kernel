use super::*;
use crate::trap::TrapFrame;

impl Kernel {
    // AGENT: wait-token scheduling is still single-hart, so "current" means the
    // CPU0 task whose kernel stack is actively executing.
    fn is_current_task_on_cpu0(&self, task_id: usize) -> bool {
        self.cur_task(0)
            .map(|current| current.id() == task_id)
            .unwrap_or(false)
    }

    // AGENT: enqueue only runnable tasks that are not held by job-control stop.
    fn enqueue_task_if_ready(&self, task: &Arc<Task>) {
        if task.sched_state() == TaskRunState::Runnable
            && !task.is_job_stopped()
            && !self.is_current_task_on_cpu0(task.id())
        {
            self.run_queue.enqueue(task);
        }
    }

    // AGENT: record that a wait/event completed; job-stopped tasks become
    // runnable in scheduler state but stay off the run queue until SIGCONT.
    fn make_task_runnable(&self, task: &Arc<Task>) {
        task.set_sched_state(TaskRunState::Runnable);
        self.enqueue_task_if_ready(task);
    }

    // AGENT: apply one job-control transition to the whole thread group while
    // keeping each thread's live scheduler state as the resume source of truth.
    fn set_process_job_stopped(&self, task: &Arc<Task>, stopped: bool) {
        task.set_job_stopped(stopped);
        let thread_ids = task.process.thread_ids();
        for thread_id in thread_ids {
            let Some(thread) = self.tasks.find_task(thread_id) else {
                continue;
            };
            if !Arc::ptr_eq(&thread.process, &task.process) {
                continue;
            }
            if stopped {
                self.run_queue.remove(thread_id);
            } else if !thread.done() {
                self.enqueue_task_if_ready(&thread);
            }
        }
    }

    // AGENT: common QEMU wait-token wake path. Event, futex, epoll, and timer
    // wakeups should make a sleeping task runnable through the run queue instead
    // of unparking a host thread.
    pub(crate) fn wake_task_for_wait(&self, task_id: usize) -> bool {
        let Some(task) = self.tasks.find_task(task_id) else {
            return false;
        };
        if task.done() {
            return false;
        }
        if task.sched_state() == TaskRunState::Sleeping {
            self.make_task_runnable(&task);
        }
        true
    }

    // AGENT: common QEMU wait-token block path. Once CPU0 scheduling is active,
    // the current task returns to the idle context after publishing Sleeping;
    // metadata-only selftests retain the earlier no-switch compatibility path.
    pub(crate) fn block_task_for_wait(&self, task_id: usize) -> bool {
        let Some(task) = self.tasks.find_task(task_id) else {
            return false;
        };
        if task.done() {
            return false;
        }
        if task.is_job_stopped() {
            return false;
        }
        if task.sched_state() == TaskRunState::Sleeping {
            return true;
        }
        task.set_sched_state(TaskRunState::Sleeping);
        self.run_queue.remove(task_id);
        if self.is_current_task_on_cpu0(task_id) {
            self.switch_current_to_idle(0);
        }
        true
    }

    // AGENT: normalize a completed wait after either a real idle round trip or
    // the metadata-only compatibility bridge used before scheduler activation.
    pub(crate) fn finish_task_wait(&self, task_id: usize) -> bool {
        let Some(task) = self.tasks.find_task(task_id) else {
            return false;
        };
        if task.done() {
            return false;
        }
        if task.is_job_stopped() {
            return false;
        }
        if self.is_current_task_on_cpu0(task_id) {
            self.run_queue.remove(task_id);
            task.set_sched_state(TaskRunState::Running);
            task.reset_slice();
            return true;
        }

        if task.sched_state() == TaskRunState::Sleeping {
            task.set_sched_state(TaskRunState::Runnable);
            self.enqueue_task_if_ready(&task);
        }
        true
    }

    // AGENT: publish a voluntary CPU0 yield before returning through the idle
    // context; the RISC-V sched_yield ABI can reuse this boundary when added.
    pub fn yield_current(&self, cpu: usize) -> bool {
        if cpu != 0 {
            return false;
        }
        let Some(task) = self.cur_task(cpu) else {
            return false;
        };
        if task.done() || task.sched_state() != TaskRunState::Running {
            return false;
        }
        task.set_sched_state(TaskRunState::Runnable);
        self.run_queue.enqueue(&task);
        if !self.switch_current_to_idle(cpu) {
            self.schedule_next_runnable(cpu);
        }
        true
    }

    // AGENT: update the task-owned priority; queued Arc<Task> entries observe
    // the authoritative policy directly, so no run-queue refresh is required.
    pub fn boost_task_priority(&self, task_id: usize, amount: i32) -> bool {
        let Some(task) = self.tasks.find_task(task_id) else {
            return false;
        };
        if task.done() {
            return false;
        }

        task.boost_priority(amount);
        true
    }

    // AGENT: route process-directed signals through the process thread set so
    // family and pid callers never need to store or rediscover a leader Task.
    pub fn send_signal_to_process(&self, process: &Arc<Process>, signo: i32, sender_pid: isize) {
        let target = process
            .thread_ids()
            .into_iter()
            .filter_map(|tid| self.tasks.find_task(tid))
            .find(|task| !task.done());
        if let Some(target) = target {
            self.send_signal_to_task(&target, signo, sender_pid);
        }
    }

    // AGENT: keep thread-target selection, pending-signal enqueue, and scheduler
    // wakeup together after a target Task has been selected.
    pub fn send_signal_to_task(&self, task: &Arc<Task>, signo: i32, sender_tid: isize) {
        if signo <= 0 || signo as u32 > NSIG {
            return;
        }
        let signo = signo as u32;
        let queued = task.enqueue_signal(signo as i32, sender_tid);
        if task.done() || (!queued && signo != SIGCONT && signo != SIGKILL) {
            return;
        }
        match signo {
            SIGCONT => {
                self.set_process_job_stopped(task, false);
            }
            SIGKILL => {
                self.set_process_job_stopped(task, false);
                if task.sched_state() == TaskRunState::Sleeping {
                    self.make_task_runnable(task);
                }
            }
            _ if task.is_job_stopped() => {}
            _ if task.sched_state() == TaskRunState::Sleeping => {
                self.wake_task_for_wait(task.id());
            }
            _ => {}
        }
    }

    // AGENT: WaitToken interruptible waits query this instead of removing the
    // signal; actual delivery still belongs to the syscall/schedule boundary.
    pub(crate) fn task_has_interrupting_signal(&self, task_id: usize) -> bool {
        self.tasks
            .find_task(task_id)
            .is_some_and(|task| !task.done() && task.has_interrupting_signal())
    }

    // AGENT: deliver pending signals outside a live trap borrow by snapshotting
    // and reinstalling the authoritative frame in the task's kernel stack.
    pub fn deliver_pending_signals(&self, cpu: usize) -> usize {
        let task = self.cur_task(cpu);
        let active_frame = task
            .as_ref()
            .and_then(|task| task.snapshot_user_trap_frame().ok());
        let (delivered, updated_frame) = self.deliver_pending_signals_inner(cpu, active_frame);
        if let (Some(task), Some(frame)) = (task, updated_frame) {
            task.install_user_trap_frame(frame)
                .expect("task frame disappeared during signal delivery");
        }
        delivered
    }

    // AGENT: QEMU syscall return supplies a clone of the live complete frame and
    // receives a complete replacement only when a userspace handler is entered.
    pub(crate) fn deliver_pending_signals_from_frame(
        &self,
        cpu: usize,
        interrupted: TrapFrame,
    ) -> Option<TrapFrame> {
        self.deliver_pending_signals_inner(cpu, Some(interrupted)).1
    }

    // AGENT: deliver default stop actions as process-wide run-queue barriers
    // without overwriting the other threads' underlying scheduler states.
    fn deliver_pending_signals_inner(
        &self,
        cpu: usize,
        mut active_frame: Option<TrapFrame>,
    ) -> (usize, Option<TrapFrame>) {
        if cpu != 0 {
            return (0, None);
        }
        let task = match self.cur_task(cpu) {
            Some(task) => task,
            None => return (0, None),
        };
        let mut delivered = 0usize;
        let mut updated_frame = None;
        while let Some(sig) = task.take_deliverable_signal() {
            delivered += 1;
            match sig.action.resolve(sig.signo) {
                SignalDeliveryAction::Ignore | SignalDeliveryAction::Continue => continue,
                SignalDeliveryAction::Stop => {
                    task.set_sched_state(TaskRunState::Runnable);
                    self.set_process_job_stopped(&task, true);
                    // AGENT: a stopped live task must leave its kernel stack
                    // through idle; metadata-only tests still select in place.
                    if !self.switch_current_to_idle(cpu) {
                        self.set_cur(cpu, None);
                        self.schedule_next_runnable(cpu);
                    }
                    break;
                }
                SignalDeliveryAction::Terminate => {
                    self.exit_thread_group(cpu, &task, ExitReason::Signal(sig.signo as u8));
                    break;
                }
                SignalDeliveryAction::Handler(handler) => {
                    let interrupted = match active_frame
                        .take()
                        .or_else(|| task.snapshot_user_trap_frame().ok())
                    {
                        Some(ctx) => ctx,
                        None => {
                            task.requeue_signal_front(sig.signo as i32, sig.sender_tid);
                            break;
                        }
                    };
                    let ctx = task.enter_signal_handler(sig, handler, interrupted);
                    active_frame = Some(ctx.clone());
                    updated_frame = Some(ctx);
                    break;
                }
            }
        }
        (delivered, updated_frame)
    }

    // AGENT: advance global timers after CPU0 has advanced the logical clock.
    pub(crate) fn advance_timers(&self) {
        let fired = {
            let mut timers = global_timer_wheel().lock();
            timers.advance()
        };

        for timer in fired {
            self.dispatch_timer(timer);
        }
    }

    // AGENT: dispatch typed timer expiry targets into the existing wake/signal
    // paths after the timer wheel lock has been released.
    fn dispatch_timer(&self, timer: TimerEntry) {
        match timer.target {
            TimerTarget::Noop => {}
            TimerTarget::WakeToken { token } => {
                token.wake_timeout();
            }
            TimerTarget::WakeTask { task_id } => {
                self.wake_task_for_wait(task_id);
            }
            TimerTarget::SignalTask {
                task_id,
                signo,
                sender_tid,
            } => {
                if let Some(task) = self.tasks.find_task(task_id) {
                    self.send_signal_to_task(&task, signo, sender_tid);
                }
            }
        }
    }

    // AGENT: CPU0 owns global logical-clock and timer-wheel progression.
    pub fn schedule_tick(&self, cpu: usize) {
        dtk(cpu);
        if cpu == 0 {
            self.advance_timers();
        }
        if cpu != 0 {
            return;
        }
        match self.cur_task(cpu) {
            Some(t) if t.done() => {
                t.set_sched_state(TaskRunState::Zombie);
                self.run_queue.remove(t.id());
                if !self.switch_current_to_idle(cpu) {
                    self.schedule_next_runnable(cpu);
                }
            }
            Some(t) if t.sched_state() != TaskRunState::Running => {}
            Some(t) => {
                if t.tick_slice() {
                    if self.run_queue.len() == 0 {
                        t.reset_slice();
                    } else {
                        // AGENT: A ready peer gets the CPU at slice expiry; the
                        // voluntary and preemptive paths share one handoff.
                        self.yield_current(cpu);
                    }
                }
            }
            None => {
                // AGENT: before the real scheduler starts, focused state tests
                // still use the old metadata-only selection path. Once active,
                // the idle loop owns selection after the interrupt returns.
                if !self.scheduler_active(cpu) {
                    self.schedule_next_runnable(cpu);
                }
            }
        }
    }

    // AGENT: run the CPU0 scheduler on the boot/idle stack. Every task returns
    // here through its own kernel context, so an empty system can safely enable
    // interrupts and wait without retaining any task kernel stack.
    pub fn run_cpu0(&'static self) -> ! {
        let idle_context = self.prepare_cpu0_scheduler();

        loop {
            crate::csr::disable_interrupts();
            if self.run_one_cpu0_task(idle_context) {
                continue;
            }
            self.set_cur(0, None);
            crate::csr::enable_interrupts();
            crate::sbi::wait_for_interrupt();
            crate::csr::disable_interrupts();
        }
    }

    // AGENT: move any boot-installed current task onto the run queue, then
    // activate the stable CPU0 idle-context slot exactly once.
    fn prepare_cpu0_scheduler(&self) -> *mut crate::context::KernelContext {
        crate::csr::disable_interrupts();
        if let Some(current) = self.cur_task(0) {
            if !current.done() {
                current.set_sched_state(TaskRunState::Runnable);
                self.run_queue.enqueue(&current);
            }
        }
        self.set_cur(0, None);
        self.activate_cpu0_scheduler()
    }

    // AGENT: dispatch one task while retaining its Arc on the idle stack across
    // __switch; this ownership makes task -> idle detachment safe without a clone.
    fn run_one_cpu0_task(&self, idle_context: *mut crate::context::KernelContext) -> bool {
        let Some(next) = self.dequeue_runnable_task() else {
            return false;
        };

        self.set_cur(0, Some(next.clone()));
        unsafe {
            crate::context::switch_kernel_context(idle_context, next.kernel_context_ptr());
        }
        if next.done() {
            next.release_kernel_stack();
        }
        true
    }

    // AGENT: exercise finite Processor/idle/task round trips in scheduler and
    // wait selftests without entering the production infinite idle loop; reuse
    // the installed idle context when one task must wake and block repeatedly.
    #[cfg(any(test, feature = "qemu-sched-selftest", feature = "qemu-sync-selftest"))]
    pub(crate) fn run_one_cpu0_task_for_test(&'static self) -> bool {
        let idle_context = if self.scheduler_active(0) {
            self.processors[0].lock().unwrap().idle_context_ptr()
        } else {
            self.prepare_cpu0_scheduler()
        };
        self.run_one_cpu0_task(idle_context)
    }

    // AGENT: select one runnable CPU0 task without publishing it as current;
    // the idle loop and metadata-only helper share this stale-entry filtering.
    fn dequeue_runnable_task(&self) -> Option<Arc<Task>> {
        while let Some(task) = self.run_queue.dequeue() {
            if !task.done()
                && !task.is_job_stopped()
                && task.sched_state() == TaskRunState::Runnable
            {
                task.set_sched_state(TaskRunState::Running);
                task.reset_slice();
                return Some(task);
            }
            if task.done() {
                task.set_sched_state(TaskRunState::Zombie);
            }
        }
        None
    }

    // AGENT: select from the runnable set and publish the winner only through
    // Kernel::set_cur(), avoiding a second current-task marker in RunQueue.
    pub(crate) fn schedule_next_runnable(&self, cpu: usize) -> bool {
        if cpu != 0 {
            return false;
        }
        if let Some(task) = self.dequeue_runnable_task() {
            self.set_cur(cpu, Some(task));
            self.deliver_pending_signals(cpu);
            return true;
        }
        self.set_cur(cpu, None);
        false
    }
}
