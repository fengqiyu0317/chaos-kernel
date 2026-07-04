// AGENT
use crate::kernel::kernel_core::arch::CLK;
use crate::kernel::kernel_core::current::require_current_task_id;
use crate::kernel::kernel_core::kernel_base::Kernel;
use crate::kernel::kernel_core::prelude::*;
use crate::kernel::kernel_core::time::{duration_to_ticks, global_timer_wheel, TimerTarget};

// AGENT: keep QEMU scheduler wakeups behind a token so kernel wait queues store
// task identities instead of host std::thread handles.
static WAIT_TOKEN_SEQ: AtomicUsize = AtomicUsize::new(1);
// AGENT: QEMU wait tokens need a scheduler owner to move tasks between
// Sleeping and Runnable without threading a Kernel parameter through every
// migrated kernel-sim wait queue. The pointer must be installed from a leaked
// or static Kernel before real QEMU task waits are exercised.
pub(super) static WAIT_KERNEL: AtomicUsize = AtomicUsize::new(0);

// AGENT: install the QEMU scheduler backend used by WaitToken wake/block paths.
pub fn install_qemu_wait_kernel(kernel: &'static Kernel) {
    WAIT_KERNEL.store(kernel as *const Kernel as usize, Ordering::Release);
}

// AGENT: return the installed QEMU kernel backend, if this early carrier stage
// has one. Wait paths and the RISC-V syscall ABI share this single leaked Kernel.
pub(crate) fn qemu_wait_kernel() -> Option<&'static Kernel> {
    let ptr = WAIT_KERNEL.load(Ordering::Acquire);
    if ptr == 0 {
        None
    } else {
        // SAFETY: install_qemu_wait_kernel only accepts a 'static Kernel.
        Some(unsafe { &*(ptr as *const Kernel) })
    }
}

