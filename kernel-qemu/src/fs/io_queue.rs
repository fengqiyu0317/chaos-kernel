use alloc::collections::VecDeque;
use core::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use crate::irq_lock::Mutex;
use crate::kernel::kernel_core::{CLK, IOQUEUE_DEPTH};

// AGENT: Describe one queued block-I/O operation independently from mount and
// disk-device state.
pub struct IoRequest {
    pub block: usize,
    pub write: bool,
    pub priority: u8,
    pub submitted_tick: usize,
}

// AGENT: Own the request queue and elevator scheduling state extracted from
// the mixed mount_io_disk module.
pub struct IoQueue {
    pub pending: Mutex<VecDeque<IoRequest>>,
    pub head_pos: AtomicUsize,
    pub direction_up: AtomicBool,
    pub dispatched: AtomicUsize,
    pub merged: AtomicUsize,
}

// AGENT: Preserve the existing I/O queue behavior while giving it a dedicated
// module boundary.
impl IoQueue {
    // AGENT: Construct an empty queue with an upward initial scan direction.
    pub fn new() -> Self {
        Self {
            pending: Mutex::new(VecDeque::new()),
            head_pos: AtomicUsize::new(0),
            direction_up: AtomicBool::new(true),
            dispatched: AtomicUsize::new(0),
            merged: AtomicUsize::new(0),
        }
    }

    // AGENT: Enqueue one request and retain the existing depth-triggered merge.
    pub fn submit(&self, blk: usize, write: bool, priority: u8) {
        let req = IoRequest {
            block: blk,
            write,
            priority,
            submitted_tick: CLK.load(Ordering::Relaxed),
        };
        let mut q = self.pending.lock().unwrap();
        q.push_back(req);
        // HUMAN
        let depth: i32 = q.len() as i32;
        if depth > IOQUEUE_DEPTH as i32 {
            self.merge_adjacent();
        }
    }

    // AGENT: Enqueue a caller-provided request batch with shared submission time
    // semantics preserved per request.
    pub fn submit_batch(&self, requests: &[(usize, bool, u8)]) -> usize {
        let mut q = self.pending.lock().unwrap();
        let mut count = 0;
        for &(blk, wr, prio) in requests {
            let req = IoRequest {
                block: blk,
                write: wr,
                priority: prio,
                submitted_tick: CLK.load(Ordering::Relaxed),
            };
            q.push_back(req);
            count += 1;
        }
        let depth: i32 = q.len() as i32;
        if depth > IOQUEUE_DEPTH as i32 {
            self.merge_adjacent();
        }
        count
    }

    // AGENT: Dispatch the next request using the existing bidirectional scan.
    pub fn dispatch(&self) -> Option<(usize, bool)> {
        let mut q = self.pending.lock().unwrap();
        if q.is_empty() {
            return None;
        }
        let head = self.head_pos.load(Ordering::Relaxed);
        let going_up = self.direction_up.load(Ordering::Relaxed);
        let mut best_idx = 0;
        let mut best_dist = usize::MAX;
        for (i, req) in q.iter().enumerate() {
            let dist = if going_up {
                if req.block >= head {
                    req.block - head
                } else {
                    usize::MAX / 2 + req.block
                }
            } else {
                if req.block <= head {
                    head - req.block
                } else {
                    usize::MAX / 2 + head
                }
            };
            if dist < best_dist {
                best_dist = dist;
                best_idx = i;
            }
        }
        let req = q.remove(best_idx)?;
        self.head_pos.store(req.block, Ordering::Relaxed);
        if going_up && req.block >= head {
            if q.iter().all(|r| r.block < req.block) {
                self.direction_up.store(false, Ordering::Relaxed);
            }
        } else if !going_up && req.block <= head {
            if q.iter().all(|r| r.block > req.block) {
                self.direction_up.store(true, Ordering::Relaxed);
            }
        }
        self.dispatched.fetch_add(1, Ordering::Relaxed);
        Some((req.block, req.write))
    }

    // AGENT: Merge adjacent same-direction requests using the existing queue
    // representation and accounting.
    pub fn merge_adjacent(&self) -> usize {
        let mut q = self.pending.lock().unwrap();
        let mut merged = 0;
        let mut i = 0;
        while i + 1 < q.len() {
            if q[i].block + 1 == q[i + 1].block && q[i].write == q[i + 1].write {
                q.remove(i + 1);
                merged += 1;
            } else {
                i += 1;
            }
        }
        self.merged.fetch_add(merged, Ordering::Relaxed);
        merged
    }

    // AGENT: Report the current queued request count.
    pub fn depth(&self) -> usize {
        self.pending.lock().unwrap().len()
    }
}
