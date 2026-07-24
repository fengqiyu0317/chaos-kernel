// AGENT: focused RunQueue regressions shared by Rust cfg(test) and optional
// QEMU boot selftests.
use super::*;
use crate::kernel::kernel_core::{init_timer_wheel, TIMER_WHEEL};
use crate::kernel::{
    global_kernel, install_kernel, signal_bit, ExitReason, FramePool, Kernel, SigAction, SigSet,
    SignalDeliveryAction, SignalEnqueueResult, TaskRunState, NSIG, PRIO_MIN, SIGCHLD, SIGCONT,
    SIGKILL, SIGRTMIN, SIGSTOP, SIGTSTP, SIGTTIN, SIGTTOU, SIGURG, SIGUSR1, SIGUSR2, SIGWINCH,
    SIG_DFL, SIG_IGN, SYS_SIGRETURN, UNMASKABLE_SIGNAL_MASK, USER_SIGTRAMP,
};
use crate::trap::TrapFrame;
use core::sync::atomic::{AtomicBool, Ordering};

static PROCESSOR_EXIT_TEST_RAN: AtomicBool = AtomicBool::new(false);
static PROCESSOR_STOP_TEST_ENTERED: AtomicBool = AtomicBool::new(false);
static PROCESSOR_STOP_TEST_RESUMED: AtomicBool = AtomicBool::new(false);

// AGENT: expose focused scheduler queue checks to the optional QEMU boot
// selftest path and use its discovered RAM pool for real task kernel stacks.
pub fn run_all(pool: &FramePool) {
    dequeue_preserves_fifo_for_equal_priority(pool);
    queued_task_observes_policy_change_without_duplicate_entry(pool);
    kernel_boost_updates_task_policy_and_run_queue_order(pool);
    signal_numbering_uses_all_linux_slots();
    highest_signal_can_be_queued_and_blocked(pool);
    unmaskable_signals_ignore_stale_mask_bits(pool);
    default_sigcont_does_not_interrupt_wait(pool);
    standard_signals_coalesce_and_realtime_signals_queue(pool);
    default_signal_actions_follow_linux_classes();
    ignored_signal_is_neither_queued_nor_woken(pool);
    changing_to_ignored_action_discards_pending_signal(pool);
    sigcont_and_stop_generation_discard_pending_opposites(pool);
    process_signal_wakes_an_unblocked_thread(pool);
    signal_stop_and_sigcont_cover_thread_group(pool);
    sigcont_resumes_stopped_task_without_resuming_for_plain_signal(pool);
    sigcont_keeps_sleeping_task_asleep_until_wait_wakeup(pool);
    signal_handler_uses_supplied_interrupted_frame(pool);
    processor_releases_exited_stack_after_idle_handoff(pool);
}

// AGENT: execute the real Processor idle -> task -> idle path once and prove an
// exiting task's live kernel stack is released only after idle regains control.
fn processor_releases_exited_stack_after_idle_handoff(pool: &FramePool) {
    ensure_timer_wheel();
    PROCESSOR_EXIT_TEST_RAN.store(false, Ordering::Relaxed);

    let kernel = Box::leak(Box::new(Kernel::new(pool.clone())));
    kernel.proc_init();
    // AGENT: init exit is the real machine-shutdown policy; use a queued
    // ordinary process for this finite task -> idle -> assertion round trip.
    let task = kernel
        .tasks
        .spawn()
        .expect("processor exit test task should spawn");
    task.set_sched_state(TaskRunState::Runnable);
    kernel.run_queue.enqueue(&task);
    task.install_test_kernel_entry(processor_exit_test_task)
        .expect("test task should receive kernel entry");
    assert!(task.kernel_stack_top().is_some());
    install_kernel(kernel);

    assert!(kernel.run_one_cpu0_task_for_test());
    assert!(PROCESSOR_EXIT_TEST_RAN.load(Ordering::Relaxed));
    assert!(kernel.cur_task(0).is_none());
    assert!(task.done());
    assert!(task.kernel_stack_top().is_none());
}

