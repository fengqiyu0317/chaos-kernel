// AGENT: keep this module focused on physical-frame pool allocation.
use alloc::{boxed::Box, sync::Arc, vec, vec::Vec};

use super::{Mutex, PgFrame, PAGE_SZ};

const FRAME_WORD_BITS: usize = usize::BITS as usize;

// AGENT: keep physical-frame occupancy in one preallocated bitmap so claiming
// or releasing frames never allocates through the kernel heap it supplies.
pub(crate) struct FramePoolState {
    cap: usize,
    used_bits: Box<[usize]>,
    free_pages: usize,
}

// AGENT: provide allocation-free physical-frame state transitions independent
// of the BTreeSet-backed upper-layer AllocatorState.
impl FramePoolState {
    // AGENT: allocate the complete bitmap during boot before FramePool backs the
    // dynamic global heap; the bitmap never grows after this construction.
    fn new(cap: usize) -> Self {
        let words = cap / FRAME_WORD_BITS + usize::from(cap % FRAME_WORD_BITS != 0);
        Self {
            cap,
            used_bits: vec![0; words].into_boxed_slice(),
            free_pages: cap,
        }
    }

    // AGENT: claim the lowest free physical frame without allocating metadata.
    fn claim_one(&mut self) -> Option<usize> {
        for word_index in 0..self.used_bits.len() {
            let available = !self.used_bits[word_index];
            if available == 0 {
                continue;
            }
            let bit = available.trailing_zeros() as usize;
            let id = word_index.checked_mul(FRAME_WORD_BITS)?.checked_add(bit)?;
            if id >= self.cap {
                return None;
            }
            self.mark_used(id);
            return Some(id);
        }
        None
    }

    // AGENT: claim one caller-selected frame while rejecting occupied or
    // out-of-range ids.
    fn claim(&mut self, id: usize) -> Option<usize> {
        if !self.is_free(id) {
            return None;
        }
        self.mark_used(id);
        Some(id)
    }

    // AGENT: claim the first aligned free run needed by direct-mapped consumers
    // such as the dynamic heap and task kernel stacks.
    fn claim_contiguous(&mut self, count: usize, align: usize) -> Option<usize> {
        if count == 0 || align == 0 {
            return None;
        }
        for start in (0..self.cap).step_by(align) {
            let Some(end) = start.checked_add(count) else {
                break;
            };
            if end > self.cap {
                break;
            }
            if (start..end).all(|id| self.is_free(id)) {
                for id in start..end {
                    self.mark_used(id);
                }
                return Some(start);
            }
        }
        None
    }

    // AGENT: return one PgFrame-owned slot and reject duplicate or invalid
    // releases without allocating any bookkeeping node.
    pub(crate) fn release_one(&mut self, id: usize) -> bool {
        if !self.is_used(id) {
            return false;
        }
        self.mark_free(id);
        true
    }

    // AGENT: validate a whole contiguous run before clearing it so a failed
    // release cannot publish only a prefix of the pages.
    fn release_contiguous(&mut self, first: usize, count: usize) -> bool {
        let Some(end) = first.checked_add(count) else {
            return false;
        };
        if count == 0 || end > self.cap || (first..end).any(|id| !self.is_used(id)) {
            return false;
        }
        for id in first..end {
            self.mark_free(id);
        }
        true
    }

    // AGENT: expose physical-frame availability without leaking bitmap layout.
    fn is_free(&self, id: usize) -> bool {
        id < self.cap && !self.is_used(id)
    }

    // AGENT: report free physical pages from the counter updated with each bit.
    fn free_count(&self) -> usize {
        self.free_pages
    }

    // AGENT: fill caller-preallocated storage while holding the frame lock;
    // capacity is established before the lock so push cannot enter GlobalAlloc.
    fn claim_batch_into(&mut self, count: usize, ids: &mut Vec<usize>) {
        debug_assert!(ids.capacity() >= count);
        while ids.len() < count {
            let Some(id) = self.claim_one() else {
                break;
            };
            ids.push(id);
        }
    }

    // AGENT: test one preallocated occupancy bit.
    fn is_used(&self, id: usize) -> bool {
        if id >= self.cap {
            return false;
        }
        let word = id / FRAME_WORD_BITS;
        let bit = id % FRAME_WORD_BITS;
        self.used_bits[word] & (1usize << bit) != 0
    }

    // AGENT: publish one frame claim and keep the O(1) free-page count exact.
    fn mark_used(&mut self, id: usize) {
        debug_assert!(id < self.cap && !self.is_used(id));
        let word = id / FRAME_WORD_BITS;
        let bit = id % FRAME_WORD_BITS;
        self.used_bits[word] |= 1usize << bit;
        self.free_pages -= 1;
    }

