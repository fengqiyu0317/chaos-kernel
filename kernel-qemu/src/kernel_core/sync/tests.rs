// AGENT: keep WaitToken regressions next to the QEMU sync primitives and expose
// them through the same run_all + cfg_attr(test, test) pattern as mm/tests.rs.
use super::futex::FutexWaiter;
use super::*;
use crate::kernel::kernel_core::prelude::*;
use crate::kernel::kernel_core::{
    duration_to_ticks, global_timer_wheel, init_timer_wheel, TimerTarget, TimerWheel, TIMER_WHEEL,
};
use crate::kernel::{
    clear_global_kernel_for_test, epoll_ready_events, install_kernel, signal_bit, EpCtlOp, EpData,
    EpEvent, EpInst, EpKey, FLike, FdEntry, FdWriteOutcome, FileIdentity, FramePool, Kernel,
    LockKind, LockRange, PipeNode, PipeWriteOutcome, RecordLockRequest, SigAction, TaskRunState,
    VmRegion, CLK, PIPE_BUF, SIGCHLD, SIGUSR1,
};

// AGENT: share one token/result slot with the real task-stack wait regressions;
// run_all executes them serially on CPU0.
static WAIT_ROUND_TRIP_TOKEN: Mutex<Option<WaitToken>> = Mutex::new(None);
static WAIT_ROUND_TRIP_OUTCOME: Mutex<Option<WaitOutcome>> = Mutex::new(None);
// AGENT: share one futex/result pair with task-stack round-trip regressions so
// Event and Signal exercise FutexBucket::wait through the real scheduler path.
static FUTEX_ROUND_TRIP_BUCKET: Mutex<Option<Arc<FutexBucket>>> = Mutex::new(None);
static FUTEX_ROUND_TRIP_RESULT: Mutex<Option<Result<(), &'static str>>> = Mutex::new(None);
const FUTEX_ROUND_TRIP_ADDR: usize = 0xB000;
// AGENT: share pipe endpoints and results with task-stack blocking regressions;
// each test installs one pair and clears it before replacing the global kernel.
static PIPE_ROUND_TRIP_PAIR: Mutex<Option<(PipeNode, PipeNode)>> = Mutex::new(None);
static PIPE_READ_ROUND_TRIP_RESULT: Mutex<Option<Result<usize, &'static str>>> = Mutex::new(None);
static PIPE_WRITE_ROUND_TRIP_RESULT: Mutex<Option<Result<PipeWriteOutcome, &'static str>>> =
    Mutex::new(None);
// AGENT: hold only the blocking reader across the task-stack group-exit test;
// peer endpoints stay with the idle-side driver for post-exit EOF/EPIPE checks.
static GROUP_EXIT_PIPE_READER: Mutex<Option<PipeNode>> = Mutex::new(None);
static GROUP_EXIT_PIPE_RESULT: Mutex<Option<Result<usize, &'static str>>> = Mutex::new(None);
static GROUP_EXIT_TIMER_RESULT: Mutex<Option<WaitOutcome>> = Mutex::new(None);
static GROUP_EXIT_FUTEX_RESULT: Mutex<Option<Result<(), &'static str>>> = Mutex::new(None);
static GROUP_EXIT_EPOLL: Mutex<Option<EpInst>> = Mutex::new(None);
static GROUP_EXIT_EPOLL_RESULT: Mutex<Option<WaitOutcome>> = Mutex::new(None);
static GROUP_EXIT_RECORD_REQUEST: Mutex<Option<RecordLockRequest>> = Mutex::new(None);
static GROUP_EXIT_RECORD_RESULT: Mutex<Option<Result<(), &'static str>>> = Mutex::new(None);
static GROUP_EXIT_CALLER_RAN: AtomicBool = AtomicBool::new(false);
// AGENT: carry one F_SETLKW request/result through real task-stack scheduling so
// blocking record locks reuse the same wake and signal paths as pipes/futexes.
static RECORD_LOCK_ROUND_TRIP_REQUEST: Mutex<Option<RecordLockRequest>> = Mutex::new(None);
static RECORD_LOCK_ROUND_TRIP_RESULT: Mutex<Option<Result<(), &'static str>>> = Mutex::new(None);
const RECORD_LOCK_WAITER_PID: usize = 41;
const RECORD_LOCK_BLOCKER_PID: usize = 42;
const RECORD_LOCK_FILE_A: FileIdentity = FileIdentity {
    fs_id: 17,
    inode: 23,
};
const RECORD_LOCK_FILE_B: FileIdentity = FileIdentity {
    fs_id: 17,
    inode: 24,
};

// AGENT: include futex requeue/user-mapping and storage/fd regressions, using
// the boot-discovered physical pool whenever tests allocate mapped task pages.
pub fn run_all(pool: &FramePool) {
    #[cfg(feature = "qemu-sync-selftest")]
    crate::kernel::fs::block_device::tests::run_all();
    #[cfg(feature = "qemu-sync-selftest")]
    crate::kernel::fs::block_cache::tests::run_all();
    #[cfg(feature = "qemu-sync-selftest")]
    crate::kernel::fs::fs_instance::tests::run_all();
    crate::kernel::fs::fd::tests::run_all();
    // AGENT: Follow the mount-table selftests to their dedicated module after
    // splitting mount_io_disk into separate responsibilities.
    crate::kernel::fs::mount::tests::run_all();
    wait_token_binds_selected_task();
    wait_token_event_wake_wins_once();
    wait_token_timeout_wake_wins_once();
    duration_to_ticks_rounds_up_at_tick_boundaries();
    wait_token_current_deadline_times_out_without_timer_wheel();
    wait_token_expired_deadline_times_out_immediately();
    wait_token_timer_target_times_out_on_schedule_tick(pool);
    wait_token_event_wake_uses_installed_scheduler_backend(pool);
    wait_token_interruptible_wait_reports_signal_not_event(pool);
    wait_token_stays_blocked_after_masked_signal(pool);
    wait_token_sleeping_wait_reports_later_signal(pool);
    record_lock_blocking_wait_wakes_and_detects_deadlock(pool);
    record_lock_blocking_wait_returns_eintr(pool);
    futex_wait_returns_changed_without_queueing();
    futex_wait_propagates_word_read_fault();
    futex_wait_timeout_removes_published_waiter();
    futex_wait_event_removes_published_waiter(pool);
    futex_wait_signal_removes_published_waiter(pool);
    futex_cmp_requeue_propagates_word_read_fault();
    futex_cmp_requeue_mismatch_preserves_waiters();
    futex_cmp_requeue_wakes_moves_and_returns_affected();
    futex_requeue_skips_completed_waiters_when_moving();
    futex_requeue_syscalls_reject_unmapped_destination(pool);
    pipe_uses_bounded_ring_buffer_and_reports_writable();
    pipe_nonblocking_small_write_is_atomic();
    pipe_nonblocking_large_write_can_be_partial();
    pipe_buffered_bytes_precede_eof_and_missing_reader_breaks_write();
    pipe_blocking_read_sleeps_until_data_arrives(pool);
    pipe_blocking_read_wakes_for_eof(pool);
    pipe_blocking_large_write_resumes_until_complete(pool);
    pipe_blocking_write_wakes_for_broken_peer(pool);
    group_exit_unwinds_blocked_pipe_reader(pool);
    pipe_rejects_wrong_direction_direct_io();
    pipe_epoll_closed_status_reports_hup_and_err();
    fd_allocator_supports_lower_bounds_fixed_targets_and_reuse(pool);
    fd_close_detaches_epoll_subscription_before_reuse(pool);
    fd_alias_keeps_epoll_source_across_number_reuse(pool);
    forked_fd_slot_keeps_epoll_source_until_child_close(pool);
    epoll_ready_list_deduplicates_and_requeues();
}