// AGENT: exit the selected Task on its live stack, verify teardown retained that
// stack, then use the production handoff so the idle side can release it.
extern "C" fn processor_exit_test_task() -> ! {
    PROCESSOR_EXIT_TEST_RAN.store(true, Ordering::Relaxed);
    let kernel = global_kernel().expect("scheduler test kernel should be installed");
    let task = kernel
        .cur_task(0)
        .expect("scheduler test task should be current");
    assert_eq!(
        kernel.exit_current_thread(0, &task, ExitReason::Code(0)),
        Ok(())
    );
    assert!(task.done());
    assert!(task.kernel_stack_top().is_some());
    drop(task);
    kernel.switch_current_to_idle(0);
    loop {
        core::hint::spin_loop();
    }
}

// AGENT: pin the Linux/RISC-V 1..=64 ABI to compact table slots and sigset_t
// bits so neither signal 1 nor signal 64 is lost at an endpoint.
#[cfg_attr(test, test)]
fn signal_numbering_uses_all_linux_slots() {
    let mut actions = SigSet::new();
    assert_eq!(actions.actions.len(), NSIG as usize);
    assert_eq!(signal_bit(1), Some(1));
    assert_eq!(signal_bit(NSIG), Some(1u64 << 63));
    assert_eq!(signal_bit(0), None);
    assert_eq!(signal_bit(NSIG + 1), None);

    assert!(!actions.set_action(0, SigAction::default_action()));
    let caught_mask = signal_bit(SIGUSR2).expect("valid SIGUSR2");
    assert!(actions.set_action(
        1,
        SigAction {
            handler: 0x4000,
            mask: caught_mask,
        },
    ));
    assert!(actions.set_action(
        NSIG,
        SigAction {
            handler: 0x5000,
            mask: 0,
        },
    ));
    assert_eq!(
        actions.get_action(1).map(|action| action.handler),
        Some(0x4000)
    );
    assert_eq!(
        actions.get_action(NSIG).map(|action| action.handler),
        Some(0x5000)
    );
    assert!(actions.get_action(0).is_none());
    assert!(actions.get_action(NSIG + 1).is_none());

    let ignored_mask = signal_bit(SIGUSR1).expect("valid SIGUSR1");
    assert!(actions.set_action(
        SIGURG,
        SigAction {
            handler: SIG_IGN,
            mask: ignored_mask,
        },
    ));
    let default_mask = signal_bit(SIGUSR2).expect("valid SIGUSR2");
    assert!(actions.set_action(
        SIGWINCH,
        SigAction {
            handler: SIG_DFL,
            mask: default_mask,
        },
    ));

    actions.reset_for_exec();
    assert_eq!(
        actions
            .get_action(1)
            .map(|action| (action.handler, action.mask)),
        Some((SIG_DFL, 0))
    );
    assert_eq!(
        actions
            .get_action(NSIG)
            .map(|action| (action.handler, action.mask)),
        Some((SIG_DFL, 0))
    );
    assert_eq!(
        actions
            .get_action(SIGURG)
            .map(|action| (action.handler, action.mask)),
        Some((SIG_IGN, 0))
    );
    assert_eq!(
        actions
            .get_action(SIGWINCH)
            .map(|action| (action.handler, action.mask)),
        Some((SIG_DFL, 0))
    );
}

// AGENT: exercise signal 64 through the live pending queue and task mask so
// endpoint validation cannot regress independently from the table helpers.
fn highest_signal_can_be_queued_and_blocked(pool: &FramePool) {
    ensure_timer_wheel();

    let kernel = Kernel::new(pool.clone());
    let task = kernel.tasks.spawn().expect("spawn worker");
    let highest_bit = signal_bit(NSIG).expect("signal 64 must occupy bit 63");
    *task.sig_mask.lock().unwrap() = highest_bit;

    kernel.send_signal_to_task(&task, NSIG as i32, -1);
    assert_eq!(task.process.sig_queue.lock().unwrap().len(), 1);
    assert!(!task.has_interrupting_signal());
    assert!(task.take_deliverable_signal().is_none());

    *task.sig_mask.lock().unwrap() = 0;
    assert!(task.has_interrupting_signal());
    assert_eq!(
        task.take_deliverable_signal().map(|signal| signal.signo),
        Some(NSIG)
    );
}

