// AGENT: keep WaitToken regressions next to the QEMU sync primitives and expose
// them through the same run_all + cfg_attr(test, test) pattern as mm/tests.rs.
use super::*;
use crate::kernel::kernel_core::{
    global_timer_wheel, init_timer_wheel, set_current_task_id, TimerTarget, TimerWheel, TIMER_WHEEL,
};
use crate::kernel::{EpCtlOp, EpData, EpEvent, EpInst, FLike, Kernel, PipeNode, TaskRunState};

pub fn run_all() {
    wait_token_captures_current_task();
    wait_token_event_wake_wins_once();
    wait_token_timeout_wake_wins_once();
    wait_token_zero_duration_times_out_without_timer_wheel();
    wait_token_expired_deadline_times_out_immediately();
    wait_token_timer_target_times_out_on_schedule_tick();
    wait_token_event_wake_uses_installed_scheduler_backend();
    futex_wait_returns_changed_without_queueing();
    futex_wait_propagates_word_read_fault();
    futex_wait_timeout_removes_published_waiter();
    futex_cmp_requeue_propagates_word_read_fault();
    futex_requeue_skips_completed_waiters_when_moving();
    fd_close_detaches_epoll_subscription_before_reuse();
}

// AGENT: reset simulator-global wait state so QEMU boot selftests are
// deterministic when run after heap and timer-wheel initialization.
fn reset_wait_token_state(task_id: usize) {
    WAIT_KERNEL.store(0, Ordering::Release);
    set_current_task_id(Some(task_id));
    CLK.store(0, Ordering::Relaxed);
    CLK_ALL.store(0, Ordering::Relaxed);
    ensure_timer_wheel();
    *global_timer_wheel().lock() = TimerWheel::new();
}

// AGENT: leave no current task or scheduler backend behind for later selftests.
fn clear_wait_token_state() {
    WAIT_KERNEL.store(0, Ordering::Release);
    set_current_task_id(None);
}

// AGENT: ordinary Rust tests may enter without rust_main(), while QEMU boot
// selftests enter after rust_main() has already initialized the once cell.
fn ensure_timer_wheel() {
    if TIMER_WHEEL.get().is_none() {
        init_timer_wheel();
    }
}

// AGENT: build a tiny fully-free frame pool for scheduler-only selftests that
// do not exercise QEMU physical-memory discovery.
fn test_frame_pool(pages: usize) -> FramePool {
    let pool = FramePool::new(pages, MEM_OFF);
    let end = MEM_OFF + pages * PAGE_SZ;
    pool.mark_free_range(MEM_OFF, end);
    pool
}

// AGENT: WaitToken::current must bind to the current simulator task id and give
// each token a distinct identity.
#[cfg_attr(test, test)]
fn wait_token_captures_current_task() {
    reset_wait_token_state(11);

    let first = WaitToken::current();
    let second = WaitToken::current();

    assert_eq!(first.task_id(), 11);
    assert_eq!(second.task_id(), 11);
    assert_ne!(first.id(), second.id());
    assert!(!first.same(&second));
    assert!(!first.is_woken());

    clear_wait_token_state();
}

// AGENT: event wakeups complete a pending token exactly once and must beat later
// timeout attempts.
#[cfg_attr(test, test)]
fn wait_token_event_wake_wins_once() {
    reset_wait_token_state(12);

    let token = WaitToken::current();

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
    reset_wait_token_state(13);

    let token = WaitToken::current();

    assert!(token.wake_timeout());
    assert!(!token.wake_event());
    assert!(!token.wake());
    assert!(token.is_woken());
    assert!(token.is_timeout());
    assert_eq!(token.outcome(), WaitOutcome::Timeout);
    assert_eq!(token.wait(None), WaitOutcome::Timeout);

    clear_wait_token_state();
}

// AGENT: zero-length waits must finish as timeouts without touching the timer
// wheel or entering the spin wait loop.
#[cfg_attr(test, test)]
fn wait_token_zero_duration_times_out_without_timer_wheel() {
    reset_wait_token_state(14);

    let token = WaitToken::current();

    assert_eq!(
        token.wait(Some(Duration::from_nanos(0))),
        WaitOutcome::Timeout
    );
    assert!(token.is_timeout());
    assert_eq!(global_timer_wheel().lock().active_count(), 0);

    clear_wait_token_state();
}

// AGENT: already-expired absolute deadlines must timeout immediately instead of
// registering a timer and spinning.
#[cfg_attr(test, test)]
fn wait_token_expired_deadline_times_out_immediately() {
    reset_wait_token_state(15);

    CLK.store(7, Ordering::Relaxed);
    let token = WaitToken::current();

    assert_eq!(token.wait_until_tick(7), WaitOutcome::Timeout);
    assert!(token.is_timeout());
    assert_eq!(global_timer_wheel().lock().active_count(), 0);

    clear_wait_token_state();
}

