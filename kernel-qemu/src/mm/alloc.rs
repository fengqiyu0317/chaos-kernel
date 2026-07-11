// AGENT: keep this module focused on physical-frame pool allocation.
use alloc::{sync::Arc, vec::Vec};
use core::cmp::{max, min};

use super::{AllocatorState, Mutex, PgFrame, PAGE_SZ};

// AGENT: share the complete bounded frame allocator state with PgFrame so
// final-owner drops can return ids without parallel limit bookkeeping.
pub struct FramePool {
    pub(crate) allocator: Arc<Mutex<AllocatorState>>,
    pub(crate) cap: usize,
    pub(crate) base_paddr: usize,
}
impl FramePool {
    // AGENT: create a QEMU frame pool with no pages free until the boot path
    // marks linker/RAM-derived ranges usable.
    pub fn new(n: usize, base_paddr: usize) -> Self {
        Self {
            allocator: Arc::new(Mutex::new(AllocatorState::new(0, 0))),
            cap: n,
            base_paddr,
        }
    }

    // AGENT: install the contiguous boot-discovered range as the allocator's
    // bump interval instead of materializing one bitmap entry per frame.
    pub fn mark_free_range(&self, start_paddr: usize, end_paddr: usize) {
        let Some(start) = align_up_page(start_paddr) else {
            return;
        };
        let start = max(start, self.base_paddr);
        let end = min(align_down_page(end_paddr), self.limit_paddr());
        if end <= start {
            return;
        }

        let first = (start - self.base_paddr) / PAGE_SZ;
        let last = min((end - self.base_paddr) / PAGE_SZ, self.cap);
        self.allocator.lock().unwrap().reset(first, last);
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

    // AGENT: allocate the requested frame id instead of ignoring the argument.
    pub fn get(&self, id: usize) -> Option<usize> {
        self.allocator.lock().unwrap().allocate_id(id)
    }
    // AGENT: share the single-frame allocation path with the batch scanner.
    pub fn get_inner(&self) -> Option<usize> {
        self.batch_alloc(1).pop()
    }
    // AGENT: scan only physically aligned candidate starts and reject
    // impossible alignment shifts before they can overflow.
    pub fn get_contig(&self, sz: usize, align_log2: usize) -> Option<usize> {
        if sz == 0 || align_log2 >= usize::BITS as usize {
            return None;
        }
        let align_pages = 1usize << align_log2;
        let align_bytes = align_pages.checked_mul(PAGE_SZ)?;
        let first = self.first_aligned_frame_id(align_bytes)?;
        self.allocator
            .lock()
            .unwrap()
            .allocate_contiguous(first, sz, align_pages)
    }
    // AGENT: return a caller-owned frame id and surface duplicate or invalid
    // releases as allocator invariant violations.
    pub fn put(&self, idx: usize) {
        let released = self.allocator.lock().unwrap().release(idx);
        assert!(
            released,
            "frame {} was released twice or was never allocated",
            idx
        );
    }
    pub fn avail(&self, idx: usize) -> bool {
        self.allocator.lock().unwrap().is_free(idx)
    }
    pub fn free_count(&self) -> usize {
        self.allocator.lock().unwrap().free_count()
    }

    // AGENT: report the complete physical-frame span represented by this pool.
    pub fn total_pages(&self) -> usize {
        self.cap
    }

    // AGENT: allocate a physical frame as a RAII page-frame handle.
    pub fn alloc_pg_frame(&self) -> Option<PgFrame> {
        let id = self.get_inner()?;
        Some(self.pg_frame_from_allocated(id))
    }

    // AGENT: allocate a specific physical frame as a RAII page-frame handle.
    pub fn get_pg_frame(&self, id: usize) -> Option<PgFrame> {
        self.get(id)?;
        Some(self.pg_frame_from_allocated(id))
    }

    // AGENT: attach RAII ownership to a frame that is already marked allocated.
    fn pg_frame_from_allocated(&self, id: usize) -> PgFrame {
        PgFrame::from_allocated(id, self.allocator.clone(), self.base_paddr)
    }

    // AGENT: find the first frame id whose physical address satisfies an
    // alignment in bytes; callers can then advance by the equivalent page span.
    fn first_aligned_frame_id(&self, align_bytes: usize) -> Option<usize> {
        if align_bytes == 0 || !align_bytes.is_power_of_two() || self.base_paddr % PAGE_SZ != 0 {
            return None;
        }
        let offset = self.base_paddr & (align_bytes - 1);
        if offset == 0 {
            Some(0)
        } else {
            Some((align_bytes - offset) / PAGE_SZ)
        }
    }

    pub fn batch_alloc(&self, count: usize) -> Vec<usize> {
        self.allocator.lock().unwrap().allocate_batch(count)
    }
}

// AGENT: align physical range starts without wrapping on overflow.
fn align_up_page(addr: usize) -> Option<usize> {
    addr.checked_add(PAGE_SZ - 1)
        .map(|value| value & !(PAGE_SZ - 1))
}

// AGENT: align physical range ends down to a page boundary.
fn align_down_page(addr: usize) -> usize {
    addr & !(PAGE_SZ - 1)
}