// AGENT: enforce the kernel invariant at every query boundary even if internal
// state contains stale SIGKILL/SIGSTOP mask bits that userspace cannot install.
fn unmaskable_signals_ignore_stale_mask_bits(pool: &FramePool) {
    ensure_timer_wheel();

    let kernel = Kernel::new(pool.clone());
    let task = kernel.tasks.spawn().expect("spawn signal mask task");
    *task.sig_mask.lock().unwrap() = UNMASKABLE_SIGNAL_MASK;

    for signo in [SIGKILL, SIGSTOP] {
        assert_eq!(
            task.enqueue_signal(signo as i32, -1),
            SignalEnqueueResult::Queued
        );
        assert!(task.signal_is_unblocked(signo));
        assert!(task.has_interrupting_signal());
        assert_eq!(
            task.take_deliverable_signal().map(|signal| signal.signo),
            Some(signo)
        );
    }
}

// AGENT: a default SIGCONT may resume job control but must not turn an unrelated
// interruptible wait into EINTR; caught SIGCONT remains covered by Handler.
fn default_sigcont_does_not_interrupt_wait(pool: &FramePool) {
    ensure_timer_wheel();

    let kernel = Kernel::new(pool.clone());
    let task = kernel.tasks.spawn().expect("spawn SIGCONT task");
    assert_eq!(
        task.enqueue_signal(SIGCONT as i32, -1),
        SignalEnqueueResult::Queued
    );

    assert!(!task.has_interrupting_signal());
    assert_eq!(
        task.take_deliverable_signal()
            .map(|signal| signal.action.resolve(signal.signo)),
        Some(SignalDeliveryAction::Continue)
    );
}

// AGENT: retain one pending instance and its first sender for standard signals,
// but queue realtime duplicates and deliver them by signal number then FIFO.
fn standard_signals_coalesce_and_realtime_signals_queue(pool: &FramePool) {
    ensure_timer_wheel();

    let kernel = Kernel::new(pool.clone());
    let task = kernel.tasks.spawn().expect("spawn signal queue task");
    assert_eq!(
        task.enqueue_signal(SIGUSR1 as i32, 10),
        SignalEnqueueResult::Queued
    );
    assert_eq!(
        task.enqueue_signal(SIGUSR1 as i32, 11),
        SignalEnqueueResult::AlreadyPending
    );

    let realtime_high = SIGRTMIN + 2;
    assert_eq!(
        task.enqueue_signal(realtime_high as i32, 20),
        SignalEnqueueResult::Queued
    );
    assert_eq!(
        task.enqueue_signal(SIGRTMIN as i32, 21),
        SignalEnqueueResult::Queued
    );
    assert_eq!(
        task.enqueue_signal(realtime_high as i32, 22),
        SignalEnqueueResult::Queued
    );
    assert_eq!(task.process.sig_queue.lock().unwrap().len(), 4);

    let delivered: Vec<_> = (0..4)
        .map(|_| {
            let signal = task
                .take_deliverable_signal()
                .expect("queued signal should be deliverable");
            (signal.signo, signal.sender_tid)
        })
        .collect();
    assert_eq!(
        delivered,
        vec![
            (SIGUSR1, 10),
            (SIGRTMIN, 21),
            (realtime_high, 20),
            (realtime_high, 22),
        ]
    );
}

// AGENT: keep the SIG_DFL policy table pinned to the Linux/RISC-V ignore,
// continue, stop, and terminate classes supported by the current carrier.
#[cfg_attr(test, test)]
fn default_signal_actions_follow_linux_classes() {
    let action = SigAction::default_action();
    for signo in [SIGCHLD, SIGURG, SIGWINCH] {
        assert_eq!(action.resolve(signo), SignalDeliveryAction::Ignore);
    }
    assert_eq!(action.resolve(SIGCONT), SignalDeliveryAction::Continue);
    for signo in [SIGSTOP, SIGTSTP, SIGTTIN, SIGTTOU] {
        assert_eq!(action.resolve(signo), SignalDeliveryAction::Stop);
    }
    assert_eq!(action.resolve(SIGUSR1), SignalDeliveryAction::Terminate);

    let handler = SigAction {
        handler: 0x5000,
        mask: 0,
    };
    assert_eq!(
        handler.resolve(SIGUSR1),
        SignalDeliveryAction::Handler(0x5000)
    );
}