// AGENT: the QEMU timer wheel dispatches TimerTarget::WakeToken through the same
// timeout marker used by WaitToken::wait_with_timer().
#[cfg_attr(test, test)]
fn wait_token_timer_target_times_out_on_schedule_tick() {
    reset_wait_token_state(16);

    let kernel = Kernel::new(test_frame_pool(8));
    let token = WaitToken::current();
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
#[cfg_attr(test, test)]
fn wait_token_event_wake_uses_installed_scheduler_backend() {
    reset_wait_token_state(17);

    let kernel = Box::leak(Box::new(Kernel::new(test_frame_pool(8))));
    let task = kernel.tasks.spawn_root().expect("spawn test init task");
    task.set_sched_state(TaskRunState::Sleeping);
    set_current_task_id(Some(task.id()));
    install_qemu_wait_kernel(kernel);
    let token = WaitToken::current();

    assert!(token.wake_event());
    assert_eq!(task.sched_state(), TaskRunState::Runnable);
    assert_eq!(kernel.run_queue.pick_next(), Some(task.id()));

    clear_wait_token_state();
}

// AGENT: FutexBucket::wait must compare the current futex word before enqueueing
// and return the syscall-layer "changed" marker when it differs.
#[cfg_attr(test, test)]
fn futex_wait_returns_changed_without_queueing() {
    reset_wait_token_state(18);

    let futex = FutexBucket::new();
    let addr = 0x4000;
    let calls = AtomicUsize::new(0);

    let err = futex
        .wait(addr, 1, None, || {
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
    reset_wait_token_state(19);

    let futex = FutexBucket::new();
    let addr = 0x5000;

    let err = futex
        .wait(addr, 1, None, || Err("efault"))
        .expect_err("read fault should abort wait setup");

    assert_eq!(err, "efault");
    assert_eq!(futex.pending_at(addr), 0);

    clear_wait_token_state();
}

// AGENT: a matching word with an immediate timeout proves the waiter is
// published first, then removed by finish_wait() when the token times out.
#[cfg_attr(test, test)]
fn futex_wait_timeout_removes_published_waiter() {
    reset_wait_token_state(20);

    let futex = FutexBucket::new();
    let addr = 0x6000;
    let calls = AtomicUsize::new(0);

    let err = futex
        .wait(addr, 1, Some(Duration::from_nanos(0)), || {
            calls.fetch_add(1, Ordering::Relaxed);
            Ok(1)
        })
        .expect_err("zero timeout should finish as timeout");

    assert_eq!(err, "timeout");
    assert_eq!(calls.load(Ordering::Relaxed), 1);
    assert_eq!(futex.pending_at(addr), 0);

    clear_wait_token_state();
}

// AGENT: cmp_requeue now reads the source futex word through a caller-supplied
// copy-in closure; read errors should be returned instead of panicking.
#[cfg_attr(test, test)]
fn futex_cmp_requeue_propagates_word_read_fault() {
    reset_wait_token_state(21);

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

// AGENT: completed timeout entries must be discarded before requeue counts move
// slots, otherwise a stale waiter can consume move_n and leave a live waiter on src.
#[cfg_attr(test, test)]
fn futex_requeue_skips_completed_waiters_when_moving() {
    reset_wait_token_state(22);

    let src = 0x9000;
    let dst = 0xA000;
    let stale = WaitToken::current();
    let live = WaitToken::current();
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

// AGENT: closing a watched fd must remove the old epoll interest and cancel its
// pipe source callback before the same fd number can be reused for another file.
#[cfg_attr(test, test)]
fn fd_close_detaches_epoll_subscription_before_reuse() {
    reset_wait_token_state(23);

    let kernel = Kernel::new(test_frame_pool(8));
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
    epoll
        .control(EpCtlOp::ADD, read_fd, &event)
        .expect("initial epoll add should succeed");
    let sub_id = {
        let source = task.get_file(read_fd).expect("watched fd should exist");
        source
            .register_epoll(read_fd, epoll.clone(), &event)
            .expect("pipe registration should install a source subscription")
    };
    epoll.set_source_sub(read_fd, sub_id);

    task.close_fd(read_fd)
        .expect("closing watched fd should succeed");
    assert!(!epoll.events.lock().unwrap().contains_key(&read_fd));
    assert!(epoll.ready.lock().unwrap().is_empty());

    let (new_read, new_write) = PipeNode::pair();
    let (new_read_fd, new_write_fd) = task
        .add_file_pair_with_cloexec(FLike::Pipe(new_read), FLike::Pipe(new_write), false)
        .expect("fd reuse allocation should succeed");
    assert_eq!(new_read_fd, read_fd);

    let new_event = EpEvent {
        events: EpEvent::IN,
        data: EpData { ptr: 2 },
    };
    epoll
        .control(EpCtlOp::ADD, new_read_fd, &new_event)
        .expect("reused fd should not collide with a stale epoll interest");
    let new_sub_id = {
        let source = task
            .get_file(new_read_fd)
            .expect("reused watched fd should exist");
        source
            .register_epoll(new_read_fd, epoll.clone(), &new_event)
            .expect("reused pipe registration should install a source subscription")
    };
    epoll.set_source_sub(new_read_fd, new_sub_id);

    let old_writer = task
        .get_fd_entry(write_fd)
        .expect("old writer fd should still exist");
    assert_eq!(
        old_writer
            .write(b"x")
            .expect_err("old pipe should be broken"),
        "broken"
    );
    assert!(
        epoll.ready.lock().unwrap().is_empty(),
        "stale source callback marked the reused fd ready"
    );

    let _ = task.close_fd(new_read_fd);
    let _ = task.close_fd(new_write_fd);
    let _ = task.close_fd(write_fd);
    let _ = task.close_fd(epfd);
    clear_wait_token_state();
}
