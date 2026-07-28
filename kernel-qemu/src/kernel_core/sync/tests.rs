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
    EpEvent, EpInst, EpKey, FLike, FdEntry, FramePool, Kernel, PipeNode, TaskRunState, VmRegion,
    CLK, SIGUSR1,
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
    pipe_rejects_wrong_direction_direct_io();
    pipe_epoll_closed_status_reports_hup_and_err();
    fd_allocator_supports_lower_bounds_fixed_targets_and_reuse(pool);
    fd_close_detaches_epoll_subscription_before_reuse(pool);
    fd_alias_keeps_epoll_source_across_number_reuse(pool);
    forked_fd_slot_keeps_epoll_source_until_child_close(pool);
    epoll_ready_list_deduplicates_and_requeues();
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
    task.set_sched_state(TaskRunState::Sleeping);
    install_kernel(kernel);
    let token = WaitToken::for_task(task.id());

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
// dup2 exact placement/replacement, and close-driven reuse under one FdTable.
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

    let exact_fd = task
        .dup2_fd(source_fd, 2)
        .expect("dup2 exact fd allocation should succeed");
    assert_eq!(exact_fd, 2);
    task.set_cloexec(source_fd, true)
        .expect("source cloexec update should succeed");
    assert_eq!(task.dup2_fd(source_fd, source_fd), Ok(source_fd));
    assert!(task
        .get_fd_entry(source_fd)
        .expect("same-fd dup2 source should remain open")
        .is_cloexec());
    let next_exact_fd = task
        .dup_fd_from(source_fd, 2, false)
        .expect("next exact-range fd allocation should succeed");
    assert_eq!(next_exact_fd, 3);

    task.set_cloexec(high_fd, true)
        .expect("target cloexec update should succeed");
    let previous_target = task
        .get_fd_entry(high_fd)
        .expect("dup2 target should be open");
    assert_eq!(task.dup2_fd(MAX_FD - 1, high_fd), Err("ebadf"));
    assert!(task
        .get_fd_entry(high_fd)
        .expect("invalid source must not close dup2 target")
        .same_open_description(&previous_target));

    assert_eq!(task.dup2_fd(source_fd, high_fd), Ok(high_fd));
    let replaced = task
        .get_fd_entry(high_fd)
        .expect("dup2 target should hold the source OFD");
    assert!(replaced.same_open_description(
        &task
            .get_fd_entry(source_fd)
            .expect("dup2 source should remain open")
    ));
    assert!(!replaced.is_cloexec());
    task.close_fd(high_fd)
        .expect("closing the replaced fd should succeed");
    assert_eq!(task.dup_fd_from(source_fd, high_fd, false), Ok(high_fd));

    let _ = task.close_fd(source_fd);
    let _ = task.close_fd(low_fd);
    let _ = task.close_fd(exact_fd);
    let _ = task.close_fd(next_exact_fd);
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
        old_writer
            .write(b"x")
            .expect_err("old pipe should be broken"),
        "epipe"
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
        .write(b"x")
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
    assert_eq!(write_end.write_at(&payload), Ok(payload.len()));
    assert!(!write_end.poll().writable);
    assert_eq!(write_end.write_at(b"x"), Err("eagain"));
    assert_eq!(read_end.readable_len(), payload.len());

    let mut out = [0u8; 4];
    assert_eq!(read_end.read_at(&mut out), Ok(out.len()));
    assert_eq!(out, [0xA5; 4]);
    assert!(write_end.poll().writable);
}

// AGENT: pipe endpoints reject wrong-direction direct I/O even when a caller
// bypasses the OpenFileDesc permission check.
#[cfg_attr(test, test)]
fn pipe_rejects_wrong_direction_direct_io() {
    let (read_end, write_end) = PipeNode::pair();
    let mut out = [0u8; 1];

    assert_eq!(write_end.read_at(&mut out), Err("ebadf"));
    assert_eq!(read_end.write_at(b"x"), Err("ebadf"));
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