// AGENT: default-ignored signals must not become pending or wake a task that is
// sleeping for an unrelated wait condition.
fn ignored_signal_is_neither_queued_nor_woken(pool: &FramePool) {
    ensure_timer_wheel();

    let kernel = Kernel::new(pool.clone());
    let task = kernel.tasks.spawn().expect("spawn worker");
    task.set_sched_state(TaskRunState::Sleeping);

    kernel.send_signal_to_task(&task, SIGURG as i32, -1);

    assert!(task.process.sig_queue.lock().unwrap().is_empty());
    assert_eq!(task.sched_state(), TaskRunState::Sleeping);
    assert_eq!(kernel.run_queue.pick_next(), None);
}

// AGENT: installing SIG_IGN, or restoring SIG_DFL for a default-ignored
// signal, discards an existing pending instance while the queue/state locks
// make the transition atomic with signal generation.
fn changing_to_ignored_action_discards_pending_signal(pool: &FramePool) {
    ensure_timer_wheel();

    let kernel = Kernel::new(pool.clone());
    let task = kernel.tasks.spawn().expect("spawn worker");
    assert_eq!(
        task.enqueue_signal(SIGUSR1 as i32, -1),
        SignalEnqueueResult::Queued
    );
    assert!(task.process.set_signal_action(
        SIGUSR1,
        SigAction {
            handler: SIG_IGN,
            mask: 0,
        },
    ));
    assert!(task.process.sig_queue.lock().unwrap().is_empty());

    assert!(task.process.set_signal_action(
        SIGURG,
        SigAction {
            handler: 0x5000,
            mask: 0,
        },
    ));
    assert_eq!(
        task.enqueue_signal(SIGURG as i32, -1),
        SignalEnqueueResult::Queued
    );
    assert!(task
        .process
        .set_signal_action(SIGURG, SigAction::default_action()));
    assert!(task.process.sig_queue.lock().unwrap().is_empty());
}

// AGENT: apply the generation-time job-control rule even when the newly
// generated opposite signal is ignored by its current disposition.
fn sigcont_and_stop_generation_discard_pending_opposites(pool: &FramePool) {
    ensure_timer_wheel();

    let kernel = Kernel::new(pool.clone());
    let task = kernel.tasks.spawn().expect("spawn job-control queue task");
    assert_eq!(
        task.enqueue_signal(SIGSTOP as i32, -1),
        SignalEnqueueResult::Queued
    );
    assert_eq!(
        task.enqueue_signal(SIGCONT as i32, -1),
        SignalEnqueueResult::Queued
    );
    assert_eq!(
        task.process
            .sig_queue
            .lock()
            .unwrap()
            .iter()
            .map(|(signo, _)| *signo)
            .collect::<Vec<_>>(),
        vec![SIGCONT as i32]
    );

    assert_eq!(
        task.enqueue_signal(SIGTSTP as i32, -1),
        SignalEnqueueResult::Queued
    );
    assert_eq!(
        task.process
            .sig_queue
            .lock()
            .unwrap()
            .iter()
            .map(|(signo, _)| *signo)
            .collect::<Vec<_>>(),
        vec![SIGTSTP as i32]
    );

    assert!(task.process.set_signal_action(
        SIGCONT,
        SigAction {
            handler: SIG_IGN,
            mask: 0,
        },
    ));
    assert_eq!(
        task.enqueue_signal(SIGCONT as i32, -1),
        SignalEnqueueResult::Ignored
    );
    assert!(task.process.sig_queue.lock().unwrap().is_empty());
}