// AGENT: run exit_group and a canceled pipe read on their real kernel stacks;
// only the last cooperative acknowledgement may publish Zombie and SIGCHLD.
fn group_exit_unwinds_blocked_pipe_reader(pool: &FramePool) {
    reset_wait_token_state();
    GROUP_EXIT_CALLER_RAN.store(false, Ordering::Relaxed);
    *GROUP_EXIT_PIPE_RESULT.lock().unwrap() = None;
    *GROUP_EXIT_TIMER_RESULT.lock().unwrap() = None;
    *GROUP_EXIT_FUTEX_RESULT.lock().unwrap() = None;
    *GROUP_EXIT_EPOLL_RESULT.lock().unwrap() = None;
    *GROUP_EXIT_RECORD_RESULT.lock().unwrap() = None;

    let kernel = Box::leak(Box::new(Kernel::new(pool.clone())));
    kernel.proc_init();
    let init = kernel
        .cur_task(0)
        .expect("group-exit init should be current");
    assert!(init.process.set_signal_action(
        SIGCHLD,
        SigAction {
            handler: 0x5000,
            mask: 0,
        },
    ));
    let leader = kernel
        .tasks
        .fork_process(&init)
        .expect("group-exit child should fork");
    let waiter = kernel
        .tasks
        .clone_thread(&leader, 0x8000_0000, 0)
        .expect("group-exit pipe waiter should clone");
    let timer_waiter = kernel
        .tasks
        .clone_thread(&leader, 0x8100_0000, 0)
        .expect("group-exit timer waiter should clone");
    let futex_waiter = kernel
        .tasks
        .clone_thread(&leader, 0x8200_0000, 0)
        .expect("group-exit futex waiter should clone");
    let epoll_waiter = kernel
        .tasks
        .clone_thread(&leader, 0x8300_0000, 0)
        .expect("group-exit epoll waiter should clone");
    let record_waiter = kernel
        .tasks
        .clone_thread(&leader, 0x8400_0000, 0)
        .expect("group-exit record-lock waiter should clone");

    let (blocked_reader, external_writer) = PipeNode::pair();
    let (external_reader, owned_writer) = PipeNode::pair();
    leader
        .add_file(FLike::Pipe(blocked_reader.clone()))
        .expect("process should own its blocked pipe reader");
    leader
        .add_file(FLike::Pipe(owned_writer))
        .expect("process should own a writer for EOF observation");
    *GROUP_EXIT_PIPE_READER.lock().unwrap() = Some(blocked_reader.clone());
    let epoll = EpInst::new();
    *GROUP_EXIT_EPOLL.lock().unwrap() = Some(epoll.clone());
    let record_request = RecordLockRequest {
        identity: RECORD_LOCK_FILE_A,
        kind: Some(LockKind::Write),
        range: LockRange {
            start: 0,
            end: Some(10),
        },
    };
    kernel
        .record_locks
        .set_nonblocking(RECORD_LOCK_BLOCKER_PID, record_request)
        .expect("record-lock blocker should install");
    *GROUP_EXIT_RECORD_REQUEST.lock().unwrap() = Some(record_request);

    waiter
        .install_test_kernel_entry(group_exit_pipe_waiter_task)
        .expect("pipe waiter should receive its kernel entry");
    waiter.set_sched_state(TaskRunState::Runnable);
    kernel.run_queue.enqueue(&waiter);
    timer_waiter
        .install_test_kernel_entry(group_exit_timer_waiter_task)
        .expect("timer waiter should receive its kernel entry");
    futex_waiter
        .install_test_kernel_entry(group_exit_futex_waiter_task)
        .expect("futex waiter should receive its kernel entry");
    epoll_waiter
        .install_test_kernel_entry(group_exit_epoll_waiter_task)
        .expect("epoll waiter should receive its kernel entry");
    record_waiter
        .install_test_kernel_entry(group_exit_record_waiter_task)
        .expect("record-lock waiter should receive its kernel entry");
    for task in [&timer_waiter, &futex_waiter, &epoll_waiter, &record_waiter] {
        task.set_sched_state(TaskRunState::Runnable);
        kernel.run_queue.enqueue(task);
    }
    leader
        .install_test_kernel_entry(group_exit_caller_task)
        .expect("group-exit caller should receive its kernel entry");
    kernel.set_cur(0, None);
    install_kernel(kernel);

    assert!(kernel.run_one_cpu0_task_for_test());
    assert_eq!(waiter.sched_state(), TaskRunState::Sleeping);
    assert!(waiter.has_active_wait());
    assert_eq!(blocked_reader.pending_read_waiters(), 1);
    for task in [&timer_waiter, &futex_waiter, &epoll_waiter, &record_waiter] {
        assert!(kernel.run_one_cpu0_task_for_test());
        assert_eq!(task.sched_state(), TaskRunState::Sleeping);
        assert!(task.has_active_wait());
    }
    assert_eq!(global_timer_wheel().lock().active_count(), 1);
    assert_eq!(leader.process.futex.pending_at(FUTEX_ROUND_TRIP_ADDR), 1);
    assert_eq!(epoll.pending_waiters(), 1);
    assert!(kernel.record_locks.process_is_waiting(leader.process.pid()));

    leader.set_sched_state(TaskRunState::Runnable);
    kernel.run_queue.enqueue(&leader);
    assert!(kernel.run_one_cpu0_task_for_test());
    assert!(GROUP_EXIT_CALLER_RAN.load(Ordering::Relaxed));
    assert!(leader.done());
    assert!(leader.process.is_terminating());
    assert!(!leader.process.is_zombie());
    assert_eq!(
        kernel.do_wait(init.id(), leader.process.pid() as isize, 1),
        Ok((0, 0))
    );
    for task in [
        &waiter,
        &timer_waiter,
        &futex_waiter,
        &epoll_waiter,
        &record_waiter,
    ] {
        assert_eq!(task.sched_state(), TaskRunState::Runnable);
    }

    for _ in 0..5 {
        assert!(kernel.run_one_cpu0_task_for_test());
    }
    assert_eq!(
        *GROUP_EXIT_PIPE_RESULT.lock().unwrap(),
        Some(Err("group_exit"))
    );
    assert_eq!(
        *GROUP_EXIT_TIMER_RESULT.lock().unwrap(),
        Some(WaitOutcome::GroupExit)
    );
    assert_eq!(
        *GROUP_EXIT_FUTEX_RESULT.lock().unwrap(),
        Some(Err("group_exit"))
    );
    assert_eq!(
        *GROUP_EXIT_EPOLL_RESULT.lock().unwrap(),
        Some(WaitOutcome::GroupExit)
    );
    assert_eq!(
        *GROUP_EXIT_RECORD_RESULT.lock().unwrap(),
        Some(Err("group_exit"))
    );
    assert_eq!(blocked_reader.pending_read_waiters(), 0);
    assert_eq!(global_timer_wheel().lock().active_count(), 0);
    assert_eq!(leader.process.futex.pending_at(FUTEX_ROUND_TRIP_ADDR), 0);
    assert_eq!(epoll.pending_waiters(), 0);
    assert!(!kernel.record_locks.process_is_waiting(leader.process.pid()));
    assert!(leader.process.is_zombie());
    assert_eq!(leader.process.zombie_wait_status(), Some(37 << 8));
    assert_eq!(leader.process.thread_count(), 0);
    assert!(kernel.tasks.find_task(leader.id()).is_none());
    assert!(kernel.tasks.find_task(waiter.id()).is_none());
    assert!(kernel.tasks.find_task(timer_waiter.id()).is_none());
    assert!(kernel.tasks.find_task(futex_waiter.id()).is_none());
    assert!(kernel.tasks.find_task(epoll_waiter.id()).is_none());
    assert!(kernel.tasks.find_task(record_waiter.id()).is_none());
    assert_eq!(kernel.tasks.task_count(), 1);
    assert_eq!(
        init.process
            .sig_queue
            .lock()
            .unwrap()
            .iter()
            .filter(|(signo, sender)| {
                *signo == SIGCHLD as i32 && *sender == leader.process.pid() as isize
            })
            .count(),
        1
    );

    drop(blocked_reader);
    assert_eq!(
        external_writer.write_at(0, true, b"x"),
        Ok(PipeWriteOutcome::Broken { written: 0 })
    );
    let mut byte = [0u8; 1];
    assert_eq!(external_reader.read_at(0, true, &mut byte), Ok(0));

    *GROUP_EXIT_PIPE_READER.lock().unwrap() = None;
    *GROUP_EXIT_PIPE_RESULT.lock().unwrap() = None;
    *GROUP_EXIT_TIMER_RESULT.lock().unwrap() = None;
    *GROUP_EXIT_FUTEX_RESULT.lock().unwrap() = None;
    *GROUP_EXIT_EPOLL.lock().unwrap() = None;
    *GROUP_EXIT_EPOLL_RESULT.lock().unwrap() = None;
    *GROUP_EXIT_RECORD_REQUEST.lock().unwrap() = None;
    *GROUP_EXIT_RECORD_RESULT.lock().unwrap() = None;
    kernel.record_locks.release_process(RECORD_LOCK_BLOCKER_PID);
    clear_wait_token_state();
}

// AGENT: block one sibling inside PipeNode::read_at, then unwind its queue and
// local endpoint ownership before acknowledging group exit on that same stack.
extern "C" fn group_exit_pipe_waiter_task() -> ! {
    let reader = GROUP_EXIT_PIPE_READER
        .lock()
        .unwrap()
        .take()
        .expect("group-exit pipe reader should be installed");
    let kernel = crate::kernel::global_kernel().expect("group-exit kernel should be installed");
    let task = kernel
        .cur_task(0)
        .expect("group-exit pipe waiter should be current");
    let mut byte = [0u8; 1];
    let result = reader.read_at(task.id(), false, &mut byte);
    drop(reader);
    *GROUP_EXIT_PIPE_RESULT.lock().unwrap() = Some(result);
    assert_eq!(kernel.retire_current_group_member(0, &task), Ok(()));
    drop(task);
    kernel.switch_current_to_idle(0);
    loop {
        core::hint::spin_loop();
    }
}

// AGENT: prove a group-exit wake cancels a registered deadline when the owner
// resumes and unwinds WaitToken::wait_inner on its own stack.
extern "C" fn group_exit_timer_waiter_task() -> ! {
    let kernel = crate::kernel::global_kernel().expect("group-exit kernel should be installed");
    let task = kernel
        .cur_task(0)
        .expect("group-exit timer waiter should be current");
    let token = WaitToken::for_task(task.id());
    let deadline = CLK.load(Ordering::Relaxed).saturating_add(100);
    let outcome = token.wait_interruptible(Some(deadline));
    *GROUP_EXIT_TIMER_RESULT.lock().unwrap() = Some(outcome);
    assert_eq!(kernel.retire_current_group_member(0, &task), Ok(()));
    drop(task);
    kernel.switch_current_to_idle(0);
    loop {
        core::hint::spin_loop();
    }
}

// AGENT: exercise FutexBucket's normal finish_wait unlink path after GroupExit.
extern "C" fn group_exit_futex_waiter_task() -> ! {
    let kernel = crate::kernel::global_kernel().expect("group-exit kernel should be installed");
    let task = kernel
        .cur_task(0)
        .expect("group-exit futex waiter should be current");
    let result = task
        .process
        .futex
        .wait(task.id(), FUTEX_ROUND_TRIP_ADDR, 1, None, || Ok(1));
    *GROUP_EXIT_FUTEX_RESULT.lock().unwrap() = Some(result);
    assert_eq!(kernel.retire_current_group_member(0, &task), Ok(()));
    drop(task);
    kernel.switch_current_to_idle(0);
    loop {
        core::hint::spin_loop();
    }
}

// AGENT: mirror sys_epoll_wait's unconditional token removal before reaching
// the common group-exit safe point.
extern "C" fn group_exit_epoll_waiter_task() -> ! {
    let epoll = GROUP_EXIT_EPOLL
        .lock()
        .unwrap()
        .as_ref()
        .expect("group-exit epoll should be installed")
        .clone();
    let kernel = crate::kernel::global_kernel().expect("group-exit kernel should be installed");
    let task = kernel
        .cur_task(0)
        .expect("group-exit epoll waiter should be current");
    let token = epoll
        .prepare_wait(task.id())
        .expect("empty epoll should publish a waiter");
    let outcome = token.wait_interruptible(None);
    epoll.remove_waiter(&token);
    *GROUP_EXIT_EPOLL_RESULT.lock().unwrap() = Some(outcome);
    assert_eq!(kernel.retire_current_group_member(0, &task), Ok(()));
    drop(task);
    kernel.switch_current_to_idle(0);
    loop {
        core::hint::spin_loop();
    }
}

