pub struct KernLock {
    flag: AtomicBool,
    holder: AtomicUsize,
    depth: AtomicUsize,
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
        assert!(id <= MAX_THREAD_ID, "thread id {} exceeds MAX_THREAD_ID {}", id, MAX_THREAD_ID);
        if self.holder.load(Ordering::Relaxed) == id {
            // AGENT: sentinel is MAX_THREAD_ID+1, no need for id != 0 guard
            self.depth.fetch_add(1, Ordering::Relaxed);
            return;
        }
        while self.flag.compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed).is_err() {
            core::hint::spin_loop();
        }
        self.holder.store(id, Ordering::Relaxed);
        self.depth.store(1, Ordering::Relaxed);
    }
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
    pub fn held(&self) -> bool { self.flag.load(Ordering::Relaxed) }
    pub fn owner(&self) -> usize { self.holder.load(Ordering::Relaxed) }
    pub fn level(&self) -> usize { self.depth.load(Ordering::Relaxed) }
    pub fn try_enter(&self, id: usize) -> bool {
        assert!(id <= MAX_THREAD_ID, "thread id {} exceeds MAX_THREAD_ID {}", id, MAX_THREAD_ID);
        if self.holder.load(Ordering::Relaxed) == id {
            // AGENT: sentinel is MAX_THREAD_ID+1, no need for id != 0 guard
            self.depth.fetch_add(1, Ordering::Relaxed);
            return true;
        }
        if self.flag.compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed).is_ok() {
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


pub struct Spin { v: AtomicBool }
impl Spin {
    pub const fn new() -> Self { Self { v: AtomicBool::new(false) } }
    pub fn acquire(&self) {
        while self.v.compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed).is_err() {
            core::hint::spin_loop();
        }
    }
    pub fn try_acquire(&self) -> bool {
        self.v.compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed).is_ok()
    }
    pub fn release(&self) { self.v.store(false, Ordering::Release); }
    pub fn is_held(&self) -> bool { self.v.load(Ordering::Relaxed) }
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
    pub fn make() -> Arc<Mutex<Self>> { Arc::new(Mutex::new(Self::default())) }
    pub fn set(&mut self, s: u32) { self.change(0, s); }
    pub fn clear(&mut self, s: u32) { self.change(s, 0); }
    pub fn change(&mut self, rst: u32, s: u32) {
        let orig = self.ev;
        self.ev = (self.ev & !rst) | s;
        if self.ev != orig { self.cbs.retain(|f| !f(self.ev)); }
    }
    pub fn sub(&mut self, cb: Box<dyn Fn(u32) -> bool + Send>) { self.cbs.push(cb); }
    pub fn cb_len(&self) -> usize { self.cbs.len() }
}

pub fn wait_ev(bus: &Arc<Mutex<EvBus>>, mask: u32) -> u32 {
    loop {
        { let g = bus.lock().unwrap(); if (g.ev & mask) != 0 { return g.ev; } }
        thread::yield_now();
    }
}

