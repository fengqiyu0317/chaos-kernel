// AGENT
use super::*;

// AGENT: Usage map for this module in the current kernel-sim code.
//
// Active paths:
// - GKL/KernLock backs Kernel::tick() and BlockCache::sync_all(), though those
//   callers mostly touch GKL fields directly and only call leave().
// - Spin backs cache-chain locking and Channel; several callers still access
//   Spin.v directly.
// - EvBus/EvFlag is used as event-bit storage by pipe, process exit/signal,
//   and semaphore state transitions.
// - WaitToken is the common host-thread wait token used by Channel,
//   proc::WaitQueue, SyncQueue helpers, and FutexBucket.
// - SyncQueue is used by Channel through new(), signal(), broadcast(), and
//   direct access to q.
// - FutexBucket is wired to SYS_FUTEX and process-exit cleanup.
//
// Partially wired paths:
// - Sema is created through SemArr/SemCtx and uses remove()/release(), but
//   semget/semop/semctl-style syscall dispatch is not present.
//
// Unused or reserved paths:
// - KernLock::enter/held/owner/level/try_enter; Spin::try_acquire/is_held.
// - EvCb, EvBus::sub(), top-level wait_ev(), and EvFlag::WRITABLE/ERROR.
// - RegEp and SyncQueue's generic wait/timeout/epoll-registration helpers.
// - WaitToken::id() and SocketState.
// AGENT TODO: Tighten KernLock into a proper recursive GKL abstraction before
// treating it as real kernel-lock semantics: keep fields private, route
// Kernel::tick()/BlockCache::sync_all() through enter()/try_enter(), make
// release owner-checked or guard-based, and document or implement the missing
// real-kernel pieces such as fairness, preemption, and interrupt control.
pub struct KernLock {
    pub(crate) flag: AtomicBool,
    pub(crate) holder: AtomicUsize,
    pub(crate) depth: AtomicUsize,
}
impl KernLock {
    pub const fn new() -> Self {
        Self {
            flag: AtomicBool::new(false),
            holder: AtomicUsize::new(MAX_THREAD_ID + 1), // AGENT
            depth: AtomicUsize::new(0),
        }
    }
    pub fn enter(&self, id: usize) {
        assert!(
            id <= MAX_THREAD_ID,
            "thread id {} exceeds MAX_THREAD_ID {}",
            id,
            MAX_THREAD_ID
        );
        if self.holder.load(Ordering::Relaxed) == id {
            // AGENT: sentinel is MAX_THREAD_ID+1, no need for id != 0 guard
            self.depth.fetch_add(1, Ordering::Relaxed);
            return;
        }
        while self
            .flag
            .compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed)
            .is_err()
        {
            ::core::hint::spin_loop();
        }
        self.holder.store(id, Ordering::Relaxed);
        self.depth.store(1, Ordering::Relaxed);
    }
    // AGENT TODO: Replace bare leave() with leave(id) or a KernLockGuard Drop
    // path so non-owners cannot accidentally release the lock and underflow or
    // mismatched recursion depth can be caught explicitly.
    pub fn leave(&self) {
        let d = self.depth.load(Ordering::Relaxed);
        if d > 1 {
            self.depth.store(d - 1, Ordering::Relaxed);
        } else {
            self.holder.store(MAX_THREAD_ID + 1, Ordering::Relaxed); // AGENT
            self.depth.store(0, Ordering::Relaxed);
            self.flag.store(false, Ordering::Release);
        }
    }
    pub fn held(&self) -> bool {
        self.flag.load(Ordering::Relaxed)
    }
    pub fn owner(&self) -> usize {
        self.holder.load(Ordering::Relaxed)
    }
    pub fn level(&self) -> usize {
        self.depth.load(Ordering::Relaxed)
    }
    pub fn try_enter(&self, id: usize) -> bool {
        assert!(
            id <= MAX_THREAD_ID,
            "thread id {} exceeds MAX_THREAD_ID {}",
            id,
            MAX_THREAD_ID
        );
        if self.holder.load(Ordering::Relaxed) == id {
            // AGENT: sentinel is MAX_THREAD_ID+1, no need for id != 0 guard
            self.depth.fetch_add(1, Ordering::Relaxed);
            return true;
        }
        if self
            .flag
            .compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed)
            .is_ok()
        {
            self.holder.store(id, Ordering::Relaxed);
            self.depth.store(1, Ordering::Relaxed);
            true
        } else {
            false
        }
    }
}
unsafe impl Send for KernLock {}
unsafe impl Sync for KernLock {}
pub static GKL: KernLock = KernLock::new();