// AGENT: run the ordinary blocking record-lock cleanup so its wait dependency
// is deleted before this member acknowledges group exit.
extern "C" fn group_exit_record_waiter_task() -> ! {
    let request = GROUP_EXIT_RECORD_REQUEST
        .lock()
        .unwrap()
        .expect("group-exit record request should be installed");
    let kernel = crate::kernel::global_kernel().expect("group-exit kernel should be installed");
    let task = kernel
        .cur_task(0)
        .expect("group-exit record waiter should be current");
    let result = kernel
        .record_locks
        .set_blocking(task.process.pid(), task.id(), request);
    *GROUP_EXIT_RECORD_RESULT.lock().unwrap() = Some(result);
    assert_eq!(kernel.retire_current_group_member(0, &task), Ok(()));
    drop(task);
    kernel.switch_current_to_idle(0);
    loop {
        core::hint::spin_loop();
    }
}

// AGENT: issue exit_group from the leader's own live stack and return to idle;
// the blocked sibling remains the only process member after this handoff.
extern "C" fn group_exit_caller_task() -> ! {
    GROUP_EXIT_CALLER_RAN.store(true, Ordering::Relaxed);
    let kernel = crate::kernel::global_kernel().expect("group-exit kernel should be installed");
    let task = kernel
        .cur_task(0)
        .expect("group-exit leader should be current");
    task.process.set_job_stopped(true);
    assert_eq!(
        kernel.exit_thread_group(0, &task, crate::kernel::ExitReason::Code(37)),
        Ok(())
    );
    assert!(!task.process.is_job_stopped());
    drop(task);
    kernel.switch_current_to_idle(0);
    loop {
        core::hint::spin_loop();
    }
}

// AGENT: cover zero, sub-tick, exact-tick, just-over-tick, and whole-second
// conversions so relative waits retain their round-up contract.
#[cfg_attr(test, test)]
fn duration_to_ticks_rounds_up_at_tick_boundaries() {
    let tick_nanos = 1_000_000_000u64 / TIMER_TICK_HZ as u64;

    assert_eq!(duration_to_ticks(Duration::ZERO), 0);
    assert_eq!(duration_to_ticks(Duration::from_nanos(1)), 1);
    assert_eq!(duration_to_ticks(Duration::from_nanos(tick_nanos)), 1);
    assert_eq!(duration_to_ticks(Duration::from_nanos(tick_nanos + 1)), 2);
    assert_eq!(duration_to_ticks(Duration::from_secs(1)), TIMER_TICK_HZ);
}

// AGENT: reset the scheduler backend and timer state so QEMU boot selftests are
// deterministic without publishing a second current-task marker.
fn reset_wait_token_state() {
    clear_global_kernel_for_test();
    CLK.store(0, Ordering::Relaxed);
    ensure_timer_wheel();
    *global_timer_wheel().lock() = TimerWheel::new();
}

// AGENT: leave no scheduler backend behind for later selftests.
fn clear_wait_token_state() {
    clear_global_kernel_for_test();
}

// AGENT: ordinary Rust tests may enter without rust_main(), while QEMU boot
// selftests enter after rust_main() has already initialized the once cell.
fn ensure_timer_wheel() {
    if TIMER_WHEEL.get().is_none() {
        init_timer_wheel();
    }
}

// AGENT: WaitToken::for_task binds an explicitly selected task id and gives each
// wait distinct Arc-backed state without a separate wait identifier.
#[cfg_attr(test, test)]
fn wait_token_binds_selected_task() {
    reset_wait_token_state();

    let first = WaitToken::for_task(11);
    let second = WaitToken::for_task(11);

    assert_eq!(first.task_id(), 11);
    assert_eq!(second.task_id(), 11);
    assert!(!first.same(&second));
    assert!(!first.is_woken());

    clear_wait_token_state();
}

// AGENT: event wakeups complete a pending token exactly once and must beat later
// timeout attempts.
#[cfg_attr(test, test)]
fn wait_token_event_wake_wins_once() {
    reset_wait_token_state();

    let token = WaitToken::for_task(12);

    assert!(token.wake_event());
    assert!(!token.wake());
    assert!(!token.wake_timeout());
    assert!(token.is_woken());
    assert!(!token.is_timeout());
    assert_eq!(token.outcome(), WaitOutcome::Event);
    assert_eq!(token.wait(None), WaitOutcome::Event);

    clear_wait_token_state();
}

// AGENT: timeout wakeups complete a pending token exactly once and must beat
// later event attempts.
#[cfg_attr(test, test)]
fn wait_token_timeout_wake_wins_once() {
    reset_wait_token_state();

    let token = WaitToken::for_task(13);

    assert!(token.wake_timeout());
    assert!(!token.wake_event());
    assert!(!token.wake());
    assert!(token.is_woken());
    assert!(token.is_timeout());
    assert_eq!(token.outcome(), WaitOutcome::Timeout);
    assert_eq!(token.wait(None), WaitOutcome::Timeout);

    clear_wait_token_state();
}

// AGENT: a plain wait whose absolute deadline is the current tick must finish
// as a timeout without touching the timer wheel or entering the wait loop.
#[cfg_attr(test, test)]
fn wait_token_current_deadline_times_out_without_timer_wheel() {
    reset_wait_token_state();

    let token = WaitToken::for_task(14);

    assert_eq!(token.wait(Some(0)), WaitOutcome::Timeout);
    assert!(token.is_timeout());
    assert_eq!(global_timer_wheel().lock().active_count(), 0);

    clear_wait_token_state();
}

// AGENT: already-expired absolute deadlines must timeout immediately instead of
// registering a timer and spinning.
#[cfg_attr(test, test)]
fn wait_token_expired_deadline_times_out_immediately() {
    reset_wait_token_state();

    CLK.store(7, Ordering::Relaxed);
    let token = WaitToken::for_task(15);

    assert_eq!(token.wait_interruptible(Some(7)), WaitOutcome::Timeout);
    assert!(token.is_timeout());
    assert_eq!(global_timer_wheel().lock().active_count(), 0);

    clear_wait_token_state();
}

// AGENT: the QEMU timer wheel dispatches TimerTarget::WakeToken through the same
// timeout marker used by WaitToken's unified deadline path.
fn wait_token_timer_target_times_out_on_schedule_tick(pool: &FramePool) {
    reset_wait_token_state();

    let kernel = Kernel::new(pool.clone());
    let token = WaitToken::for_task(16);
    let deadline = CLK.load(Ordering::Relaxed) + 1;

    {
        let mut timers = global_timer_wheel().lock();
        timers.register_timer(
            deadline,
            0,
            TimerTarget::WakeToken {
                token: token.clone(),
            },
        );
        assert_eq!(timers.active_count(), 1);
    }

    kernel.schedule_tick(0);

    assert!(token.is_timeout());
    assert_eq!(token.outcome(), WaitOutcome::Timeout);
    assert_eq!(global_timer_wheel().lock().active_count(), 0);

    clear_wait_token_state();
}

// AGENT: when a QEMU scheduler backend is installed, a token event wake should
// make the sleeping owner task runnable through Kernel::wake_task_for_wait().
fn wait_token_event_wake_uses_installed_scheduler_backend(pool: &FramePool) {
    reset_wait_token_state();

    let kernel = Box::leak(Box::new(Kernel::new(pool.clone())));
    let task = kernel.tasks.spawn_root().expect("spawn test init task");
    install_kernel(kernel);
    let token = WaitToken::for_task(task.id());
    task.set_sched_state(TaskRunState::Running);
    assert!(task.install_active_wait(token.clone()));

    assert!(token.wake_event());
    assert_eq!(task.sched_state(), TaskRunState::Runnable);
    assert_eq!(kernel.run_queue.pick_next(), Some(task.id()));

    clear_wait_token_state();
}

// AGENT: interruptible waits must distinguish pending signals from real event
// readiness so syscall callers can return EINTR.
fn wait_token_interruptible_wait_reports_signal_not_event(pool: &FramePool) {
    reset_wait_token_state();

    let kernel = Box::leak(Box::new(Kernel::new(pool.clone())));
    kernel.proc_init();
    install_kernel(kernel);
    let task = kernel.cur_task(0).expect("init task should be current");
    let token = WaitToken::for_task(task.id());

    kernel.send_signal_to_task(&task, SIGUSR1 as i32, -1);

    assert_eq!(token.wait_interruptible(None), WaitOutcome::Signal);
    assert_eq!(token.outcome(), WaitOutcome::Signal);
    assert_eq!(task.sched_state(), TaskRunState::Running);

    clear_wait_token_state();
}

// AGENT: a masked signal remains pending without waking a Sleeping task or
// completing its WaitToken; only the watched event resumes this wait.
fn wait_token_stays_blocked_after_masked_signal(pool: &FramePool) {
    reset_wait_token_state();

    let kernel = Box::leak(Box::new(Kernel::new(pool.clone())));
    kernel.proc_init();
    let task = kernel.cur_task(0).expect("init task should be current");
    *task.sig_mask.lock().unwrap() = signal_bit(SIGUSR1).expect("SIGUSR1 should have a mask bit");
    let token = WaitToken::for_task(task.id());
    *WAIT_ROUND_TRIP_TOKEN.lock().unwrap() = Some(token.clone());
    *WAIT_ROUND_TRIP_OUTCOME.lock().unwrap() = None;
    task.install_test_kernel_entry(wait_round_trip_test_task)
        .expect("wait test task should receive kernel entry");
    install_kernel(kernel);

    assert!(kernel.run_one_cpu0_task_for_test());
    assert_eq!(task.sched_state(), TaskRunState::Sleeping);
    assert!(!token.is_woken());

    kernel.send_signal_to_task(&task, SIGUSR1 as i32, -1);
    assert_eq!(task.sched_state(), TaskRunState::Sleeping);
    assert!(!token.is_woken());
    assert_eq!(*WAIT_ROUND_TRIP_OUTCOME.lock().unwrap(), None);

    assert!(token.wake_event());
    assert!(kernel.run_one_cpu0_task_for_test());
    assert_eq!(
        *WAIT_ROUND_TRIP_OUTCOME.lock().unwrap(),
        Some(WaitOutcome::Event)
    );
    assert!(task.done());

    *WAIT_ROUND_TRIP_TOKEN.lock().unwrap() = None;
    clear_wait_token_state();
}