// AGENT: a process-directed signal chooses an unblocked sibling for scheduler
// wakeup and remains pending without waking any thread when every mask blocks it.
fn process_signal_wakes_an_unblocked_thread(pool: &FramePool) {
    ensure_timer_wheel();

    let kernel = Kernel::new(pool.clone());
    let leader = kernel.tasks.spawn().expect("spawn process leader");
    let thread = kernel
        .tasks
        .clone_thread(&leader, 0x8000_0000, 0)
        .expect("clone signal receiver");
    let signal_mask = signal_bit(SIGUSR1).expect("SIGUSR1 has a mask bit");
    *leader.sig_mask.lock().unwrap() = signal_mask;
    leader.set_sched_state(TaskRunState::Sleeping);
    thread.set_sched_state(TaskRunState::Sleeping);

    kernel.send_signal_to_process(&leader.process, SIGUSR1 as i32, -1);

    assert_eq!(leader.sched_state(), TaskRunState::Sleeping);
    assert_eq!(thread.sched_state(), TaskRunState::Runnable);
    assert_eq!(kernel.run_queue.pick_next(), Some(thread.id()));
    assert_eq!(
        thread.take_deliverable_signal().map(|signal| signal.signo),
        Some(SIGUSR1)
    );

    kernel.run_queue.remove(thread.id());
    *thread.sig_mask.lock().unwrap() = signal_mask;
    thread.set_sched_state(TaskRunState::Sleeping);
    kernel.send_signal_to_process(&leader.process, SIGUSR1 as i32, -1);

    assert_eq!(leader.sched_state(), TaskRunState::Sleeping);
    assert_eq!(thread.sched_state(), TaskRunState::Sleeping);
    assert_eq!(kernel.run_queue.pick_next(), None);
    assert_eq!(leader.process.sig_queue.lock().unwrap().len(), 1);
}

// AGENT: QEMU boot selftests initialize the timer wheel in rust_main(), while
// ordinary Rust tests may construct Kernel directly.
fn ensure_timer_wheel() {
    if TIMER_WHEEL.get().is_none() {
        init_timer_wheel();
    }
}

// AGENT: same-priority tasks should be selected in insertion order.
fn dequeue_preserves_fifo_for_equal_priority(pool: &FramePool) {
    let kernel = Kernel::new(pool.clone());
    let rq = RunQueue::new();
    let first = kernel.tasks.spawn().expect("spawn first FIFO task");
    let second = kernel.tasks.spawn().expect("spawn second FIFO task");
    let third = kernel.tasks.spawn().expect("spawn third FIFO task");
    rq.enqueue(&first);
    rq.enqueue(&second);
    rq.enqueue(&third);

    assert_eq!(rq.dequeue().map(|task| task.id()), Some(first.id()));
    assert_eq!(rq.dequeue().map(|task| task.id()), Some(second.id()));
    assert_eq!(rq.dequeue().map(|task| task.id()), Some(third.id()));
    assert_eq!(rq.dequeue().map(|task| task.id()), None);
}

// AGENT: queued tasks expose their authoritative policy through Arc<Task>, so a
// boost changes ordering without refreshing or duplicating the run-queue entry.
fn queued_task_observes_policy_change_without_duplicate_entry(pool: &FramePool) {
    let kernel = Kernel::new(pool.clone());
    let rq = RunQueue::new();
    let first = kernel.tasks.spawn().expect("spawn first policy task");
    let second = kernel.tasks.spawn().expect("spawn second policy task");
    second.boost_priority(5);
    rq.enqueue(&first);
    rq.enqueue(&second);
    assert_eq!(rq.pick_next(), Some(second.id()));

    first.boost_priority(10);
    rq.enqueue(&first);

    assert_eq!(rq.len(), 2);
    assert_eq!(rq.pick_next(), Some(first.id()));
    let dequeued = rq.dequeue().expect("boosted task should dequeue");
    assert_eq!(dequeued.id(), first.id());
    assert_eq!(dequeued.sched_policy().prio, -10);
    assert_eq!(rq.dequeue().map(|task| task.id()), Some(second.id()));
}

// AGENT: Kernel-level boosts update the task-owned policy so the run queue sees
// new ordering directly through its existing Arc<Task> entry.
fn kernel_boost_updates_task_policy_and_run_queue_order(pool: &FramePool) {
    ensure_timer_wheel();

    let kernel = Kernel::new(pool.clone());
    let first = kernel.tasks.spawn().expect("spawn first task");
    let second = kernel.tasks.spawn().expect("spawn second task");

    second.boost_priority(5);
    first.set_sched_state(TaskRunState::Runnable);
    second.set_sched_state(TaskRunState::Runnable);
    kernel.run_queue.enqueue(&first);
    kernel.run_queue.enqueue(&second);
    assert_eq!(kernel.run_queue.pick_next(), Some(second.id()));

    assert!(kernel.boost_task_priority(first.id(), 10));
    assert_eq!(first.sched_policy().prio, -10);
    assert_eq!(kernel.run_queue.pick_next(), Some(first.id()));
    let dequeued = kernel
        .run_queue
        .dequeue()
        .expect("boosted task should dequeue first");
    assert_eq!(dequeued.id(), first.id());
    assert_eq!(dequeued.sched_policy().prio, -10);

    assert!(kernel.boost_task_priority(first.id(), i32::MAX));
    assert_eq!(first.sched_policy().prio, PRIO_MIN);
    kernel.run_queue.enqueue(&first);
    assert_eq!(kernel.run_queue.pick_next(), Some(first.id()));
}

