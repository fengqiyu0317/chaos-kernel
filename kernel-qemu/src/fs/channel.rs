// AGENT
use super::*;

// AGENT: keep Channel internals private so buffer, shutdown, and wakeup
// invariants are maintained only through the methods below.
pub struct Channel {
    buf: Mutex<CircBuf>,
    wq: ConditionWait,
    shut: AtomicBool,
}
impl Channel {
    // AGENT: Channel state is coordinated by CircBuf's Mutex plus ConditionWait.
    pub fn new(cap: usize) -> Self {
        let effective_cap = cap.clamp(1, 1 << 20);
        Self {
            buf: Mutex::new(CircBuf::new(effective_cap)),
            wq: ConditionWait::new(),
            shut: AtomicBool::new(false),
        }
    }
    // AGENT: wait_until() checks buffer/shutdown state and registers the waiter
    // under the same buffer lock, then sleeps after that lock is released.
    pub fn recv(&self, task_id: usize) -> Option<u8> {
        self.wq.wait_until(task_id, &self.buf, |ring| {
            if let Some(v) = ring.pop() {
                Some(Some(v))
            } else if self.shut.load(Ordering::Acquire) {
                Some(None)
            } else {
                None
            }
        })
    }
    // AGENT: reject writes after close and wake one receiver only after a byte
    // has been published under the buffer mutex.
    pub fn send(&self, v: u8) -> bool {
        let mut ring = self.buf.lock().unwrap();
        if self.shut.load(Ordering::Acquire) || !ring.push(v) {
            return false;
        }
        drop(ring);
        // HUMAN
        self.wq.signal();
        true
    }
    // AGENT: publish shutdown while holding buf so recv cannot miss the close
    // between checking the predicate and enqueueing its WaitToken.
    pub fn close(&self) {
        {
            let _ring = self.buf.lock().unwrap();
            self.shut.store(true, Ordering::Release);
        }
        // HUMAN
        self.wq.broadcast();
    }

    // AGENT: non-blocking receive reads only under the buffer mutex.
    pub fn try_recv(&self) -> Option<u8> {
        self.buf.lock().unwrap().pop()
    }

    // AGENT: batch send shares the same closed-state and wakeup rules as send().
    pub fn send_batch(&self, data: &[u8]) -> usize {
        let mut ring = self.buf.lock().unwrap();
        if self.shut.load(Ordering::Acquire) {
            return 0;
        }
        let written = ring.fill_from(data);
        if written > 0 {
            drop(ring);
            self.wq.signal_n(written);
        }
        written
    }

    // AGENT: depth is a pure buffer query over the protected ring buffer.
    pub fn depth(&self) -> usize {
        self.buf.lock().unwrap().len()
    }

    // AGENT: draining holds only the buffer mutex and never waits.
    pub fn drain_all(&self) -> Vec<u8> {
        let mut ring = self.buf.lock().unwrap();
        let len = ring.len();
        let mut result = Vec::with_capacity(len);
        ring.drain_to(&mut result, len);
        result
    }

    // AGENT: shutdown state is published with release and observed with acquire.
    pub fn is_closed(&self) -> bool {
        self.shut.load(Ordering::Acquire)
    }

    // AGENT: remaining capacity is a pure buffer query over the protected ring buffer.
    pub fn remaining_capacity(&self) -> usize {
        self.buf.lock().unwrap().remaining()
    }
}