// AGENT: prove that a signal arriving only after the task has switched to idle
// resumes the kernel wait frame and is surfaced as Signal rather than Event.
fn wait_token_sleeping_wait_reports_later_signal(pool: &FramePool) {
    reset_wait_token_state();

    let kernel = Box::leak(Box::new(Kernel::new(pool.clone())));
    kernel.proc_init();
    let task = kernel.cur_task(0).expect("init task should be current");
    let token = WaitToken::for_task(task.id());
    *WAIT_ROUND_TRIP_TOKEN.lock().unwrap() = Some(token.clone());
    *WAIT_ROUND_TRIP_OUTCOME.lock().unwrap() = None;
    task.install_test_kernel_entry(wait_round_trip_test_task)
        .expect("wait test task should receive kernel entry");
    install_kernel(kernel);

    assert!(kernel.run_one_cpu0_task_for_test());
    assert_eq!(task.sched_state(), TaskRunState::Sleeping);
    assert!(!token.is_woken());

    kernel.send_signal_to_task(&task, SIGUSR1 as i32, -1);
    assert_eq!(task.sched_state(), TaskRunState::Runnable);
    assert!(kernel.run_one_cpu0_task_for_test());
    assert_eq!(
        *WAIT_ROUND_TRIP_OUTCOME.lock().unwrap(),
        Some(WaitOutcome::Signal)
    );
    assert!(task.done());

    *WAIT_ROUND_TRIP_TOKEN.lock().unwrap() = None;
    clear_wait_token_state();
}

// AGENT: enter wait_interruptible() on a real task kernel stack, then mark only
// the test task exited and use the production task-to-idle handoff; invoking
// process-level exit here would trigger the real init-exit shutdown policy.
extern "C" fn wait_round_trip_test_task() -> ! {
    let token = WAIT_ROUND_TRIP_TOKEN
        .lock()
        .unwrap()
        .as_ref()
        .expect("wait round-trip token should be installed")
        .clone();
    let outcome = token.wait_interruptible(None);
    *WAIT_ROUND_TRIP_OUTCOME.lock().unwrap() = Some(outcome);

    let kernel = crate::kernel::global_kernel().expect("wait test kernel should be installed");
    let task = kernel
        .cur_task(0)
        .expect("wait test task should still be current");
    task.mark_thread_exited();
    drop(task);
    kernel.switch_current_to_idle(0);
    loop {
        core::hint::spin_loop();
    }
}

// AGENT: FutexBucket::wait must compare the current futex word before enqueueing
// and return the syscall-layer "changed" marker when it differs.
#[cfg_attr(test, test)]
fn futex_wait_returns_changed_without_queueing() {
    reset_wait_token_state();

    let futex = FutexBucket::new();
    let addr = 0x4000;
    let calls = AtomicUsize::new(0);

    let err = futex
        .wait(18, addr, 1, None, || {
            calls.fetch_add(1, Ordering::Relaxed);
            Ok(0)
        })
        .expect_err("different futex word should not sleep");

    assert_eq!(err, "changed");
    assert_eq!(calls.load(Ordering::Relaxed), 1);
    assert_eq!(futex.pending_at(addr), 0);

    clear_wait_token_state();
}

// AGENT: failed userspace copy-in should bubble out of wait setup without
// publishing a stale waiter or panicking inside the futex bucket.
#[cfg_attr(test, test)]
fn futex_wait_propagates_word_read_fault() {
    reset_wait_token_state();

    let futex = FutexBucket::new();
    let addr = 0x5000;

    let err = futex
        .wait(19, addr, 1, None, || Err("efault"))
        .expect_err("read fault should abort wait setup");

    assert_eq!(err, "efault");
    assert_eq!(futex.pending_at(addr), 0);

    clear_wait_token_state();
}

// AGENT: a matching word with an already-expired absolute deadline proves the
// waiter is published first, then removed by finish_wait() on timeout.
#[cfg_attr(test, test)]
fn futex_wait_timeout_removes_published_waiter() {
    reset_wait_token_state();

    let futex = FutexBucket::new();
    let addr = 0x6000;
    let calls = AtomicUsize::new(0);

    let err = futex
        .wait(20, addr, 1, Some(0), || {
            calls.fetch_add(1, Ordering::Relaxed);
            Ok(1)
        })
        .expect_err("zero timeout should finish as timeout");

    assert_eq!(err, "timeout");
    assert_eq!(calls.load(Ordering::Relaxed), 1);
    assert_eq!(futex.pending_at(addr), 0);

    clear_wait_token_state();
}

// AGENT: a real FUTEX_WAKE round trip must return success and leave no waiter,
// even though the event producer currently also unlinks the selected entry.
fn futex_wait_event_removes_published_waiter(pool: &FramePool) {
    reset_wait_token_state();

    let kernel = Box::leak(Box::new(Kernel::new(pool.clone())));
    kernel.proc_init();
    let task = kernel.cur_task(0).expect("init task should be current");
    let futex = Arc::new(FutexBucket::new());
    *FUTEX_ROUND_TRIP_BUCKET.lock().unwrap() = Some(futex.clone());
    *FUTEX_ROUND_TRIP_RESULT.lock().unwrap() = None;
    task.install_test_kernel_entry(futex_wait_round_trip_test_task)
        .expect("futex wait test task should receive kernel entry");
    install_kernel(kernel);

    assert!(kernel.run_one_cpu0_task_for_test());
    assert_eq!(task.sched_state(), TaskRunState::Sleeping);
    assert_eq!(futex.pending_at(FUTEX_ROUND_TRIP_ADDR), 1);
    assert_eq!(*FUTEX_ROUND_TRIP_RESULT.lock().unwrap(), None);

    assert_eq!(futex.wake(FUTEX_ROUND_TRIP_ADDR, 1), 1);
    assert_eq!(task.sched_state(), TaskRunState::Runnable);
    assert!(kernel.run_one_cpu0_task_for_test());

    assert_eq!(*FUTEX_ROUND_TRIP_RESULT.lock().unwrap(), Some(Ok(())));
    assert_eq!(futex.pending_at(FUTEX_ROUND_TRIP_ADDR), 0);
    assert!(task.done());

    *FUTEX_ROUND_TRIP_BUCKET.lock().unwrap() = None;
    *FUTEX_ROUND_TRIP_RESULT.lock().unwrap() = None;
    clear_wait_token_state();
}

// AGENT: an interrupting signal must return EINTR and let finish_wait unlink
// the published futex waiter rather than leaving a completed queue entry.
fn futex_wait_signal_removes_published_waiter(pool: &FramePool) {
    reset_wait_token_state();

    let kernel = Box::leak(Box::new(Kernel::new(pool.clone())));
    kernel.proc_init();
    let task = kernel.cur_task(0).expect("init task should be current");
    let futex = Arc::new(FutexBucket::new());
    *FUTEX_ROUND_TRIP_BUCKET.lock().unwrap() = Some(futex.clone());
    *FUTEX_ROUND_TRIP_RESULT.lock().unwrap() = None;
    task.install_test_kernel_entry(futex_wait_round_trip_test_task)
        .expect("futex wait test task should receive kernel entry");
    install_kernel(kernel);

    assert!(kernel.run_one_cpu0_task_for_test());
    assert_eq!(task.sched_state(), TaskRunState::Sleeping);
    assert_eq!(futex.pending_at(FUTEX_ROUND_TRIP_ADDR), 1);
    assert_eq!(*FUTEX_ROUND_TRIP_RESULT.lock().unwrap(), None);

    kernel.send_signal_to_task(&task, SIGUSR1 as i32, -1);
    assert_eq!(task.sched_state(), TaskRunState::Runnable);
    assert!(kernel.run_one_cpu0_task_for_test());

    assert_eq!(*FUTEX_ROUND_TRIP_RESULT.lock().unwrap(), Some(Err("eintr")));
    assert_eq!(futex.pending_at(FUTEX_ROUND_TRIP_ADDR), 0);
    assert!(task.done());

    *FUTEX_ROUND_TRIP_BUCKET.lock().unwrap() = None;
    *FUTEX_ROUND_TRIP_RESULT.lock().unwrap() = None;
    clear_wait_token_state();
}

// AGENT: execute FutexBucket::wait on a real task kernel stack and publish its
// terminal result before returning to the test driver's idle context.
extern "C" fn futex_wait_round_trip_test_task() -> ! {
    let futex = FUTEX_ROUND_TRIP_BUCKET
        .lock()
        .unwrap()
        .as_ref()
        .expect("futex round-trip bucket should be installed")
        .clone();
    let kernel = crate::kernel::global_kernel().expect("futex test kernel should be installed");
    let task_id = kernel
        .cur_task(0)
        .expect("futex test task should be current")
        .id();
    let result = futex.wait(task_id, FUTEX_ROUND_TRIP_ADDR, 1, None, || Ok(1));
    *FUTEX_ROUND_TRIP_RESULT.lock().unwrap() = Some(result);

    let task = kernel
        .cur_task(0)
        .expect("futex test task should still be current");
    task.mark_thread_exited();
    drop(task);
    kernel.switch_current_to_idle(0);
    loop {
        core::hint::spin_loop();
    }
}