pub struct Spin {
    pub(crate) v: AtomicBool,
}
impl Spin {
    pub const fn new() -> Self {
        Self {
            v: AtomicBool::new(false),
        }
    }
    pub fn acquire(&self) {
        while self
            .v
            .compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed)
            .is_err()
        {
            ::core::hint::spin_loop();
        }
    }
    pub fn try_acquire(&self) -> bool {
        self.v
            .compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed)
            .is_ok()
    }
    pub fn release(&self) {
        self.v.store(false, Ordering::Release);
    }
    pub fn is_held(&self) -> bool {
        self.v.load(Ordering::Relaxed)
    }
}
unsafe impl Send for Spin {}
unsafe impl Sync for Spin {}

// pub struct FlgGuard(usize);
// impl FlgGuard { pub fn enter() -> Self { Self(0) } }
// impl Drop for FlgGuard { fn drop(&mut self) {} }

pub struct EvFlag;
impl EvFlag {
    pub const READABLE: u32 = 1 << 0;
    pub const WRITABLE: u32 = 1 << 1;
    pub const ERROR: u32 = 1 << 2;
    pub const CLOSED: u32 = 1 << 3;
    pub const PROC_QUIT: u32 = 1 << 10;
    pub const CHILD_QUIT: u32 = 1 << 11;
    pub const RECV_SIG: u32 = 1 << 12;
    pub const SEM_RM: u32 = 1 << 20;
    pub const SEM_ACQ: u32 = 1 << 21;
}

pub type EvCb = Box<dyn Fn(u32) -> bool + Send>;

#[derive(Default)]
pub struct EvBus {
    pub ev: u32,
    pub cbs: Vec<Box<dyn Fn(u32) -> bool + Send>>,
}
impl EvBus {
    pub fn make() -> Arc<Mutex<Self>> {
        Arc::new(Mutex::new(Self::default()))
    }
    pub fn set(&mut self, s: u32) {
        self.change(0, s);
    }
    pub fn clear(&mut self, s: u32) {
        self.change(s, 0);
    }
    pub fn change(&mut self, rst: u32, s: u32) {
        let orig = self.ev;
        self.ev = (self.ev & !rst) | s;
        if self.ev != orig {
            self.cbs.retain(|f| !f(self.ev));
        }
    }
    pub fn sub(&mut self, cb: Box<dyn Fn(u32) -> bool + Send>) {
        self.cbs.push(cb);
    }
    pub fn cb_len(&self) -> usize {
        self.cbs.len()
    }
}

pub fn wait_ev(bus: &Arc<Mutex<EvBus>>, mask: u32) -> u32 {
    loop {
        {
            let g = bus.lock().unwrap();
            if (g.ev & mask) != 0 {
                return g.ev;
            }
        }
        thread::yield_now();
    }
}

pub struct RegEp {
    pub task_id: usize,
    pub epfd: usize,
    pub fd: usize,
}

// AGENT: keep host-thread parking behind a token so kernel wait queues do not
// store std::thread::Thread directly.
static WAIT_TOKEN_SEQ: AtomicUsize = AtomicUsize::new(1);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WaitOutcome {
    Event,
    Timeout,
}