pub struct RegEp {
    pub task_id: usize,
    pub epfd: usize,
    pub fd: usize,
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
    q: Mutex<VecDeque<thread::Thread>>,
    eq: Mutex<VecDeque<RegEp>>,
}
impl SyncQueue {
    pub fn new() -> Self { Self { q: Mutex::new(VecDeque::new()), eq: Mutex::new(VecDeque::new()) } }
    pub fn park_on<T>(&self, g: &Mutex<T>, pred: impl Fn(&T) -> bool) -> bool {
        let d = g.lock().unwrap();
        let satisfied = pred(&d);
        drop(d);
        if satisfied { return true; }
        let th = thread::current();
        let mut wq = self.q.lock().unwrap();
        wq.push_back(th);
        drop(wq);
        thread::park();
        true
    }
    pub fn signal(&self) {
        let mut q = self.q.lock().unwrap();
        match q.len() {
            0 => {}
            1 => { let t = q.pop_front().unwrap(); drop(q); t.unpark(); }
            _ => { let t = q.pop_front().unwrap(); drop(q); t.unpark(); }
        }
    }
    pub fn broadcast(&self) {
        let mut q = self.q.lock().unwrap();
        let batch: Vec<thread::Thread> = q.drain(..).collect();
        drop(q);
        for t in batch { t.unpark(); }
    }
    // AGENT: replaced locked-while-unparking with batch-drain-then-unpark (consistent with signal/broadcast)
    pub fn signal_n(&self, n: usize) -> usize {
        let mut q = self.q.lock().unwrap();
        let to_wake = n.min(q.len());
        let batch: Vec<_> = q.drain(..to_wake).collect();
        drop(q);
        for t in &batch { t.unpark(); }
        batch.len()
    }
    pub fn pending(&self) -> usize { let q = self.q.lock().unwrap(); q.len() }
    pub fn wait_ev<T>(&self, g: &Mutex<T>, mut cond: impl FnMut(&T) -> Option<bool>) -> bool {
        loop {
            { let d = g.lock().unwrap(); if let Some(r) = cond(&d) { return r; } }
            { let mut q = self.q.lock().unwrap(); q.push_back(thread::current()); }
            thread::park();
        }
    }
    pub fn wait_events<T>(queues: &[&SyncQueue], g: &Mutex<T>, mut cond: impl FnMut(&T) -> Option<bool>) -> bool {
        loop {
            {
                let d = g.lock().unwrap();
                if let Some(r) = cond(&d) { return r; }
            }
            for wq in queues {
                let mut q = wq.q.lock().unwrap();
                q.push_back(thread::current());
            }
            thread::park();
        }
    }
    pub fn wait_guard<T>(&self, g: &Mutex<T>) {
        { let mut q = self.q.lock().unwrap(); q.push_back(thread::current()); }
        drop(g.lock().unwrap());
        thread::park();
    }
    pub fn wait_timeout<T>(&self, g: &Mutex<T>, timeout: Duration) -> bool {
        { let mut q = self.q.lock().unwrap(); q.push_back(thread::current()); }
        drop(g.lock().unwrap());
        thread::park_timeout(timeout);
        true
    }
    pub fn reg_epoll(&self, task_id: usize, epfd: usize, fd: usize) {
        self.eq.lock().unwrap().push_back(RegEp { task_id, epfd, fd });
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

struct SemaInner { cnt: isize, pid: usize, rm: bool, bus: EvBus }

pub struct Sema { inner: Arc<Mutex<SemaInner>> }

pub struct SemaGuard<'a> { s: &'a Sema }

impl Sema {
    pub fn new(c: isize) -> Self {
        Sema { inner: Arc::new(Mutex::new(SemaInner { cnt: c, rm: false, pid: 0, bus: EvBus::default() })) }
    }
    pub fn remove(&self) {
        let mut i = self.inner.lock().unwrap();
        i.rm = true;
        i.bus.set(EvFlag::SEM_RM);
    }
    pub fn release(&self) {
        let mut i = self.inner.lock().unwrap();
        i.cnt += 1;
        if i.cnt >= 1 { i.bus.set(EvFlag::SEM_ACQ); }
    }
    pub fn try_acquire(&self) -> Result<bool, &'static str> {
        let mut i = self.inner.lock().unwrap();
        if i.rm { return Err("removed"); }
        if i.cnt >= 1 {
            i.cnt -= 1;
            if i.cnt < 1 { i.bus.clear(EvFlag::SEM_ACQ); }
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
    pub fn get_val(&self) -> isize { self.inner.lock().unwrap().cnt }
    pub fn get_ncnt(&self) -> usize { self.inner.lock().unwrap().bus.cb_len() }
    pub fn get_pid(&self) -> usize { self.inner.lock().unwrap().pid }
    pub fn set_pid(&self, p: usize) { self.inner.lock().unwrap().pid = p; }
    pub fn set_val(&self, v: isize) {
        let mut i = self.inner.lock().unwrap();
        i.cnt = v;
        if i.cnt >= 1 { i.bus.set(EvFlag::SEM_ACQ); }
    }
}

impl<'a> Drop for SemaGuard<'a> { fn drop(&mut self) { self.s.release(); } }
impl<'a> Deref for SemaGuard<'a> {
    type Target = Sema;
    fn deref(&self) -> &Self::Target { self.s }
}

pub struct FutexBucket {
    waiters: Mutex<VecDeque<(usize, thread::Thread, Arc<AtomicBool>)>>,
}
impl FutexBucket {
    pub fn new() -> Self { Self { waiters: Mutex::new(VecDeque::new()) } }
    // AGENT: added assert to enforce addr == val address
    pub fn wait(&self, addr: usize, expected: u32, val: &AtomicU32, timeout: Option<Duration>) -> Result<(), &'static str> {
        assert_eq!(val.as_ptr() as usize, addr, "addr must match val address");
        let flag = Arc::new(AtomicBool::new(false));
        if val.load(Ordering::SeqCst) != expected { return Err("changed"); }
        { let mut w = self.waiters.lock().unwrap();
          w.push_back((addr, thread::current(), flag.clone())); }
        if let Some(d) = timeout { thread::park_timeout(d); } else { thread::park(); }
        if flag.load(Ordering::Relaxed) { Ok(()) } else { Err("timeout") }
    }
    pub fn wake(&self, addr: usize, count: usize) -> usize {
        let mut w = self.waiters.lock().unwrap();
        let mut woken = 0;
        w.retain(|(a, t, f)| {
            if *a == addr && woken < count {
                f.store(true, Ordering::Relaxed);
                t.unpark();
                woken += 1;
                false
            } else { true }
        });
        woken
    }
    pub fn requeue(&self, src: usize, dst: usize, wake_n: usize, move_n: usize) -> usize {
        let mut w = self.waiters.lock().unwrap();
        let (mut wk, mut mv) = (0, 0);
        for e in w.iter_mut() {
            if e.0 == src {
                if wk < wake_n {
                    e.2.store(true, Ordering::Relaxed);
                    e.1.unpark();
                    wk += 1;
                } else if mv < move_n {
                    e.0 = dst;
                    mv += 1;
                }
            }
        }
        w.retain(|(_, _, f)| !f.load(Ordering::Relaxed));
        wk
    }
    pub fn pending_at(&self, addr: usize) -> usize {
        self.waiters.lock().unwrap().iter().filter(|(a, _, _)| *a == addr).count()
    }
}

pub struct FutexTable {
    table: Mutex<VecDeque<(usize, thread::Thread)>>,
}

impl FutexTable {
    pub fn new() -> Self { Self { table: Mutex::new(VecDeque::new()) } }

