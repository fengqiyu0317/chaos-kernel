// AGENT
use super::*;

// AGENT: keep ring-buffer cursors private so rd/wr/n invariants stay local.
pub struct CircBuf {
    data: Vec<u8>,
    rd: usize,
    wr: usize,
    cap: usize,
    n: usize,
}

// AGENT: rd is the next byte to read, wr is the next slot to write.
impl CircBuf {
    // AGENT: initialize an empty ring without exposing cursor details.
    pub fn new(c: usize) -> Self {
        Self {
            data: vec![0u8; c],
            rd: 0,
            wr: 0,
            cap: c,
            n: 0,
        }
    }

    // AGENT: write at wr before advancing so slot 0 is usable and semantics are FIFO.
    pub fn push(&mut self, v: u8) -> bool {
        if self.full() {
            return false;
        }
        self.data[self.wr] = v;
        self.wr = (self.wr + 1) % self.cap;
        self.n += 1;
        true
    }

    // AGENT: read from rd before advancing to mirror push's cursor semantics.
    pub fn pop(&mut self) -> Option<u8> {
        if self.empty() {
            return None;
        }
        let v = self.data[self.rd];
        self.rd = (self.rd + 1) % self.cap;
        self.n -= 1;
        Some(v)
    }

    // AGENT: expose the buffered byte count without exposing raw cursors.
    pub fn len(&self) -> usize {
        self.n
    }

    // AGENT: keep the legacy empty() API while routing through the invariant field.
    pub fn empty(&self) -> bool {
        self.n == 0
    }

    // AGENT: full rings reject writes before any modulo arithmetic.
    pub fn full(&self) -> bool {
        self.n >= self.cap
    }

    // AGENT: report the actual number moved instead of assuming all pops succeed.
    pub fn drain_to(&mut self, dst: &mut Vec<u8>, max: usize) -> usize {
        let mut moved = 0;
        while moved < max {
            let Some(b) = self.pop() else {
                break;
            };
            dst.push(b);
            moved += 1;
        }
        moved
    }

    // AGENT: copy a FIFO prefix without consuming it so splice can commit a
    // pipe read only after the destination write has succeeded.
    pub fn peek_to(&self, dst: &mut Vec<u8>, max: usize) -> usize {
        let moved = min(max, self.n);
        dst.reserve(moved);
        for offset in 0..moved {
            dst.push(self.data[(self.rd + offset) % self.cap]);
        }
        moved
    }

    // AGENT: consume an already-committed FIFO prefix without exposing ring
    // cursors to pipe splice callers.
    pub fn discard(&mut self, count: usize) -> usize {
        let discarded = min(count, self.n);
        if self.cap != 0 {
            self.rd = (self.rd + discarded) % self.cap;
        }
        self.n -= discarded;
        discarded
    }

    // AGENT: fill through push so capacity handling stays in one place.
    pub fn fill_from(&mut self, src: &[u8]) -> usize {
        let mut written = 0;
        for &b in src {
            if !self.push(b) {
                break;
            }
            written += 1;
        }
        written
    }

    // AGENT: remaining capacity is exact because n is kept within cap.
    pub fn remaining(&self) -> usize {
        self.cap - self.n
    }
}