const WAIT_PENDING: u8 = 0;
const WAIT_EVENT: u8 = 1;
const WAIT_TIMEOUT: u8 = 2;

#[derive(Clone)]
pub struct WaitToken {
    id: usize,
    state: Arc<WaitState>,
}

struct WaitState {
    outcome: AtomicU8,
    host: HostWaiter,
}

struct HostWaiter {
    thread: thread::Thread,
}

impl HostWaiter {
    fn current() -> Self {
        Self {
            thread: thread::current(),
        }
    }

    fn park(&self) {
        thread::park();
    }

    fn park_timeout(&self, timeout: Duration) {
        thread::park_timeout(timeout);
    }

    fn wake(&self) {
        self.thread.unpark();
    }
}

impl WaitToken {
    pub fn current() -> Self {
        Self {
            id: WAIT_TOKEN_SEQ.fetch_add(1, Ordering::Relaxed),
            state: Arc::new(WaitState {
                outcome: AtomicU8::new(WAIT_PENDING),
                host: HostWaiter::current(),
            }),
        }
    }

    pub fn id(&self) -> usize {
        self.id
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
            self.state.host.wake();
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
            self.state.host.wake();
            true
        } else {
            false
        }
    }

    pub fn wait(&self, timeout: Option<Duration>) -> WaitOutcome {
        match timeout {
            Some(d) => {
                let deadline = std::time::Instant::now() + d;
                while !self.is_woken() {
                    let now = std::time::Instant::now();
                    if now >= deadline {
                        self.wake_timeout();
                        break;
                    }
                    self.state.host.park_timeout(deadline - now);
                }
            }
            None => {
                while !self.is_woken() {
                    self.state.host.park();
                }
            }
        }
        self.outcome()
    }

    // AGENT: wait using the logical kernel timer wheel instead of host
    // Instant/park_timeout.
    pub fn wait_with_timer(&self, timeout: Duration) -> WaitOutcome {
        let ticks = duration_to_ticks(timeout);
        if ticks == 0 {
            self.wake_timeout();
            return self.outcome();
        }
        let deadline = CLK.load(Ordering::Relaxed).saturating_add(ticks);
        let timers = global_timer_wheel();
        let timer_id = {
            let mut wheel = timers.lock().unwrap();
            wheel.register_timer(
                deadline,
                0,
                TimerTarget::WakeToken {
                    token: self.clone(),
                },
            )
        };
        let outcome = self.wait(None);
        if outcome == WaitOutcome::Event {
            timers.lock().unwrap().cancel(timer_id);
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
            _ => WaitOutcome::Event,
        }
    }

    pub fn same(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.state, &other.state)
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum SocketState {
    Closed,
    Listen,
    SynSent,
    SynRecvd,
    Established,
    FinWait1,
    FinWait2,
    TimeWait,
    CloseWait,
    LastAck,
    Closing,
}

