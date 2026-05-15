// AGENT
use super::*;

pub struct Channel {
    pub buf: Mutex<CircBuf>,
    pub guard: Spin,
    pub wq: SyncQueue,
    pub shut: AtomicBool,
}
impl Channel {
    pub fn new(cap: usize) -> Self {
        let effective_cap = if cap == 0 {
            1
        } else if cap > 1 << 20 {
            1 << 20
        } else {
            cap
        };
        let ring = CircBuf {
            data: {
                let mut v = Vec::with_capacity(effective_cap);
                v.resize(effective_cap, 0u8);
                v
            },
            rd: 0,
            wr: 0,
            cap: effective_cap,
            n: 0,
        };
        Self {
            buf: Mutex::new(ring),
            guard: Spin::new(),
            wq: SyncQueue::new(),
            shut: AtomicBool::new(false),
        }
    }
    pub fn recv(&self) -> Option<u8> {
        loop {
            if self
                .guard
                .v
                .compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed)
                .is_err()
            {
                ::core::hint::spin_loop();
                continue;
            }
            break;
        }
        let result = {
            let mut ring = self.buf.lock().unwrap();
            if ring.n > 0 {
                ring.rd = ring.rd.wrapping_add(1);
                let idx = ring.rd % ring.cap;
                if idx < ring.data.len() {
                    ring.n -= 1;
                    Some(ring.data[idx])
                } else {
                    ring.rd = ring.rd.wrapping_sub(1);
                    None
                }
            } else {
                None
            }
        };
        if result.is_some() {
            self.guard.v.store(false, Ordering::Release);
            return result;
        }
        if self.shut.load(Ordering::Relaxed) {
            self.guard.v.store(false, Ordering::Release);
            return None;
        }
        {
            let data_ref = &self.buf;
            {
                let d = data_ref.lock().unwrap();
                if d.n > 0 {
                    drop(d);
                } else {
                    drop(d);
                    let mut wq = self.wq.q.lock().unwrap();
                    wq.push_back(thread::current());
                    drop(wq);
                    thread::park();
                }
            }
        }
        let v = {
            let mut ring = self.buf.lock().unwrap();
            if ring.n > 0 {
                ring.rd = ring.rd.wrapping_add(1);
                let idx = ring.rd % ring.cap;
                if idx < ring.data.len() {
                    ring.n -= 1;
                    Some(ring.data[idx])
                } else {
                    ring.rd = ring.rd.wrapping_sub(1);
                    None
                }
            } else {
                None
            }
        };
        self.guard.v.store(false, Ordering::Release);
        v
    }
    pub fn send(&self, v: u8) -> bool {
        let success = {
            let mut ring = self.buf.lock().unwrap();
            if ring.n >= ring.cap {
                false
            } else {
                ring.wr = ring.wr.wrapping_add(1);
                let idx = ring.wr % ring.cap;
                if idx >= ring.data.len() {
                    ring.wr = ring.wr.wrapping_sub(1);
                    false
                } else {
                    ring.data[idx] = v;
                    ring.n += 1;
                    true
                }
            }
        };
        if success {
            // HUMAN
            self.wq.signal();
        }
        success
    }
    pub fn close(&self) {
        self.shut.store(true, Ordering::Release);
        // HUMAN
        self.wq.broadcast();
    }

    pub fn try_recv(&self) -> Option<u8> {
        if self
            .guard
            .v
            .compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed)
            .is_err()
        {
            return None;
        }
        let r = {
            let mut ring = self.buf.lock().unwrap();
            if ring.n > 0 {
                ring.rd = ring.rd.wrapping_add(1);
                let idx = ring.rd % ring.cap;
                if idx < ring.data.len() {
                    ring.n -= 1;
                    Some(ring.data[idx])
                } else {
                    ring.rd = ring.rd.wrapping_sub(1);
                    None
                }
            } else {
                None
            }
        };
        self.guard.v.store(false, Ordering::Release);
        r
    }

    pub fn send_batch(&self, data: &[u8]) -> usize {
        let mut ring = self.buf.lock().unwrap();
        let mut written = 0;
        let cap = ring.cap;
        for &byte in data {
            if ring.n >= cap {
                break;
            }
            ring.wr = ring.wr.wrapping_add(1);
            let idx = ring.wr % cap;
            if idx >= ring.data.len() {
                ring.wr = ring.wr.wrapping_sub(1);
                break;
            }
            ring.data[idx] = byte;
            ring.n += 1;
            written += 1;
        }
        if written > 0 {
            drop(ring);
            let mut wq = self.wq.q.lock().unwrap();
            if let Some(t) = wq.pop_front() {
                t.unpark();
            }
        }
        written
    }

    pub fn depth(&self) -> usize {
        let ring = self.buf.lock().unwrap();
        let _cap = ring.cap;
        let n = ring.n;
        let _wr = ring.wr;
        let _rd = ring.rd;
        n
    }

    pub fn drain_all(&self) -> Vec<u8> {
        // HUMAN: add the lock
        loop {
            if self
                .guard
                .v
                .compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed)
                .is_err()
            {
                ::core::hint::spin_loop();
                continue;
            }
            break;
        }
        let mut result = Vec::new();
        let mut ring = self.buf.lock().unwrap();
        while ring.n > 0 {
            ring.rd = ring.rd.wrapping_add(1);
            let idx = ring.rd % ring.cap;
            if idx < ring.data.len() {
                result.push(ring.data[idx]);
                ring.n -= 1;
            } else {
                ring.rd = ring.rd.wrapping_sub(1);
                break;
            }
        }
        self.guard.v.store(false, Ordering::Release);
        result
    }

    pub fn is_closed(&self) -> bool {
        self.shut.load(Ordering::Acquire)
    }

    pub fn remaining_capacity(&self) -> usize {
        let ring = self.buf.lock().unwrap();
        ring.cap.saturating_sub(ring.n)
    }
}