// AGENT: QEMU timer interrupts use this hook to drive the migrated logical
// kernel clock and timer wheel once a Kernel has been installed.
pub(crate) fn qemu_wait_timer_tick() {
    if let Some(kernel) = qemu_wait_kernel() {
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

#[derive(Clone)]
pub struct WaitToken {
    id: usize,
    state: Arc<WaitState>,
}

struct WaitState {
    outcome: AtomicU8,
    task_id: usize,
}

impl WaitToken {
    // AGENT: QEMU wait ownership is the current simulator task, not a host
    // thread. Kernel::set_cur() publishes this id before syscall/wait code runs.
    pub fn current() -> Self {
        Self {
            id: WAIT_TOKEN_SEQ.fetch_add(1, Ordering::Relaxed),
            state: Arc::new(WaitState {
                outcome: AtomicU8::new(WAIT_PENDING),
                task_id: require_current_task_id("WaitToken"),
            }),
        }
    }

    pub fn id(&self) -> usize {
        self.id
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
        if let Some(kernel) = qemu_wait_kernel() {
            kernel.wake_task_for_wait(self.state.task_id);
        }
    }

    // AGENT: park the owning task in scheduler state. The full context switch is
    // supplied by a later QEMU scheduler milestone; until then this function
    // records the semantic state transition and the loop below spins.
    fn block_waiter_task(&self) {
        if let Some(kernel) = qemu_wait_kernel() {
            kernel.block_task_for_wait(self.state.task_id);
        }
    }

    // AGENT: repair the current task state after the temporary spin wait bridge
    // sees completion but before callers continue on the same kernel stack.
    fn finish_waiter_task(&self) {
        if let Some(kernel) = qemu_wait_kernel() {
            kernel.finish_task_wait(self.state.task_id);
        }
    }

    // AGENT: interruptible waits observe pending task signals separately from
    // resource readiness so syscall code can return EINTR instead of success.
    fn has_interrupting_signal(&self) -> bool {
        qemu_wait_kernel()
            .is_some_and(|kernel| kernel.task_has_interrupting_signal(self.state.task_id))
    }

    // AGENT: shared wait loop for plain and signal-interruptible waits during
    // the current spin-based QEMU scheduler bridge.
    fn wait_inner(&self, interruptible: bool) -> WaitOutcome {
        let mut blocked = false;
        while !self.is_woken() {
            if interruptible && self.has_interrupting_signal() {
                self.wake_signal();
                break;
            }
            if !blocked {
                self.block_waiter_task();
                blocked = true;
                if self.is_woken() {
                    break;
                }
            }
            ::core::hint::spin_loop();
        }
        if blocked {
            self.finish_waiter_task();
        }
        self.outcome()
    }

    // AGENT: QEMU has no host Instant/park_timeout. Optional timeouts are routed
    // through the kernel timer wheel, while indefinite waits block the current
    // task and spin until the eventual scheduler/context-switch layer resumes it.
    pub fn wait(&self, timeout: Option<Duration>) -> WaitOutcome {
        if let Some(timeout) = timeout {
            return self.wait_with_timer(timeout);
        }
        self.wait_inner(false)
    }

    // AGENT: syscall-facing waits use this variant when a pending signal should
    // interrupt the wait and be delivered at the syscall return boundary.
    pub fn wait_interruptible(&self, timeout: Option<Duration>) -> WaitOutcome {
        if let Some(timeout) = timeout {
            return self.wait_with_timer_inner(timeout, true);
        }
        self.wait_inner(true)
    }

    // AGENT: wait using the logical kernel timer wheel instead of host
    // Instant/park_timeout.
    pub fn wait_with_timer(&self, timeout: Duration) -> WaitOutcome {
        self.wait_with_timer_inner(timeout, false)
    }

    // AGENT: keep timeout setup common between plain and interruptible waits.
    fn wait_with_timer_inner(&self, timeout: Duration, interruptible: bool) -> WaitOutcome {
        let ticks = duration_to_ticks(timeout);
        if ticks == 0 {
            self.wake_timeout();
            return self.outcome();
        }
        let deadline = CLK.load(Ordering::Relaxed).saturating_add(ticks);
        self.wait_until_tick_inner(deadline, interruptible)
    }

    // AGENT: wait until an absolute logical tick deadline, using the same typed
    // timer target that QEMU timer interrupts will dispatch.
    pub fn wait_until_tick(&self, deadline: usize) -> WaitOutcome {
        self.wait_until_tick_inner(deadline, false)
    }

    // AGENT: absolute-deadline variant for syscall waits that can be interrupted
    // by signals before the timer fires.
    pub fn wait_until_tick_interruptible(&self, deadline: usize) -> WaitOutcome {
        self.wait_until_tick_inner(deadline, true)
    }

    fn wait_until_tick_inner(&self, deadline: usize, interruptible: bool) -> WaitOutcome {
        if self.is_woken() {
            return self.outcome();
        }
        if CLK.load(Ordering::Relaxed) >= deadline {
            self.wake_timeout();
            return self.outcome();
        }
        let timers = global_timer_wheel();
        let timer_id = {
            let mut wheel = timers.lock();
            wheel.register_timer(
                deadline,
                0,
                TimerTarget::WakeToken {
                    token: self.clone(),
                },
            )
        };
        let outcome = self.wait_inner(interruptible);
        if outcome != WaitOutcome::Timeout {
            timers.lock().cancel(timer_id);
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

    // AGENT: enqueue the current task while the caller still holds the condition
    // state lock, then let the caller drop that state lock before blocking.
    pub fn enqueue_current_locked(&self) -> WaitToken {
        let token = WaitToken::current();
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

    pub fn park_on<T>(&self, g: &Mutex<T>, pred: impl Fn(&T) -> bool) -> bool {
        self.wait_until(g, |d| if pred(d) { Some(true) } else { None })
    }

    pub fn wait_ev<T>(&self, g: &Mutex<T>, mut cond: impl FnMut(&T) -> Option<bool>) -> bool {
        self.wait_until(g, |d| cond(d))
    }

    pub fn wait_until<T, R>(&self, g: &Mutex<T>, mut cond: impl FnMut(&mut T) -> Option<R>) -> R {
        loop {
            let token = {
                let mut d = g.lock().unwrap();
                if let Some(r) = cond(&mut d) {
                    return r;
                }
                self.waiters.enqueue_current_locked()
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

    pub fn prepare_wait_locked(&self) -> Option<WaitToken> {
        let mut q = self.waiters.q.lock().unwrap();
        if self.take_pending_wake_locked() {
            None
        } else {
            let token = WaitToken::current();
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