pub struct SyncQueue {
    pub(crate) q: Mutex<VecDeque<WaitToken>>,
    eq: Mutex<VecDeque<RegEp>>,
}
impl SyncQueue {
    pub fn new() -> Self {
        Self {
            q: Mutex::new(VecDeque::new()),
            eq: Mutex::new(VecDeque::new()),
        }
    }
    pub fn park_on<T>(&self, g: &Mutex<T>, pred: impl Fn(&T) -> bool) -> bool {
        let d = g.lock().unwrap();
        let satisfied = pred(&d);
        drop(d);
        if satisfied {
            return true;
        }
        let token = WaitToken::current();
        let mut wq = self.q.lock().unwrap();
        wq.push_back(token.clone());
        drop(wq);
        token.wait(None);
        let d = g.lock().unwrap();
        pred(&d)
    }
    pub fn signal(&self) {
        loop {
            let token = {
                let mut q = self.q.lock().unwrap();
                q.pop_front()
            };
            match token {
                Some(token) if token.wake() => return,
                Some(_) => continue,
                None => return,
            }
        }
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
    pub fn signal_n(&self, n: usize) -> usize {
        let mut woken = 0;
        while woken < n {
            let token = {
                let mut q = self.q.lock().unwrap();
                q.pop_front()
            };
            match token {
                Some(token) if token.wake() => woken += 1,
                Some(_) => continue,
                None => break,
            }
        }
        woken
    }
    pub fn pending(&self) -> usize {
        let q = self.q.lock().unwrap();
        q.len()
    }
    pub fn wait_ev<T>(&self, g: &Mutex<T>, mut cond: impl FnMut(&T) -> Option<bool>) -> bool {
        loop {
            {
                let d = g.lock().unwrap();
                if let Some(r) = cond(&d) {
                    return r;
                }
            }
            let token = WaitToken::current();
            {
                let mut q = self.q.lock().unwrap();
                q.push_back(token.clone());
            }
            token.wait(None);
        }
    }
    pub fn wait_events<T>(
        queues: &[&SyncQueue],
        g: &Mutex<T>,
        mut cond: impl FnMut(&T) -> Option<bool>,
    ) -> bool {
        loop {
            {
                let d = g.lock().unwrap();
                if let Some(r) = cond(&d) {
                    return r;
                }
            }
            let token = WaitToken::current();
            for wq in queues {
                let mut q = wq.q.lock().unwrap();
                q.push_back(token.clone());
            }
            token.wait(None);
            for wq in queues {
                let mut q = wq.q.lock().unwrap();
                q.retain(|queued| !queued.same(&token));
            }
        }
    }
    pub fn wait_guard<T>(&self, g: &Mutex<T>) {
        let token = WaitToken::current();
        {
            let mut q = self.q.lock().unwrap();
            q.push_back(token.clone());
        }
        drop(g.lock().unwrap());
        token.wait(None);
    }
    pub fn wait_timeout<T>(&self, g: &Mutex<T>, timeout: Duration) -> bool {
        let token = WaitToken::current();
        {
            let mut q = self.q.lock().unwrap();
            q.push_back(token.clone());
        }
        drop(g.lock().unwrap());
        match token.wait(Some(timeout)) {
            WaitOutcome::Event => true,
            WaitOutcome::Timeout => {
                let mut q = self.q.lock().unwrap();
                q.retain(|queued| !queued.same(&token));
                false
            }
        }
    }
    pub fn reg_epoll(&self, task_id: usize, epfd: usize, fd: usize) {
        self.eq
            .lock()
            .unwrap()
            .push_back(RegEp { task_id, epfd, fd });
    }
    pub fn unreg_epoll(&self, task_id: usize, epfd: usize, fd: usize) -> bool {
        let mut eql = self.eq.lock().unwrap();
        for i in 0..eql.len() {
            if eql[i].task_id == task_id && eql[i].epfd == epfd && eql[i].fd == fd {
                eql.remove(i);
                return true;
            }
        }
        false
    }
}

struct SemaInner {
    cnt: isize,
    pid: usize,
    rm: bool,
    bus: EvBus,
}

pub struct Sema {
    inner: Arc<Mutex<SemaInner>>,
}

pub struct SemaGuard<'a> {
    s: &'a Sema,
}

