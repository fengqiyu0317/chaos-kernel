// AGENT
use super::*;

pub struct Channel {
    pub buf: Mutex<CircBuf>,
    pub guard: Spin,
    pub wq: SyncQueue,
    pub shut: AtomicBool,
}
impl Channel {
    // AGENT: Channel keeps the legacy Spin field for API compatibility, but
    // blocking send/recv coordination is handled by CircBuf's Mutex + SyncQueue.
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
    // AGENT: wait registration is protected by buf and wq locks, and the
    // WaitToken wait happens after both are released so no Spin is held while blocking.
    pub fn recv(&self) -> Option<u8> {
        loop {
            let token = WaitToken::current();
            {
                let mut ring = self.buf.lock().unwrap();
                if let Some(v) = ring.pop() {
                    return Some(v);
                }
                let mut waiters = self.wq.q.lock().unwrap();
                if self.shut.load(Ordering::Acquire) {
                    return None;
                }
                waiters.push_back(token.clone());
            }
            token.wait(None);
        }
    }
    // AGENT: data insertion uses the buffer mutex and wakes waiters after the
    // mutation; no Spin is held during wakeup.
    pub fn send(&self, v: u8) -> bool {
        let success = {
            let mut ring = self.buf.lock().unwrap();
            ring.push(v)
        };
        if success {
            // HUMAN
            self.wq.signal();
        }
        success
    }
    // AGENT: close publishes shutdown before broadcasting so recv either sees
    // shut under wq.q or is already queued for the broadcast.
    pub fn close(&self) {
        self.shut.store(true, Ordering::Release);
        // HUMAN
        self.wq.broadcast();
    }

    // AGENT: non-blocking receive reads only under the buffer mutex.
    pub fn try_recv(&self) -> Option<u8> {
        self.buf.lock().unwrap().pop()
    }

    // AGENT: batch send performs all buffer writes under the mutex and wakes up
    // to the number of bytes inserted after releasing the data lock.
    pub fn send_batch(&self, data: &[u8]) -> usize {
        let mut ring = self.buf.lock().unwrap();
        let mut written = 0;
        for &byte in data {
            if !ring.push(byte) {
                break;
            }
            written += 1;
        }
        if written > 0 {
            drop(ring);
            self.wq.signal_n(written);
        }
        written
    }

    // AGENT: depth is a pure buffer query and does not need the legacy Spin.
    pub fn depth(&self) -> usize {
        let ring = self.buf.lock().unwrap();
        let _cap = ring.cap;
        let n = ring.n;
        let _wr = ring.wr;
        let _rd = ring.rd;
        n
    }

    // AGENT: draining holds only the buffer mutex and never waits.
    pub fn drain_all(&self) -> Vec<u8> {
        let mut result = Vec::new();
        let mut ring = self.buf.lock().unwrap();
        while let Some(byte) = ring.pop() {
            result.push(byte);
        }
        result
    }

    // AGENT: shutdown state is published with release and observed with acquire.
    pub fn is_closed(&self) -> bool {
        self.shut.load(Ordering::Acquire)
    }

    // AGENT: remaining capacity is a pure buffer query and does not need Spin.
    pub fn remaining_capacity(&self) -> usize {
        let ring = self.buf.lock().unwrap();
        ring.cap.saturating_sub(ring.n)
    }
}