// AGENT: run F_SETLKW on a real task stack, prove it sleeps behind another PID,
// reject the reverse dependency as EDEADLK, then resume after unlock broadcast.
fn record_lock_blocking_wait_wakes_and_detects_deadlock(pool: &FramePool) {
    reset_wait_token_state();
    let kernel = Box::leak(Box::new(Kernel::new(pool.clone())));
    kernel.proc_init();
    let task = kernel
        .cur_task(0)
        .expect("record-lock test task should exist");
    let owned_a = RecordLockRequest {
        identity: RECORD_LOCK_FILE_A,
        kind: Some(LockKind::Write),
        range: LockRange {
            start: 0,
            end: Some(10),
        },
    };
    let owned_b = RecordLockRequest {
        identity: RECORD_LOCK_FILE_B,
        kind: Some(LockKind::Write),
        range: LockRange {
            start: 0,
            end: Some(10),
        },
    };
    kernel
        .record_locks
        .set_nonblocking(RECORD_LOCK_WAITER_PID, owned_a)
        .expect("waiter setup lock should install");
    kernel
        .record_locks
        .set_nonblocking(RECORD_LOCK_BLOCKER_PID, owned_b)
        .expect("blocker setup lock should install");
    *RECORD_LOCK_ROUND_TRIP_REQUEST.lock().unwrap() = Some(owned_b);
    *RECORD_LOCK_ROUND_TRIP_RESULT.lock().unwrap() = None;
    task.install_test_kernel_entry(record_lock_round_trip_test_task)
        .expect("record-lock wait task should receive kernel entry");
    install_kernel(kernel);

    assert!(kernel.run_one_cpu0_task_for_test());
    assert_eq!(task.sched_state(), TaskRunState::Sleeping);
    assert_eq!(*RECORD_LOCK_ROUND_TRIP_RESULT.lock().unwrap(), None);
    assert_eq!(
        kernel
            .record_locks
            .set_blocking(RECORD_LOCK_BLOCKER_PID, task.id(), owned_a),
        Err("edeadlk")
    );

    kernel
        .record_locks
        .release_file(RECORD_LOCK_BLOCKER_PID, RECORD_LOCK_FILE_B);
    assert_eq!(task.sched_state(), TaskRunState::Runnable);
    assert!(kernel.run_one_cpu0_task_for_test());
    assert_eq!(*RECORD_LOCK_ROUND_TRIP_RESULT.lock().unwrap(), Some(Ok(())));
    assert!(task.done());

    kernel.record_locks.release_process(RECORD_LOCK_WAITER_PID);
    *RECORD_LOCK_ROUND_TRIP_REQUEST.lock().unwrap() = None;
    *RECORD_LOCK_ROUND_TRIP_RESULT.lock().unwrap() = None;
    clear_wait_token_state();
}

// AGENT: prove a pending signal wins an interruptible F_SETLKW wait and removes
// the process wait edge instead of granting the lock or leaving a stale token.
fn record_lock_blocking_wait_returns_eintr(pool: &FramePool) {
    reset_wait_token_state();
    let kernel = Box::leak(Box::new(Kernel::new(pool.clone())));
    kernel.proc_init();
    let task = kernel
        .cur_task(0)
        .expect("record-lock signal task should exist");
    let request = RecordLockRequest {
        identity: RECORD_LOCK_FILE_A,
        kind: Some(LockKind::Write),
        range: LockRange {
            start: 5,
            end: Some(15),
        },
    };
    kernel
        .record_locks
        .set_nonblocking(RECORD_LOCK_BLOCKER_PID, request)
        .expect("record-lock signal blocker should install");
    *RECORD_LOCK_ROUND_TRIP_REQUEST.lock().unwrap() = Some(request);
    *RECORD_LOCK_ROUND_TRIP_RESULT.lock().unwrap() = None;
    task.install_test_kernel_entry(record_lock_round_trip_test_task)
        .expect("record-lock signal task should receive kernel entry");
    install_kernel(kernel);

    assert!(kernel.run_one_cpu0_task_for_test());
    assert_eq!(task.sched_state(), TaskRunState::Sleeping);
    kernel.send_signal_to_task(&task, SIGUSR1 as i32, -1);
    assert_eq!(task.sched_state(), TaskRunState::Runnable);
    assert!(kernel.run_one_cpu0_task_for_test());
    assert_eq!(
        *RECORD_LOCK_ROUND_TRIP_RESULT.lock().unwrap(),
        Some(Err("eintr"))
    );
    assert!(task.done());
    assert!(!kernel
        .record_locks
        .process_has_locks(RECORD_LOCK_WAITER_PID));

    kernel.record_locks.release_process(RECORD_LOCK_BLOCKER_PID);
    *RECORD_LOCK_ROUND_TRIP_REQUEST.lock().unwrap() = None;
    *RECORD_LOCK_ROUND_TRIP_RESULT.lock().unwrap() = None;
    clear_wait_token_state();
}

// AGENT: execute the shared record-lock blocking API from a schedulable kernel
// context and return to the test driver's idle stack after publishing the result.
extern "C" fn record_lock_round_trip_test_task() -> ! {
    let request = RECORD_LOCK_ROUND_TRIP_REQUEST
        .lock()
        .unwrap()
        .expect("record-lock round-trip request should be installed");
    let kernel = crate::kernel::global_kernel().expect("record-lock test kernel should exist");
    let task = kernel
        .cur_task(0)
        .expect("record-lock test task should be current");
    let result = kernel
        .record_locks
        .set_blocking(RECORD_LOCK_WAITER_PID, task.id(), request);
    *RECORD_LOCK_ROUND_TRIP_RESULT.lock().unwrap() = Some(result);

    task.mark_thread_exited();
    drop(task);
    kernel.switch_current_to_idle(0);
    loop {
        core::hint::spin_loop();
    }
}

// AGENT: cmp_requeue now reads the source futex word through a caller-supplied
// copy-in closure; read errors should be returned instead of panicking.
#[cfg_attr(test, test)]
fn futex_cmp_requeue_propagates_word_read_fault() {
    reset_wait_token_state();

    let futex = FutexBucket::new();
    let src = 0x7000;
    let dst = 0x8000;

    let err = futex
        .cmp_requeue(src, dst, 1, 1, 1, || Err("efault"))
        .expect_err("cmp_requeue should propagate read faults");

    assert_eq!(err, "efault");
    assert_eq!(futex.pending_at(src), 0);
    assert_eq!(futex.pending_at(dst), 0);

    clear_wait_token_state();
}

// AGENT: a failed comparison must leave every waiter on the source queue and
// must not consume either the wake or move quota.
#[cfg_attr(test, test)]
fn futex_cmp_requeue_mismatch_preserves_waiters() {
    reset_wait_token_state();

    let futex = FutexBucket::new();
    let src = 0x7100;
    let dst = 0x8100;
    let waiter = WaitToken::for_task(23);
    futex.publish_waiter_for_test(src, waiter.clone());

    let err = futex
        .cmp_requeue(src, dst, 1, 1, 2, || Ok(1))
        .expect_err("a mismatched source word should reject cmp_requeue");

    assert_eq!(err, "changed");
    assert_eq!(futex.pending_at(src), 1);
    assert_eq!(futex.pending_at(dst), 0);
    assert!(!waiter.is_woken());
    assert_eq!(futex.wake(src, 1), 1);

    clear_wait_token_state();
}

// AGENT: a matching comparison wakes the first waiter, moves only the requested
// successor, returns wake+move, and makes the moved waiter visible at dst.
#[cfg_attr(test, test)]
fn futex_cmp_requeue_wakes_moves_and_returns_affected() {
    reset_wait_token_state();

    let futex = FutexBucket::new();
    let src = 0x7200;
    let dst = 0x8200;
    let first = WaitToken::for_task(24);
    let second = WaitToken::for_task(25);
    let third = WaitToken::for_task(26);
    futex.publish_waiter_for_test(src, first.clone());
    futex.publish_waiter_for_test(src, second.clone());
    futex.publish_waiter_for_test(src, third.clone());
    let reads = AtomicUsize::new(0);

    let affected = futex
        .cmp_requeue(src, dst, 1, 1, 7, || {
            reads.fetch_add(1, Ordering::Relaxed);
            Ok(7)
        })
        .expect("a matching source word should requeue waiters");

    assert_eq!(affected, 2);
    assert_eq!(reads.load(Ordering::Relaxed), 1);
    assert!(first.is_woken());
    assert!(!second.is_woken());
    assert!(!third.is_woken());
    assert_eq!(futex.pending_at(src), 1);
    assert_eq!(futex.pending_at(dst), 1);

    assert_eq!(futex.wake(dst, 1), 1);
    assert!(second.is_woken());
    assert!(!third.is_woken());
    assert_eq!(futex.wake(src, 1), 1);
    assert!(third.is_woken());

    clear_wait_token_state();
}

// AGENT: completed timeout entries must be discarded before requeue counts move
// slots, otherwise a stale waiter can consume move_n and leave a live waiter on src.
#[cfg_attr(test, test)]
fn futex_requeue_skips_completed_waiters_when_moving() {
    reset_wait_token_state();

    let src = 0x9000;
    let dst = 0xA000;
    let stale = WaitToken::for_task(22);
    let live = WaitToken::for_task(22);
    let mut waiters = VecDeque::new();

    assert!(stale.wake_timeout());
    waiters.push_back(FutexWaiter {
        addr: src,
        token: stale,
    });
    waiters.push_back(FutexWaiter {
        addr: src,
        token: live.clone(),
    });

    let result = FutexBucket::requeue_locked(&mut waiters, src, dst, 0, 1);

    assert_eq!(result.woken, 0);
    assert_eq!(result.moved, 1);
    assert_eq!(waiters.len(), 1);
    assert_eq!(waiters[0].addr, dst);
    assert!(waiters[0].token.same(&live));

    clear_wait_token_state();
}