    // AGENT: publish one frame release and keep the O(1) free-page count exact.
    fn mark_free(&mut self, id: usize) {
        debug_assert!(id < self.cap && self.is_used(id));
        let word = id / FRAME_WORD_BITS;
        let bit = id % FRAME_WORD_BITS;
        self.used_bits[word] &= !(1usize << bit);
        self.free_pages += 1;
    }
}

// AGENT: share dedicated physical-frame bitmap state with PgFrame so frame
// ownership never depends on the generic BTreeSet AllocatorState.
#[derive(Clone)]
pub struct FramePool {
    pub(crate) state: Arc<Mutex<FramePoolState>>,
    pub(crate) cap: usize,
    pub(crate) base_paddr: usize,
}

impl FramePool {
    // AGENT: put the complete RAM span under one allocator; boot-held frames,
    // page-backed heap spans, page tables, and user pages share this state.
    pub fn new(n: usize, base_paddr: usize) -> Self {
        Self {
            state: Arc::new(Mutex::new(FramePoolState::new(n))),
            cap: n,
            base_paddr,
        }
    }

    // AGENT: allocate one aligned contiguous run for a direct-mapped owner.
    pub fn alloc_contiguous_pages(&self, count: usize, align_pages: usize) -> Option<usize> {
        let first = self
            .state
            .lock()
            .unwrap()
            .claim_contiguous(count, align_pages)?;
        self.frame_id_to_paddr(first)
    }

    // AGENT: return one direct-mapped contiguous allocation to the shared pool.
    pub fn release_contiguous_pages(&self, paddr: usize, count: usize) -> bool {
        let Some(first) = self.paddr_to_frame_id(paddr) else {
            return false;
        };
        self.state.lock().unwrap().release_contiguous(first, count)
    }

    // AGENT: map a frame id back to the physical address owned by this pool.
    pub fn frame_id_to_paddr(&self, id: usize) -> Option<usize> {
        if id >= self.cap {
            return None;
        }
        id.checked_mul(PAGE_SZ)
            .and_then(|offset| self.base_paddr.checked_add(offset))
    }

    // AGENT: validate that a physical address names a page in this pool.
    pub fn paddr_to_frame_id(&self, paddr: usize) -> Option<usize> {
        if paddr < self.base_paddr || paddr % PAGE_SZ != 0 {
            return None;
        }
        let id = (paddr - self.base_paddr) / PAGE_SZ;
        if id < self.cap {
            Some(id)
        } else {
            None
        }
    }

    // AGENT: compute the exclusive physical end of the frame interval.
    pub fn limit_paddr(&self) -> usize {
        self.cap
            .checked_mul(PAGE_SZ)
            .and_then(|span| self.base_paddr.checked_add(span))
            .unwrap_or(usize::MAX)
    }

    // AGENT: expose allocator pressure without leaking raw frame ownership.
    pub fn free_count(&self) -> usize {
        self.state.lock().unwrap().free_count()
    }

    // AGENT: report the complete physical-frame span represented by this pool.
    pub fn total_pages(&self) -> usize {
        self.cap
    }

    // AGENT: allocate a physical frame as a RAII page-frame handle.
    pub fn alloc_pg_frame(&self) -> Option<PgFrame> {
        let id = self.state.lock().unwrap().claim_one()?;
        Some(self.pg_frame_from_allocated(id))
    }

    // AGENT: allocate a specific physical frame as a RAII page-frame handle.
    pub fn get_pg_frame(&self, id: usize) -> Option<PgFrame> {
        self.state.lock().unwrap().claim(id)?;
        Some(self.pg_frame_from_allocated(id))
    }

    // AGENT: return batch ownership only through PgFrame handles; a partial
    // allocation is dropped here so callers observe all-or-nothing semantics.
    pub fn alloc_pg_frames(&self, count: usize) -> Option<Vec<PgFrame>> {
        let mut ids = Vec::with_capacity(count);
        self.state.lock().unwrap().claim_batch_into(count, &mut ids);
        let frames: Vec<PgFrame> = ids
            .into_iter()
            .map(|id| self.pg_frame_from_allocated(id))
            .collect();
        if frames.len() == count {
            Some(frames)
        } else {
            None
        }
    }

    // AGENT: preserve the physical contiguity required by direct-mapped kernel
    // stacks while returning ordinary PgFrame owners instead of a raw run.
    pub fn alloc_contiguous_pg_frames(
        &self,
        count: usize,
        align_pages: usize,
    ) -> Option<Vec<PgFrame>> {
        let first = self
            .state
            .lock()
            .unwrap()
            .claim_contiguous(count, align_pages)?;
        let end = first
            .checked_add(count)
            .expect("claimed contiguous frame range should not overflow");
        Some(
            (first..end)
                .map(|id| self.pg_frame_from_allocated(id))
                .collect(),
        )
    }

    // AGENT: attach RAII ownership to a frame that is already marked allocated.
    fn pg_frame_from_allocated(&self, id: usize) -> PgFrame {
        PgFrame::from_allocated(id, self.state.clone(), self.base_paddr)
    }
}
