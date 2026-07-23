// AGENT
use crate::kernel::kernel_core::arch::CLK;
use crate::kernel::kernel_core::kernel_base::global_kernel;
use crate::kernel::kernel_core::prelude::*;
use crate::kernel::kernel_core::time::{global_timer_wheel, TimerTarget};

// AGENT: QEMU timer interrupts use this hook to drive the migrated logical
// kernel clock and timer wheel once a Kernel has been installed.
pub(crate) fn qemu_wait_timer_tick() {
    if let Some(kernel) = global_kernel() {
        kernel.schedule_tick(0);
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WaitOutcome {
    Event,
    Timeout,
    Signal,
}

const WAIT_PENDING: u8 = 0;
const WAIT_EVENT: u8 = 1;
const WAIT_TIMEOUT: u8 = 2;
const WAIT_SIGNAL: u8 = 3;

// AGENT: clones share one wait identity through the Arc-backed state; distinct
// waits are distinguished with Arc::ptr_eq rather than a redundant numeric id.
#[derive(Clone)]
pub struct WaitToken {
    state: Arc<WaitState>,
}

struct WaitState {
    outcome: AtomicU8,
    task_id: usize,
}

impl WaitToken {
    // AGENT: bind a wait to the task selected from Processor.current by the
    // caller, avoiding a second global current-task marker in the sync layer.
    pub fn for_task(task_id: usize) -> Self {
        assert_ne!(task_id, 0, "WaitToken needs a nonzero Task::id()");
        Self {
            state: Arc::new(WaitState {
                outcome: AtomicU8::new(WAIT_PENDING),
                task_id,
            }),
        }
    }

    // AGENT: expose the scheduler task carried by this QEMU wait token.
    pub fn task_id(&self) -> usize {
        self.state.task_id
    }

    pub fn wake(&self) -> bool {
        self.wake_event()
    }

    // AGENT: mark a normal event wake; returns false if timeout or another wake
    // already won the race.
    pub fn wake_event(&self) -> bool {
        if self
            .state
            .outcome
            .compare_exchange(
                WAIT_PENDING,
                WAIT_EVENT,
                Ordering::Release,
                Ordering::Relaxed,
            )
            .is_ok()
        {
            self.wake_waiter_task();
            true
        } else {
            false
        }
    }

    // AGENT: mark a timer expiry wake separately from a normal event wake.
    pub fn wake_timeout(&self) -> bool {
        if self
            .state
            .outcome
            .compare_exchange(
                WAIT_PENDING,
                WAIT_TIMEOUT,
                Ordering::Release,
                Ordering::Relaxed,
            )
            .is_ok()
        {
            self.wake_waiter_task();
            true
        } else {
            false
        }
    }

    // AGENT: mark a wait as interrupted by a pending signal without pretending
    // the watched futex/epoll/channel event became ready.
    fn wake_signal(&self) -> bool {
        if self
            .state
            .outcome
            .compare_exchange(
                WAIT_PENDING,
                WAIT_SIGNAL,
                Ordering::Release,
                Ordering::Relaxed,
            )
            .is_ok()
        {
            self.wake_waiter_task();
            true
        } else {
            false
        }
    }

    // AGENT: wake the task that owns this token through the installed QEMU
    // scheduler backend. In early carrier smoke paths without a backend, the
    // atomic outcome alone lets a spinning waiter observe completion.
    fn wake_waiter_task(&self) {
        if let Some(kernel) = global_kernel() {
            kernel.wake_task_for_wait(self.state.task_id);
        }
    }

    // AGENT: park the owning task in scheduler state; with CPU0 scheduling
    // active, this returns only after a wakeup requeues and resumes the task.
    fn block_waiter_task(&self) {
        if let Some(kernel) = global_kernel() {
            kernel.block_task_for_wait(self.state.task_id);
        }
    }

    // AGENT: interruptible waits observe pending task signals separately from
    // resource readiness so syscall code can return EINTR instead of success.
    fn has_interrupting_signal(&self) -> bool {
        global_kernel()
            .is_some_and(|kernel| kernel.task_has_interrupting_signal(self.state.task_id))
    }

    // AGENT: plain kernel waits use an optional absolute logical tick deadline;
    // None waits indefinitely and does not observe pending task signals.
    pub fn wait(&self, deadline: Option<usize>) -> WaitOutcome {
        self.wait_inner(deadline, false)
    }

    // AGENT: syscall-facing waits use the same absolute deadline representation
    // but additionally surface pending signals as WaitOutcome::Signal.
    pub fn wait_interruptible(&self, deadline: Option<usize>) -> WaitOutcome {
        self.wait_inner(deadline, true)
    }

    // AGENT: centralize optional timer registration, signal interruption,
    // scheduler state transitions, and early-wake timer cancellation.
    fn wait_inner(&self, deadline: Option<usize>, interruptible: bool) -> WaitOutcome {
        if self.is_woken() {
            return self.outcome();
        }
        let timer_id = match deadline {
            Some(deadline) => {
                if CLK.load(Ordering::Relaxed) >= deadline {
                    self.wake_timeout();
                    return self.outcome();
                }
                let mut wheel = global_timer_wheel().lock();
                Some(wheel.register_timer(
                    deadline,
                    0,
                    TimerTarget::WakeToken {
                        token: self.clone(),
                    },
                ))
            }
            None => None,
        };

        while !self.is_woken() {
            if interruptible && self.has_interrupting_signal() {
                self.wake_signal();
                break;
            }
            // AGENT: a scheduler wake is not necessarily this token completing:
            // masked signals and unrelated task wakes may resume the waiter
            // while its outcome is still pending. Re-enter Sleeping after every
            // such spurious wake instead of spinning forever after the first
            // block round trip.
            self.block_waiter_task();
            if self.is_woken() {
                break;
            }
        }

        let outcome = self.outcome();
        if outcome != WaitOutcome::Timeout {
            if let Some(timer_id) = timer_id {
                global_timer_wheel().lock().cancel(timer_id);
            }
        }
        outcome
    }

    pub fn is_woken(&self) -> bool {
        self.state.outcome.load(Ordering::Acquire) != WAIT_PENDING
    }

    pub fn is_timeout(&self) -> bool {
        self.state.outcome.load(Ordering::Acquire) == WAIT_TIMEOUT
    }

    pub fn outcome(&self) -> WaitOutcome {
        match self.state.outcome.load(Ordering::Acquire) {
            WAIT_TIMEOUT => WaitOutcome::Timeout,
            WAIT_SIGNAL => WaitOutcome::Signal,
            _ => WaitOutcome::Event,
        }
    }

    pub fn same(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.state, &other.state)
    }
}

// AGENT: local WaitToken queue only; condition checks, saved wake credits, and
// higher-level waiting policies live in ConditionWait / CountingEvent.
pub(crate) struct WaitQueue {
    q: Mutex<VecDeque<WaitToken>>,
}

impl WaitQueue {
    pub fn new() -> Self {
        Self {
            q: Mutex::new(VecDeque::new()),
        }
    }

    // AGENT: enqueue an explicitly selected task while the caller still holds
    // the condition state lock, then release that state lock before blocking.
    pub fn enqueue_task_locked(&self, task_id: usize) -> WaitToken {
        let token = WaitToken::for_task(task_id);
        let mut q = self.q.lock().unwrap();
        q.push_back(token.clone());
        token
    }
    // AGENT: remove a token that is no longer waiting.
    pub fn remove_waiter(&self, token: &WaitToken) {
        let mut q = self.q.lock().unwrap();
        q.retain(|queued| !queued.same(token));
    }

    fn signal_or_else(&self, mut on_empty: impl FnMut()) {
        loop {
            let token = {
                let mut q = self.q.lock().unwrap();
                match q.pop_front() {
                    Some(token) => Some(token),
                    None => {
                        on_empty();
                        None
                    }
                }
            };
            match token {
                Some(token) if token.wake() => return,
                Some(_) => continue,
                None => return,
            }
        }
    }

    pub fn signal(&self) {
        self.signal_or_else(|| {});
    }

    pub fn broadcast(&self) {
        let mut q = self.q.lock().unwrap();
        let batch: Vec<WaitToken> = q.drain(..).collect();
        drop(q);
        for token in batch {
            token.wake();
        }
    }
    // AGENT: wake up to n live tokens and skip stale tokens already completed by timeout.
    fn signal_n_or_else(&self, n: usize, mut on_short: impl FnMut(usize)) -> usize {
        let mut woken = 0;
        while woken < n {
            let token = {
                let mut q = self.q.lock().unwrap();
                match q.pop_front() {
                    Some(token) => Some(token),
                    None => {
                        on_short(n - woken);
                        None
                    }
                }
            };
            match token {
                Some(token) if token.wake() => woken += 1,
                Some(_) => continue,
                None => break,
            }
        }
        woken
    }

    pub fn signal_n(&self, n: usize) -> usize {
        self.signal_n_or_else(n, |_| {})
    }

    pub fn pending(&self) -> usize {
        let q = self.q.lock().unwrap();
        q.len()
    }
}

// AGENT: condition-variable style helper: check caller state under its mutex,
// enqueue while that state is still protected, then wait after releasing it.
pub struct ConditionWait {
    waiters: WaitQueue,
}

impl ConditionWait {
    pub fn new() -> Self {
        Self {
            waiters: WaitQueue::new(),
        }
    }

    // AGENT: require the scheduler-aware caller to identify the task that may
    // be parked instead of resolving current task state inside the wait helper.
    pub fn park_on<T>(&self, task_id: usize, g: &Mutex<T>, pred: impl Fn(&T) -> bool) -> bool {
        self.wait_until(task_id, g, |d| if pred(d) { Some(true) } else { None })
    }

    // AGENT: propagate the selected Task::id() through the generic event wait.
    pub fn wait_ev<T>(
        &self,
        task_id: usize,
        g: &Mutex<T>,
        mut cond: impl FnMut(&T) -> Option<bool>,
    ) -> bool {
        self.wait_until(task_id, g, |d| cond(d))
    }

    // AGENT: bind each queued token to the Task::id() supplied by the caller
    // while preserving the condition-check and enqueue atomicity.
    pub fn wait_until<T, R>(
        &self,
        task_id: usize,
        g: &Mutex<T>,
        mut cond: impl FnMut(&mut T) -> Option<R>,
    ) -> R {
        loop {
            let token = {
                let mut d = g.lock().unwrap();
                if let Some(r) = cond(&mut d) {
                    return r;
                }
                self.waiters.enqueue_task_locked(task_id)
            };
            token.wait(None);
        }
    }

    pub fn signal(&self) {
        self.waiters.signal();
    }

    pub fn signal_n(&self, n: usize) -> usize {
        self.waiters.signal_n(n)
    }

    pub fn broadcast(&self) {
        self.waiters.broadcast();
    }

    pub fn pending(&self) -> usize {
        self.waiters.pending()
    }
}

// AGENT: counting event helper; unlike ConditionWait, signal-before-wait is
// remembered through pending_wakes and consumed by later waiters.
pub struct CountingEvent {
    waiters: WaitQueue,
    pending_wakes: AtomicUsize,
}

impl CountingEvent {
    pub fn new() -> Self {
        Self {
            waiters: WaitQueue::new(),
            pending_wakes: AtomicUsize::new(0),
        }
    }

    // AGENT: called while waiters.q is locked, so pending credits and queued
    // waiters are observed as one event state.
    fn take_pending_wake_locked(&self) -> bool {
        let pending = self.pending_wakes.load(Ordering::Relaxed);
        if pending == 0 {
            return false;
        }
        self.pending_wakes.store(pending - 1, Ordering::Relaxed);
        true
    }

    // AGENT: called while waiters.q is locked to preserve signal-before-wait
    // ordering.
    fn add_pending_wakes_locked(&self, count: usize) {
        if count == 0 {
            return;
        }
        let pending = self.pending_wakes.load(Ordering::Relaxed);
        self.pending_wakes.store(
            pending
                .checked_add(count)
                .expect("CountingEvent pending wake credit overflow"),
            Ordering::Relaxed,
        );
    }

    // AGENT: consume a saved wake or enqueue the explicitly identified task
    // without consulting global current-task state.
    pub fn prepare_wait_locked(&self, task_id: usize) -> Option<WaitToken> {
        let mut q = self.waiters.q.lock().unwrap();
        if self.take_pending_wake_locked() {
            None
        } else {
            let token = WaitToken::for_task(task_id);
            q.push_back(token.clone());
            Some(token)
        }
    }

    pub fn signal(&self) {
        self.waiters
            .signal_or_else(|| self.add_pending_wakes_locked(1));
    }

    pub fn signal_n(&self, n: usize) -> usize {
        self.waiters
            .signal_n_or_else(n, |left| self.add_pending_wakes_locked(left))
    }

    pub fn broadcast(&self) {
        self.waiters.broadcast();
    }

    pub fn pending(&self) -> usize {
        self.waiters.pending()
    }
}
