// AGENT
use super::*;

pub struct Channel {
    pub buf: Mutex<CircBuf>,
    pub guard: Spin,
    pub wq: ConditionWait,
    pub shut: AtomicBool,
}
impl Channel {
    // AGENT: Channel keeps the legacy Spin field for API compatibility, but
    // blocking send/recv coordination is handled by CircBuf's Mutex + ConditionWait.
    pub fn new(cap: usize) -> Self {
        let effective_cap = cap.clamp(1, 1 << 20);
        Self {
            buf: Mutex::new(CircBuf::new(effective_cap)),
            guard: Spin::new(),
            wq: ConditionWait::new(),
            shut: AtomicBool::new(false),
        }
    }
    // AGENT: wait_until() checks buffer/shutdown state and registers the waiter
    // under the same buffer lock, then sleeps after that lock is released.
    pub fn recv(&self) -> Option<u8> {
        self.wq.wait_until(&self.buf, |ring| {
            if let Some(v) = ring.pop() {
                Some(Some(v))
            } else if self.shut.load(Ordering::Acquire) {
                Some(None)
            } else {
                None
            }
        })
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
    // shut while holding buf or has already queued its WaitToken.
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
        self.buf.lock().unwrap().len()
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
        self.buf.lock().unwrap().remaining()
    }
}