impl Sema {
    pub fn new(c: isize) -> Self {
        Sema {
            inner: Arc::new(Mutex::new(SemaInner {
                cnt: c,
                rm: false,
                pid: 0,
                bus: EvBus::default(),
            })),
        }
    }
    pub fn remove(&self) {
        let mut i = self.inner.lock().unwrap();
        i.rm = true;
        i.bus.set(EvFlag::SEM_RM);
    }
    pub fn release(&self) {
        let mut i = self.inner.lock().unwrap();
        i.cnt += 1;
        if i.cnt >= 1 {
            i.bus.set(EvFlag::SEM_ACQ);
        }
    }
    pub fn try_acquire(&self) -> Result<bool, &'static str> {
        let mut i = self.inner.lock().unwrap();
        if i.rm {
            return Err("removed");
        }
        if i.cnt >= 1 {
            i.cnt -= 1;
            if i.cnt < 1 {
                i.bus.clear(EvFlag::SEM_ACQ);
            }
            Ok(true)
        } else {
            Ok(false)
        }
    }
    pub fn acquire_spin(&self) -> Result<(), &'static str> {
        loop {
            match self.try_acquire()? {
                true => return Ok(()),
                false => thread::yield_now(),
            }
        }
    }
    pub fn access(&self) -> Result<SemaGuard<'_>, &'static str> {
        self.acquire_spin()?;
        Ok(SemaGuard { s: self })
    }
    pub fn get_val(&self) -> isize {
        self.inner.lock().unwrap().cnt
    }
    pub fn get_ncnt(&self) -> usize {
        self.inner.lock().unwrap().bus.cb_len()
    }
    pub fn get_pid(&self) -> usize {
        self.inner.lock().unwrap().pid
    }
    pub fn set_pid(&self, p: usize) {
        self.inner.lock().unwrap().pid = p;
    }
    pub fn set_val(&self, v: isize) {
        let mut i = self.inner.lock().unwrap();
        i.cnt = v;
        if i.cnt >= 1 {
            i.bus.set(EvFlag::SEM_ACQ);
        }
    }
}

impl<'a> Drop for SemaGuard<'a> {
    fn drop(&mut self) {
        self.s.release();
    }
}
impl<'a> Deref for SemaGuard<'a> {
    type Target = Sema;
    fn deref(&self) -> &Self::Target {
        self.s
    }
}

// AGENT: futex wait queues keep kernel-style wait tokens instead of host
// thread handles.
#[derive(Clone)]
struct FutexWaiter {
    addr: usize,
    token: WaitToken,
}

// AGENT: keep wake and move counts separate because FUTEX_REQUEUE and
// FUTEX_CMP_REQUEUE expose different return-value semantics.
struct FutexRequeueResult {
    woken: usize,
    moved: usize,
}

impl FutexRequeueResult {
    fn affected(&self) -> usize {
        self.woken + self.moved
    }
}

// AGENT: distinguish futex timeout backends while sharing the waiter setup.
#[derive(Clone, Copy)]
enum FutexWaitClock {
    Host,
    KernelTimer,
}