// AGENT: REQUEUE and CMP_REQUEUE must resolve uaddr2 through the live Sv39 map,
// not accept every aligned numeric address below USER_TOP as a valid futex.
#[cfg_attr(test, test)]
fn futex_requeue_syscalls_reject_unmapped_destination(pool: &FramePool) {
    reset_wait_token_state();

    let kernel = Kernel::new(pool.clone());
    kernel.proc_init();
    let task = kernel.cur_task(0).expect("init task should be current");
    let src = 0x4100_0000;
    let unmapped_dst = 0x4200_0000;
    {
        let mut addr_space = task.process.addr_space.lock().unwrap();
        addr_space
            .map_region(
                VmRegion::new(src, PAGE_SZ, VM_READ | VM_WRITE),
                &kernel.pool,
            )
            .expect("source futex page should map");
        addr_space
            .write_user_bytes(src, &7u32.to_ne_bytes(), &kernel.pool)
            .expect("source futex word should be writable");
    }

    assert_eq!(
        kernel.dispatch_syscall(SYS_FUTEX, src, 3, 0, 0, unmapped_dst, 0),
        Err("efault")
    );
    assert_eq!(
        kernel.dispatch_syscall(SYS_FUTEX, src, 9, 0, 0, unmapped_dst, 7),
        Err("efault")
    );

    clear_wait_token_state();
}

// AGENT: exercise the generic id allocator through fd lower-bound allocation,
// dup3 exact placement/replacement, and close-driven reuse under one FdTable.
fn fd_allocator_supports_lower_bounds_fixed_targets_and_reuse(pool: &FramePool) {
    let kernel = Kernel::new(pool.clone());
    let task = kernel.tasks.spawn_root().expect("spawn fd allocator task");
    let source_fd = task
        .add_file(FLike::Ep(EpInst::new()))
        .expect("initial fd allocation should succeed");
    assert_eq!(source_fd, 0);

    let high_fd = task
        .dup_fd_from(source_fd, 5, false)
        .expect("lower-bound fd allocation should succeed");
    assert_eq!(high_fd, 5);
    let low_fd = task
        .dup_fd(source_fd, false)
        .expect("skipped low fd should remain allocatable");
    assert_eq!(low_fd, 1);
    let next_high_fd = task
        .dup_fd_from(source_fd, 5, false)
        .expect("next lower-bound fd allocation should succeed");
    assert_eq!(next_high_fd, 6);

    let exact_fd = kernel
        .dup3_task_fd(&task, source_fd, 2, false)
        .expect("dup3 exact fd allocation should succeed");
    assert_eq!(exact_fd, 2);
    task.set_cloexec(source_fd, true)
        .expect("source cloexec update should succeed");
    assert!(task
        .get_fd_entry(source_fd)
        .expect("source should remain open after cloexec update")
        .is_cloexec());
    let next_exact_fd = task
        .dup_fd_from(source_fd, 2, false)
        .expect("next exact-range fd allocation should succeed");
    assert_eq!(next_exact_fd, 3);

    let dup3_fd = kernel
        .dup3_task_fd(&task, source_fd, 4, true)
        .expect("dup3 exact fd allocation should succeed");
    assert_eq!(dup3_fd, 4);
    let dup3_entry = task
        .get_fd_entry(dup3_fd)
        .expect("dup3 exact target should be installed");
    assert!(dup3_entry.same_open_description(
        &task
            .get_fd_entry(source_fd)
            .expect("dup3 source should remain open")
    ));
    assert!(dup3_entry.is_cloexec());
    assert_eq!(
        kernel.dup3_task_fd(&task, source_fd, source_fd, false),
        Err("einval")
    );

    let pending_pair = task
        .add_file_pair_transaction(
            FdEntry::new(FLike::Ep(EpInst::new())),
            FdEntry::new(FLike::Ep(EpInst::new())),
            |first_fd, second_fd| {
                assert_eq!(
                    kernel.dup3_task_fd(&task, source_fd, first_fd, false),
                    Err("ebusy")
                );
                assert_eq!(
                    kernel.dup3_task_fd(&task, source_fd, second_fd, true),
                    Err("ebusy")
                );
                Ok(())
            },
        )
        .expect("pending pair should commit after dup target collisions");

    task.set_cloexec(high_fd, true)
        .expect("target cloexec update should succeed");
    let previous_target = task
        .get_fd_entry(high_fd)
        .expect("dup3 target should be open");
    assert_eq!(
        kernel.dup3_task_fd(&task, MAX_FD - 1, high_fd, false),
        Err("ebadf")
    );
    assert!(task
        .get_fd_entry(high_fd)
        .expect("invalid source must not close dup3 target")
        .same_open_description(&previous_target));

    assert_eq!(
        kernel.dup3_task_fd(&task, source_fd, high_fd, false),
        Ok(high_fd)
    );
    let replaced = task
        .get_fd_entry(high_fd)
        .expect("dup3 target should hold the source OFD");
    assert!(replaced.same_open_description(
        &task
            .get_fd_entry(source_fd)
            .expect("dup3 source should remain open")
    ));
    assert!(!replaced.is_cloexec());
    task.close_fd(high_fd)
        .expect("closing the replaced fd should succeed");
    assert_eq!(task.dup_fd_from(source_fd, high_fd, false), Ok(high_fd));

    let _ = task.close_fd(source_fd);
    let _ = task.close_fd(low_fd);
    let _ = task.close_fd(exact_fd);
    let _ = task.close_fd(next_exact_fd);
    let _ = task.close_fd(dup3_fd);
    let _ = task.close_fd(pending_pair.0);
    let _ = task.close_fd(pending_pair.1);
    let _ = task.close_fd(high_fd);
    let _ = task.close_fd(next_high_fd);
}

// AGENT: closing a watched fd must remove the old epoll interest and cancel its
// pipe source callback before the same fd number can be reused for another file.
fn fd_close_detaches_epoll_subscription_before_reuse(pool: &FramePool) {
    reset_wait_token_state();

    let kernel = Kernel::new(pool.clone());
    let task = kernel.tasks.spawn_root().expect("spawn test init task");
    let (old_read, old_write) = PipeNode::pair();
    let (read_fd, write_fd) = task
        .add_file_pair_with_cloexec(FLike::Pipe(old_read), FLike::Pipe(old_write), false)
        .expect("pipe fd allocation should succeed");
    let epoll = EpInst::new();
    let epfd = task
        .add_file(FLike::Ep(epoll.clone()))
        .expect("epoll fd allocation should succeed");

    let event = EpEvent {
        events: EpEvent::IN,
        data: EpData { ptr: 1 },
    };
    let source = task.get_fd_entry(read_fd).expect("watched fd should exist");
    let key = EpKey::from_entry(read_fd, &source);
    epoll
        .control(EpCtlOp::ADD, key.clone(), &event)
        .expect("initial epoll add should succeed");
    key.source().add_epoll_watcher(&epoll);
    let sub_id = source
        .register_epoll_source(&key, &epoll, &event)
        .expect("pipe registration should install a source subscription");
    epoll.set_source_sub(&key, sub_id);

    task.close_fd(read_fd)
        .expect("closing watched fd should succeed");
    assert!(!epoll.has_interest(&key));
    assert_eq!(epoll.ready_len(), 0);
    // AGENT: release test-only kernel handles so pipe endpoint lifetime reflects
    // the now-empty fd table and epoll registration set.
    drop(key);
    drop(source);

    let (new_read, new_write) = PipeNode::pair();
    let (new_read_fd, new_write_fd) = task
        .add_file_pair_with_cloexec(FLike::Pipe(new_read), FLike::Pipe(new_write), false)
        .expect("fd reuse allocation should succeed");
    assert_eq!(new_read_fd, read_fd);

    let new_event = EpEvent {
        events: EpEvent::IN,
        data: EpData { ptr: 2 },
    };
    let new_source = task
        .get_fd_entry(new_read_fd)
        .expect("reused watched fd should exist");
    let new_key = EpKey::from_entry(new_read_fd, &new_source);
    epoll
        .control(EpCtlOp::ADD, new_key.clone(), &new_event)
        .expect("reused fd should not collide with a stale epoll interest");
    new_key.source().add_epoll_watcher(&epoll);
    let new_sub_id = new_source
        .register_epoll_source(&new_key, &epoll, &new_event)
        .expect("reused pipe registration should install a source subscription");
    epoll.set_source_sub(&new_key, new_sub_id);

    let old_writer = task
        .get_fd_entry(write_fd)
        .expect("old writer fd should still exist");
    assert_eq!(
        old_writer.write(task.id(), b"x"),
        Ok(FdWriteOutcome::BrokenPipe { written: 0 })
    );
    assert_eq!(
        epoll.ready_len(),
        0,
        "stale source callback marked the reused fd ready"
    );

    let _ = task.close_fd(new_read_fd);
    let _ = task.close_fd(new_write_fd);
    let _ = task.close_fd(write_fd);
    let _ = task.close_fd(epfd);
    clear_wait_token_state();
}

