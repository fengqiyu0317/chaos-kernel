// AGENT: keep WaitToken regressions next to the QEMU sync primitives and expose
// them through the same run_all + cfg_attr(test, test) pattern as mm/tests.rs.
use super::*;
use crate::kernel::kernel_core::{
    global_timer_wheel, init_timer_wheel, set_current_task_id, TimerTarget, TimerWheel, TIMER_WHEEL,
};
use crate::kernel::{Kernel, TaskRunState};

pub fn run_all() {
    wait_token_captures_current_task();
    wait_token_event_wake_wins_once();
    wait_token_timeout_wake_wins_once();
    wait_token_zero_duration_times_out_without_timer_wheel();
    wait_token_expired_deadline_times_out_immediately();
    wait_token_timer_target_times_out_on_schedule_tick();
    wait_token_event_wake_uses_installed_scheduler_backend();
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

    let kernel = Kernel::new(8);
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

    let kernel = Box::leak(Box::new(Kernel::new(8)));
    let task = kernel.tasks.spawn_root();
    task.set_sched_state(TaskRunState::Sleeping);
    set_current_task_id(Some(task.id()));
    install_qemu_wait_kernel(kernel);
    let token = WaitToken::current();

    assert!(token.wake_event());
    assert_eq!(task.sched_state(), TaskRunState::Runnable);
    assert_eq!(kernel.run_queue.pick_next(), Some(task.id()));

    clear_wait_token_state();
}