pub struct FutexBucket {
    waiters: Mutex<VecDeque<FutexWaiter>>,
}
impl FutexBucket {
    pub fn new() -> Self {
        Self {
            waiters: Mutex::new(VecDeque::new()),
        }
    }
    // AGENT: added assert to enforce addr == val address
    pub fn wait(
        &self,
        addr: usize,
        expected: u32,
        val: &AtomicU32,
        timeout: Option<Duration>,
    ) -> Result<(), &'static str> {
        self.wait_inner(addr, expected, val, timeout, FutexWaitClock::Host)
    }

    // AGENT: futex syscall timeouts use the kernel timer wheel so timeout wakeup
    // follows the same logical clock as scheduler ticks.
    pub fn wait_with_timer(
        &self,
        addr: usize,
        expected: u32,
        val: &AtomicU32,
        timeout: Option<Duration>,
    ) -> Result<(), &'static str> {
        self.wait_inner(addr, expected, val, timeout, FutexWaitClock::KernelTimer)
    }

    // AGENT: compare and enqueue under one queue lock so a wake cannot slip
    // between seeing the expected value and publishing this waiter.
    fn wait_inner(
        &self,
        addr: usize,
        expected: u32,
        val: &AtomicU32,
        timeout: Option<Duration>,
        clock: FutexWaitClock,
    ) -> Result<(), &'static str> {
        assert_eq!(val.as_ptr() as usize, addr, "addr must match val address");
        let token = WaitToken::current();
        {
            let mut w = self.waiters.lock().unwrap();
            if val.load(Ordering::SeqCst) != expected {
                return Err("changed");
            }
            w.push_back(FutexWaiter {
                addr,
                token: token.clone(),
            });
        }

        let outcome = match (clock, timeout) {
            (FutexWaitClock::KernelTimer, Some(timeout)) => token.wait_with_timer(timeout),
            _ => token.wait(timeout),
        };
        self.finish_wait(&token, outcome)
    }

    fn finish_wait(&self, token: &WaitToken, outcome: WaitOutcome) -> Result<(), &'static str> {
        match outcome {
            WaitOutcome::Event => Ok(()),
            WaitOutcome::Timeout => {
                let mut w = self.waiters.lock().unwrap();
                w.retain(|waiter| !waiter.token.same(token));
                Err("timeout")
            }
        }
    }
    pub fn wake(&self, addr: usize, count: usize) -> usize {
        let mut w = self.waiters.lock().unwrap();
        Self::wake_locked(&mut w, addr, count)
    }
    // AGENT: process exit wakes and removes every futex waiter owned by this bucket.
    pub fn wake_all(&self) -> usize {
        let mut w = self.waiters.lock().unwrap();
        let count = w.len();
        for waiter in w.drain(..) {
            waiter.token.wake();
        }
        count
    }
    pub fn wake_op(
        &self,
        addr: usize,
        count: usize,
        addr2: usize,
        count2: usize,
        op: impl FnOnce() -> Result<u32, &'static str>,
        cmp: impl FnOnce(u32) -> Result<bool, &'static str>,
    ) -> Result<usize, &'static str> {
        let mut w = self.waiters.lock().unwrap();
        let old = op()?;
        let should_wake_addr2 = cmp(old)?;
        let mut woken = Self::wake_locked(&mut w, addr, count);
        if should_wake_addr2 {
            woken += Self::wake_locked(&mut w, addr2, count2);
        }
        Ok(woken)
    }
    pub fn requeue(&self, src: usize, dst: usize, wake_n: usize, move_n: usize) -> usize {
        let mut w = self.waiters.lock().unwrap();
        Self::requeue_locked(&mut w, src, dst, wake_n, move_n).woken
    }
    pub fn cmp_requeue(
        &self,
        src: usize,
        dst: usize,
        wake_n: usize,
        move_n: usize,
        val: &AtomicU32,
        expected: u32,
    ) -> Result<usize, &'static str> {
        assert_eq!(val.as_ptr() as usize, src, "addr must match val address");
        let mut w = self.waiters.lock().unwrap();
        if val.load(Ordering::SeqCst) != expected {
            return Err("changed");
        }
        Ok(Self::requeue_locked(&mut w, src, dst, wake_n, move_n).affected())
    }
    pub fn pending_at(&self, addr: usize) -> usize {
        self.waiters
            .lock()
            .unwrap()
            .iter()
            .filter(|waiter| waiter.addr == addr)
            .count()
    }
    fn wake_locked(waiters: &mut VecDeque<FutexWaiter>, addr: usize, count: usize) -> usize {
        let mut woken = 0;
        waiters.retain(|waiter| {
            if waiter.addr == addr && woken < count {
                if waiter.token.wake() {
                    woken += 1;
                }
                false
            } else {
                true
            }
        });
        woken
    }
    fn requeue_locked(
        waiters: &mut VecDeque<FutexWaiter>,
        src: usize,
        dst: usize,
        wake_n: usize,
        move_n: usize,
    ) -> FutexRequeueResult {
        let (mut wk, mut mv) = (0, 0);
        for waiter in waiters.iter_mut() {
            if waiter.addr == src {
                if wk < wake_n {
                    if waiter.token.wake() {
                        wk += 1;
                    }
                } else if mv < move_n {
                    waiter.addr = dst;
                    mv += 1;
                }
            }
        }
        waiters.retain(|waiter| !waiter.token.is_woken());
        FutexRequeueResult {
            woken: wk,
            moved: mv,
        }
    }
}