// AGENT: preserve a watched OFD while a dup alias exists, allow the same fd
// number to register a replacement OFD, and retire only the old key on last close.
fn fd_alias_keeps_epoll_source_across_number_reuse(pool: &FramePool) {
    reset_wait_token_state();

    let kernel = Kernel::new(pool.clone());
    let task = kernel.tasks.spawn_root().expect("spawn epoll alias task");
    let (old_read, old_write) = PipeNode::pair();
    let (old_read_fd, old_write_fd) = task
        .add_file_pair_with_cloexec(FLike::Pipe(old_read), FLike::Pipe(old_write), false)
        .expect("old pipe allocation should succeed");
    let alias_fd = task
        .dup_fd(old_read_fd, false)
        .expect("watched OFD alias should succeed");
    let epoll = EpInst::new();
    let epfd = task
        .add_file(FLike::Ep(epoll.clone()))
        .expect("epoll allocation should succeed");

    let old_event = EpEvent {
        events: EpEvent::IN,
        data: EpData { ptr: 11 },
    };
    let old_source = task
        .get_fd_entry(old_read_fd)
        .expect("old watched fd should exist");
    let old_key = EpKey::from_entry(old_read_fd, &old_source);
    epoll
        .control(EpCtlOp::ADD, old_key.clone(), &old_event)
        .expect("old OFD registration should succeed");
    old_key.source().add_epoll_watcher(&epoll);
    let old_sub = old_source
        .register_epoll_source(&old_key, &epoll, &old_event)
        .expect("old pipe should install a source subscription");
    epoll.set_source_sub(&old_key, old_sub);

    task.close_fd(old_read_fd)
        .expect("closing one OFD alias should succeed");
    assert!(epoll.has_interest(&old_key));

    let (new_read, new_write) = PipeNode::pair();
    let (new_read_fd, new_write_fd) = task
        .add_file_pair_with_cloexec(FLike::Pipe(new_read), FLike::Pipe(new_write), false)
        .expect("replacement pipe allocation should succeed");
    assert_eq!(new_read_fd, old_read_fd);

    let new_event = EpEvent {
        events: EpEvent::IN,
        data: EpData { ptr: 22 },
    };
    let new_source = task
        .get_fd_entry(new_read_fd)
        .expect("replacement watched fd should exist");
    let new_key = EpKey::from_entry(new_read_fd, &new_source);
    assert!(new_key != old_key);
    epoll
        .control(EpCtlOp::ADD, new_key.clone(), &new_event)
        .expect("same fd with a new OFD should be a distinct registration");
    new_key.source().add_epoll_watcher(&epoll);
    let new_sub = new_source
        .register_epoll_source(&new_key, &epoll, &new_event)
        .expect("replacement pipe should install a source subscription");
    epoll.set_source_sub(&new_key, new_sub);

    task.get_fd_entry(old_write_fd)
        .expect("old writer should remain open")
        .write(task.id(), b"x")
        .expect("old writer should wake the old OFD registration");
    let (ready_key, ready_event) = epoll
        .pop_ready()
        .expect("old OFD readiness should reach epoll");
    assert!(ready_key == old_key);
    assert_eq!(ready_event.data.ptr, 11);
    assert!(epoll_ready_events(ready_key.source().poll(), ready_event.events) != 0);

    task.close_fd(alias_fd)
        .expect("closing the last old OFD slot should succeed");
    assert!(!epoll.has_interest(&old_key));
    assert!(epoll.has_interest(&new_key));

    let _ = task.close_fd(new_read_fd);
    let _ = task.close_fd(new_write_fd);
    let _ = task.close_fd(old_write_fd);
    let _ = task.close_fd(epfd);
    clear_wait_token_state();
}

// AGENT: count fd-table slots across forked Process tables so closing the
// parent's watched descriptor cannot retire an OFD still installed in the child.
fn forked_fd_slot_keeps_epoll_source_until_child_close(pool: &FramePool) {
    reset_wait_token_state();

    let kernel = Kernel::new(pool.clone());
    let parent = kernel.tasks.spawn_root().expect("spawn epoll fork parent");
    let (read_end, write_end) = PipeNode::pair();
    let (read_fd, write_fd) = parent
        .add_file_pair_with_cloexec(FLike::Pipe(read_end), FLike::Pipe(write_end), false)
        .expect("fork source pipe allocation should succeed");
    let epoll = EpInst::new();
    let epfd = parent
        .add_file(FLike::Ep(epoll.clone()))
        .expect("fork epoll allocation should succeed");
    let event = EpEvent {
        events: EpEvent::IN,
        data: EpData { ptr: 33 },
    };
    let source = parent
        .get_fd_entry(read_fd)
        .expect("fork watched source should exist");
    let key = EpKey::from_entry(read_fd, &source);
    epoll
        .control(EpCtlOp::ADD, key.clone(), &event)
        .expect("fork watched source registration should succeed");
    key.source().add_epoll_watcher(&epoll);
    let sub_id = source
        .register_epoll_source(&key, &epoll, &event)
        .expect("fork watched pipe should install a source subscription");
    epoll.set_source_sub(&key, sub_id);

    let child = kernel
        .tasks
        .fork_process(&parent)
        .expect("fork should copy fd-table slots");
    parent
        .close_fd(read_fd)
        .expect("parent watched fd close should succeed");
    assert!(epoll.has_interest(&key));

    child
        .close_fd(read_fd)
        .expect("child last watched fd close should succeed");
    assert!(!epoll.has_interest(&key));

    let _ = parent.close_fd(write_fd);
    let _ = parent.close_fd(epfd);
    let _ = child.close_fd(write_fd);
    let _ = child.close_fd(epfd);
    clear_wait_token_state();
}

// AGENT: EpInst's ready list models Linux epitem queueing: source callbacks
// deduplicate a watched fd until epoll_wait consumes it, and LT delivery can
// requeue the same still-ready fd.
#[cfg_attr(test, test)]
fn epoll_ready_list_deduplicates_and_requeues() {
    reset_wait_token_state();

    let epoll = EpInst::new();
    let (read_end, _write_end) = PipeNode::pair();
    let source = FdEntry::new(FLike::Pipe(read_end));
    let key = EpKey::from_entry(3, &source);
    let event = EpEvent {
        events: EpEvent::IN,
        data: EpData { ptr: 7 },
    };
    epoll
        .control(EpCtlOp::ADD, key.clone(), &event)
        .expect("epoll add should succeed");

    epoll.mark_ready(&key);
    epoll.mark_ready(&key);
    assert_eq!(epoll.ready_len(), 1);

    let (queued_key, queued) = epoll.pop_ready().expect("ready fd should be queued");
    assert_eq!(queued_key.fd(), 3);
    assert_eq!(queued.data.ptr, 7);
    assert_eq!(epoll.ready_len(), 0);

    epoll.requeue_ready(&key);
    assert_eq!(epoll.ready_len(), 1);

    epoll
        .control(EpCtlOp::DEL, key, &event)
        .expect("epoll delete should succeed");
    assert_eq!(epoll.ready_len(), 0);
    assert!(epoll.pop_ready().is_none());
    clear_wait_token_state();
}

// AGENT: pipe buffers use CircBuf capacity, report full pipes as not writable,
// and become writable again when reads free ring slots.
#[cfg_attr(test, test)]
fn pipe_uses_bounded_ring_buffer_and_reports_writable() {
    let (read_end, write_end) = PipeNode::pair();
    let payload = vec![0xA5; 4 * 1024];

    assert!(write_end.poll().writable);
    assert_eq!(
        write_end.write_at(0, true, &payload),
        Ok(PipeWriteOutcome::Written(payload.len()))
    );
    assert!(!write_end.poll().writable);
    assert_eq!(write_end.write_at(0, true, b"x"), Err("eagain"));
    assert_eq!(read_end.readable_len(), payload.len());

    let mut out = [0u8; 4];
    assert_eq!(read_end.read_at(0, true, &mut out), Ok(out.len()));
    assert_eq!(out, [0xA5; 4]);
    assert!(write_end.poll().writable);
}

// AGENT: a nonblocking write no larger than PIPE_BUF must publish either the
// whole record or no bytes, even when the ring has some but insufficient room.
#[cfg_attr(test, test)]
fn pipe_nonblocking_small_write_is_atomic() {
    let (read_end, write_end) = PipeNode::pair();
    let fill = vec![0x31; PIPE_BUF - 1];

    assert_eq!(
        write_end.write_at(0, true, &fill),
        Ok(PipeWriteOutcome::Written(fill.len()))
    );
    assert_eq!(write_end.write_at(0, true, b"xy"), Err("eagain"));
    assert_eq!(read_end.readable_len(), fill.len());

    let mut actual = vec![0; fill.len()];
    assert_eq!(read_end.read_at(0, true, &mut actual), Ok(fill.len()));
    assert_eq!(actual, fill);
}

// AGENT: nonblocking writes larger than PIPE_BUF may consume available room
// and report partial progress instead of rolling the complete request back.
#[cfg_attr(test, test)]
fn pipe_nonblocking_large_write_can_be_partial() {
    let (read_end, write_end) = PipeNode::pair();
    let payload = vec![0x62; PIPE_BUF + 7];

    assert_eq!(
        write_end.write_at(0, true, &payload),
        Ok(PipeWriteOutcome::Written(PIPE_BUF))
    );
    assert_eq!(read_end.readable_len(), PIPE_BUF);
}

// AGENT: closing the final writer preserves already-buffered data before EOF;
// closing the final reader turns the next write into a broken-pipe outcome.
#[cfg_attr(test, test)]
fn pipe_buffered_bytes_precede_eof_and_missing_reader_breaks_write() {
    let (read_end, write_end) = PipeNode::pair();
    assert_eq!(
        write_end.write_at(0, true, b"end"),
        Ok(PipeWriteOutcome::Written(3))
    );
    drop(write_end);

    let mut actual = [0; 3];
    assert_eq!(read_end.read_at(0, true, &mut actual), Ok(3));
    assert_eq!(&actual, b"end");
    assert_eq!(read_end.read_at(0, true, &mut actual), Ok(0));

    let (last_reader, lone_writer) = PipeNode::pair();
    drop(last_reader);
    assert_eq!(
        lone_writer.write_at(0, true, b"x"),
        Ok(PipeWriteOutcome::Broken { written: 0 })
    );
}

// AGENT: exercise an empty blocking read on a real task stack; a producer wake
// must move the task Sleeping -> Runnable and make it recheck the pipe state.
fn pipe_blocking_read_sleeps_until_data_arrives(pool: &FramePool) {
    reset_wait_token_state();
    *PIPE_ROUND_TRIP_PAIR.lock().unwrap() = Some(PipeNode::pair());
    *PIPE_READ_ROUND_TRIP_RESULT.lock().unwrap() = None;

    let kernel = Box::leak(Box::new(Kernel::new(pool.clone())));
    kernel.proc_init();
    let task = kernel.cur_task(0).expect("init task should be current");
    task.install_test_kernel_entry(pipe_read_round_trip_test_task)
        .expect("pipe read test task should receive kernel entry");
    install_kernel(kernel);

    assert!(kernel.run_one_cpu0_task_for_test());
    assert_eq!(task.sched_state(), TaskRunState::Sleeping);
    assert_eq!(*PIPE_READ_ROUND_TRIP_RESULT.lock().unwrap(), None);
    assert_eq!(
        PIPE_ROUND_TRIP_PAIR
            .lock()
            .unwrap()
            .as_ref()
            .expect("pipe pair should remain installed")
            .1
            .write_at(0, true, b"r"),
        Ok(PipeWriteOutcome::Written(1))
    );
    assert_eq!(task.sched_state(), TaskRunState::Runnable);

    assert!(kernel.run_one_cpu0_task_for_test());
    assert_eq!(*PIPE_READ_ROUND_TRIP_RESULT.lock().unwrap(), Some(Ok(1)));
    assert!(task.done());

    *PIPE_ROUND_TRIP_PAIR.lock().unwrap() = None;
    *PIPE_READ_ROUND_TRIP_RESULT.lock().unwrap() = None;
    clear_wait_token_state();
}