// AGENT: run SIGSTOP and SIGCONT through the real task -> idle -> task path;
// stopping removes every sibling from the queue and continuing requeues all
// threads whose underlying scheduler state remains runnable.
fn signal_stop_and_sigcont_cover_thread_group(pool: &FramePool) {
    ensure_timer_wheel();
    PROCESSOR_STOP_TEST_ENTERED.store(false, Ordering::Relaxed);
    PROCESSOR_STOP_TEST_RESUMED.store(false, Ordering::Relaxed);

    let kernel = Box::leak(Box::new(Kernel::new(pool.clone())));
    let task = kernel
        .tasks
        .spawn_root()
        .expect("stop test init task should spawn");
    task.set_sched_state(TaskRunState::Runnable);
    kernel.run_queue.enqueue(&task);
    let thread = kernel
        .tasks
        .clone_thread(&task, 0x8000_0000, 0)
        .expect("thread clone should succeed");
    thread.set_sched_state(TaskRunState::Runnable);
    kernel.run_queue.enqueue(&thread);
    task.install_test_kernel_entry(processor_stop_test_task)
        .expect("stop test task should receive kernel entry");
    install_kernel(kernel);

    assert!(kernel.run_one_cpu0_task_for_test());
    assert!(PROCESSOR_STOP_TEST_ENTERED.load(Ordering::Relaxed));
    assert!(task.process.is_job_stopped());
    assert!(thread.process.is_job_stopped());
    assert_eq!(task.sched_state(), TaskRunState::Runnable);
    assert_eq!(thread.sched_state(), TaskRunState::Runnable);
    assert!(kernel.cur_task(0).is_none());
    assert_eq!(kernel.run_queue.pick_next(), None);

    kernel.send_signal_to_task(&task, SIGCONT as i32, -1);

    assert!(!task.process.is_job_stopped());
    assert!(!thread.process.is_job_stopped());
    assert_eq!(kernel.run_queue.len(), 2);
    assert_eq!(kernel.run_queue.pick_next(), Some(task.id()));

    assert!(kernel.run_one_cpu0_task_for_test());
    assert!(PROCESSOR_STOP_TEST_RESUMED.load(Ordering::Relaxed));
    assert!(task.done());
    assert!(kernel.cur_task(0).is_none());
    assert_eq!(kernel.run_queue.pick_next(), Some(thread.id()));

    kernel.run_queue.remove(thread.id());
    thread.mark_thread_exited();
    thread.release_kernel_stack();
}

// AGENT: enter stop delivery on the selected task's real kernel stack; the
// function resumes only after the test driver sends SIGCONT and dispatches it
// again through the idle scheduler.
extern "C" fn processor_stop_test_task() -> ! {
    PROCESSOR_STOP_TEST_ENTERED.store(true, Ordering::Relaxed);
    let kernel = global_kernel().expect("stop test kernel should be installed");
    let task = kernel
        .cur_task(0)
        .expect("stop test task should be current");

    kernel.send_signal_to_task(&task, SIGSTOP as i32, -1);
    assert_eq!(kernel.deliver_pending_signals(0), 1);

    PROCESSOR_STOP_TEST_RESUMED.store(true, Ordering::Relaxed);
    task.mark_thread_exited();
    drop(task);
    kernel.switch_current_to_idle(0);
    loop {
        core::hint::spin_loop();
    }
}

// AGENT: ordinary pending signals stay queued for a stopped task; SIGCONT is
// the explicit transition back to runnable state.
fn sigcont_resumes_stopped_task_without_resuming_for_plain_signal(pool: &FramePool) {
    ensure_timer_wheel();

    let kernel = Kernel::new(pool.clone());
    let task = kernel.tasks.spawn().expect("spawn worker");
    task.set_sched_state(TaskRunState::Runnable);
    task.process.set_job_stopped(true);

    kernel.send_signal_to_task(&task, SIGUSR1 as i32, -1);
    assert!(task.process.is_job_stopped());
    assert_eq!(task.sched_state(), TaskRunState::Runnable);
    assert_eq!(kernel.run_queue.pick_next(), None);

    kernel.send_signal_to_task(&task, SIGCONT as i32, -1);
    assert!(!task.process.is_job_stopped());
    assert_eq!(task.sched_state(), TaskRunState::Runnable);
    assert_eq!(kernel.run_queue.pick_next(), Some(task.id()));
}