    // AGENT: added assert to enforce addr == val address
    pub fn ftx_wait(&self, addr: usize, expected: u32, val: &AtomicU32) -> bool {
        assert_eq!(val.as_ptr() as usize, addr, "addr must match val address");
        if val.load(Ordering::SeqCst) != expected { return false; }
        let mut wq = self.table.lock().unwrap();
        wq.push_back((addr, thread::current()));
        drop(wq);
        thread::park();
        true
    }

    // HUMAN: delete the "target" and "limit" variables
    // AGENT: fixed wk-count matching logic — wk += 1 only when actually woken, tightened while condition
    pub fn ftx_wake(&self, addr: usize, count: usize) -> usize {
        let mut wq = self.table.lock().unwrap();
        let mut wk = 0usize;
        let mut cursor = 0;
        while cursor < wq.len() && wk < count {
            if wq[cursor].0 == addr {
                let entry = wq.remove(cursor).unwrap();
                entry.1.unpark();
                wk += 1;
            } else {
                cursor += 1;
            }
        }
        wk
    }

    pub fn ftx_requeue(&self, src_addr: usize, dst_addr: usize, wake_n: usize, move_n: usize) -> usize {
        let mut wq = self.table.lock().unwrap();
        let mut wk = 0;
        let mut mv = 0;
        let mut i = 0;
        while i < wq.len() {
            if wq[i].0 == src_addr {
                if wk < wake_n {
                    let (_, t) = wq.remove(i).unwrap();
                    t.unpark();
                    wk += 1;
                } else if mv < move_n {
                    wq[i].0 = dst_addr;
                    mv += 1;
                    i += 1;
                } else {
                    i += 1;
                }
            } else {
                i += 1;
            }
        }
        wk
    }
}