// AGENT: dropping the final writer is itself a read condition transition; an
// empty blocking reader must wake and finish with EOF instead of sleeping forever.
fn pipe_blocking_read_wakes_for_eof(pool: &FramePool) {
    reset_wait_token_state();
    *PIPE_ROUND_TRIP_PAIR.lock().unwrap() = Some(PipeNode::pair());
    *PIPE_READ_ROUND_TRIP_RESULT.lock().unwrap() = None;

    let kernel = Box::leak(Box::new(Kernel::new(pool.clone())));
    kernel.proc_init();
    let task = kernel.cur_task(0).expect("init task should be current");
    task.install_test_kernel_entry(pipe_read_round_trip_test_task)
        .expect("pipe EOF test task should receive kernel entry");
    install_kernel(kernel);

    assert!(kernel.run_one_cpu0_task_for_test());
    assert_eq!(task.sched_state(), TaskRunState::Sleeping);
    let sleeping_pair = PIPE_ROUND_TRIP_PAIR
        .lock()
        .unwrap()
        .replace(PipeNode::pair())
        .expect("sleeping pipe pair should remain installed");
    drop(sleeping_pair);
    assert_eq!(task.sched_state(), TaskRunState::Runnable);

    assert!(kernel.run_one_cpu0_task_for_test());
    assert_eq!(*PIPE_READ_ROUND_TRIP_RESULT.lock().unwrap(), Some(Ok(0)));
    assert!(task.done());

    *PIPE_ROUND_TRIP_PAIR.lock().unwrap() = None;
    *PIPE_READ_ROUND_TRIP_RESULT.lock().unwrap() = None;
    clear_wait_token_state();
}

// AGENT: a blocking write larger than PIPE_BUF may first fill the ring, but it
// must sleep and resume until the complete request is written after space frees.
fn pipe_blocking_large_write_resumes_until_complete(pool: &FramePool) {
    reset_wait_token_state();
    *PIPE_ROUND_TRIP_PAIR.lock().unwrap() = Some(PipeNode::pair());
    *PIPE_WRITE_ROUND_TRIP_RESULT.lock().unwrap() = None;

    let kernel = Box::leak(Box::new(Kernel::new(pool.clone())));
    kernel.proc_init();
    let task = kernel.cur_task(0).expect("init task should be current");
    task.install_test_kernel_entry(pipe_write_round_trip_test_task)
        .expect("pipe write test task should receive kernel entry");
    install_kernel(kernel);

    assert!(kernel.run_one_cpu0_task_for_test());
    assert_eq!(task.sched_state(), TaskRunState::Sleeping);
    assert_eq!(
        PIPE_ROUND_TRIP_PAIR
            .lock()
            .unwrap()
            .as_ref()
            .expect("pipe pair should remain installed")
            .0
            .readable_len(),
        PIPE_BUF
    );

    let mut first = [0; 1];
    assert_eq!(
        PIPE_ROUND_TRIP_PAIR
            .lock()
            .unwrap()
            .as_ref()
            .expect("pipe pair should remain installed")
            .0
            .read_at(0, true, &mut first),
        Ok(1)
    );
    assert_eq!(task.sched_state(), TaskRunState::Runnable);
    assert!(kernel.run_one_cpu0_task_for_test());
    assert_eq!(
        *PIPE_WRITE_ROUND_TRIP_RESULT.lock().unwrap(),
        Some(Ok(PipeWriteOutcome::Written(PIPE_BUF + 1)))
    );
    assert!(task.done());

    let mut remaining = vec![0; PIPE_BUF];
    assert_eq!(
        PIPE_ROUND_TRIP_PAIR
            .lock()
            .unwrap()
            .as_ref()
            .expect("pipe pair should remain installed")
            .0
            .read_at(0, true, &mut remaining),
        Ok(PIPE_BUF)
    );
    assert!(remaining.iter().all(|byte| *byte == 0x5a));

    *PIPE_ROUND_TRIP_PAIR.lock().unwrap() = None;
    *PIPE_WRITE_ROUND_TRIP_RESULT.lock().unwrap() = None;
    clear_wait_token_state();
}

// AGENT: dropping the final reader must wake a writer that already made large-
// write progress, preserving that progress while reporting the broken peer.
fn pipe_blocking_write_wakes_for_broken_peer(pool: &FramePool) {
    reset_wait_token_state();
    *PIPE_ROUND_TRIP_PAIR.lock().unwrap() = Some(PipeNode::pair());
    *PIPE_WRITE_ROUND_TRIP_RESULT.lock().unwrap() = None;

    let kernel = Box::leak(Box::new(Kernel::new(pool.clone())));
    kernel.proc_init();
    let task = kernel.cur_task(0).expect("init task should be current");
    task.install_test_kernel_entry(pipe_write_round_trip_test_task)
        .expect("broken pipe test task should receive kernel entry");
    install_kernel(kernel);

    assert!(kernel.run_one_cpu0_task_for_test());
    assert_eq!(task.sched_state(), TaskRunState::Sleeping);
    let sleeping_pair = PIPE_ROUND_TRIP_PAIR
        .lock()
        .unwrap()
        .replace(PipeNode::pair())
        .expect("sleeping pipe pair should remain installed");
    drop(sleeping_pair);
    assert_eq!(task.sched_state(), TaskRunState::Runnable);

    assert!(kernel.run_one_cpu0_task_for_test());
    assert_eq!(
        *PIPE_WRITE_ROUND_TRIP_RESULT.lock().unwrap(),
        Some(Ok(PipeWriteOutcome::Broken { written: PIPE_BUF }))
    );
    assert!(task.done());

    *PIPE_ROUND_TRIP_PAIR.lock().unwrap() = None;
    *PIPE_WRITE_ROUND_TRIP_RESULT.lock().unwrap() = None;
    clear_wait_token_state();
}

// AGENT: task-stack entry used by pipe_blocking_read_sleeps_until_data_arrives.
extern "C" fn pipe_read_round_trip_test_task() -> ! {
    let read_end = PIPE_ROUND_TRIP_PAIR
        .lock()
        .unwrap()
        .as_ref()
        .expect("pipe pair should be installed")
        .0
        .clone();
    let kernel = crate::kernel::global_kernel().expect("pipe test kernel should be installed");
    let task = kernel
        .cur_task(0)
        .expect("pipe read task should be current");
    let mut byte = [0; 1];
    *PIPE_READ_ROUND_TRIP_RESULT.lock().unwrap() =
        Some(read_end.read_at(task.id(), false, &mut byte));

    task.mark_thread_exited();
    drop(task);
    kernel.switch_current_to_idle(0);
    loop {
        core::hint::spin_loop();
    }
}

// AGENT: task-stack entry used by pipe_blocking_large_write_resumes_until_complete.
extern "C" fn pipe_write_round_trip_test_task() -> ! {
    let write_end = PIPE_ROUND_TRIP_PAIR
        .lock()
        .unwrap()
        .as_ref()
        .expect("pipe pair should be installed")
        .1
        .clone();
    let kernel = crate::kernel::global_kernel().expect("pipe test kernel should be installed");
    let task = kernel
        .cur_task(0)
        .expect("pipe write task should be current");
    let payload = [0x5a; PIPE_BUF + 1];
    *PIPE_WRITE_ROUND_TRIP_RESULT.lock().unwrap() =
        Some(write_end.write_at(task.id(), false, &payload));

    task.mark_thread_exited();
    drop(task);
    kernel.switch_current_to_idle(0);
    loop {
        core::hint::spin_loop();
    }
}

// AGENT: pipe endpoints reject wrong-direction direct I/O even when a caller
// bypasses the OpenFileDesc permission check.
#[cfg_attr(test, test)]
fn pipe_rejects_wrong_direction_direct_io() {
    let (read_end, write_end) = PipeNode::pair();
    let mut out = [0u8; 1];

    assert_eq!(write_end.read_at(0, true, &mut out), Err("ebadf"));
    assert_eq!(read_end.write_at(0, true, b"x"), Err("ebadf"));
}

// AGENT: pipe peer-close state must wake epoll and also survive the level scan
// as public HUP/ERR events.
#[cfg_attr(test, test)]
fn pipe_epoll_closed_status_reports_hup_and_err() {
    let (read_end, write_end) = PipeNode::pair();
    drop(write_end);

    let read_status = read_end.poll();
    assert!(read_status.readable);
    assert!(read_status.closed);
    let read_ready = epoll_ready_events(read_status, EpEvent::IN);
    assert_eq!(read_ready & EpEvent::IN, EpEvent::IN);
    assert_eq!(read_ready & EpEvent::HUP, EpEvent::HUP);
    assert_eq!(
        epoll_ready_events(read_status, EpEvent::HUP) & EpEvent::HUP,
        EpEvent::HUP
    );

    let (read_end, write_end) = PipeNode::pair();
    drop(read_end);

    let write_status = write_end.poll();
    assert!(write_status.error);
    assert!(write_status.closed);
    let write_ready = epoll_ready_events(write_status, 0);
    assert_eq!(write_ready & EpEvent::ERR, EpEvent::ERR);
    assert_eq!(write_ready & EpEvent::HUP, EpEvent::HUP);
}