// AGENT: SIGCONT clears job-control stop but does not collapse a still-blocked
// wait into runnable state; the real wait wakeup owns that transition.
fn sigcont_keeps_sleeping_task_asleep_until_wait_wakeup(pool: &FramePool) {
    ensure_timer_wheel();

    let kernel = Kernel::new(pool.clone());
    let task = kernel.tasks.spawn().expect("spawn worker");
    task.set_sched_state(TaskRunState::Sleeping);
    task.process.set_job_stopped(true);

    kernel.send_signal_to_task(&task, SIGCONT as i32, -1);
    assert!(!task.process.is_job_stopped());
    assert_eq!(task.sched_state(), TaskRunState::Sleeping);
    assert_eq!(kernel.run_queue.pick_next(), None);

    assert!(kernel.wake_task_for_wait(task.id()));
    assert_eq!(task.sched_state(), TaskRunState::Runnable);
    assert_eq!(kernel.run_queue.pick_next(), Some(task.id()));
}

// AGENT: QEMU syscall delivery supplies the complete live TrapFrame; handler
// entry and sigreturn must preserve every register outside the handler ABI slots.
fn signal_handler_uses_supplied_interrupted_frame(pool: &FramePool) {
    ensure_timer_wheel();

    let kernel = Kernel::new(pool.clone());
    kernel.proc_init();
    let task = kernel.cur_task(0).expect("init task should be current");
    let handler = 0x5000usize;
    assert!(task.process.sig_state.lock().unwrap().set_action(
        SIGUSR1,
        SigAction {
            handler,
            mask: signal_bit(SIGUSR2).expect("valid SIGUSR2"),
        },
    ));

    let mut interrupted = TrapFrame::new();
    for index in 1..interrupted.regs.len() {
        interrupted.regs[index] = 0x3000 + index;
    }
    interrupted.regs[10] = 0xfeed;
    interrupted.regs[2] = 0x8000_0000;
    interrupted.sstatus = 0x20;
    interrupted.sepc = 0x1234;

    kernel.send_signal_to_task(&task, SIGUSR1 as i32, 77);
    let next = kernel
        .deliver_pending_signals_from_frame(0, interrupted.clone())
        .expect("handler delivery should produce a next frame");

    assert_eq!(next.sepc, handler);
    assert_eq!(next.regs[1], USER_SIGTRAMP);
    assert_eq!(next.regs[10], SIGUSR1 as usize);
    assert_eq!(next.regs[11], 77);
    assert_eq!(next.regs[12], interrupted.sepc);
    for index in 0..interrupted.regs.len() {
        if !matches!(index, 1 | 10..=12) {
            assert_eq!(next.regs[index], interrupted.regs[index]);
        }
    }
    assert_eq!(next.sstatus, interrupted.sstatus);
    assert_ne!(
        *task.sig_mask.lock().unwrap() & signal_bit(SIGUSR1).expect("valid SIGUSR1"),
        0
    );
    assert_ne!(
        *task.sig_mask.lock().unwrap() & signal_bit(SIGUSR2).expect("valid SIGUSR2"),
        0
    );

    {
        let sig_frames = task.sig_frames.lock().unwrap();
        assert_eq!(sig_frames.len(), 1);
        assert_eq!(sig_frames[0].saved_frame, interrupted);
    }

    task.install_user_trap_frame(next)
        .expect("handler frame should install");
    assert_eq!(
        kernel.dispatch_syscall_without_signal_delivery(SYS_SIGRETURN, 0, 0, 0, 0, 0, 0),
        Ok(0xfeed)
    );
    assert_eq!(
        task.snapshot_user_trap_frame()
            .expect("sigreturn frame should exist"),
        interrupted
    );
    assert_eq!(*task.sig_mask.lock().unwrap(), 0);
}
