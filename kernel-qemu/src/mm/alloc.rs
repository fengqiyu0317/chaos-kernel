// AGENT: keep this module focused on physical-frame pool allocation.
use alloc::{sync::Arc, vec, vec::Vec};
use core::cmp::{max, min};

use super::{Mutex, PgFrame, PAGE_SZ};

// AGENT: track the availability of every physical frame managed by the kernel;
// PgFrame shares this bitmap so RAII drops can return allocated pages.
pub struct FramePool {
    pub(crate) slots: Arc<Mutex<Vec<bool>>>,
    pub(crate) cap: usize,
    pub(crate) base_paddr: usize,
}
impl FramePool {
    // AGENT: create a QEMU frame pool with no pages free until the boot path
    // marks linker/RAM-derived ranges usable.
    pub fn new(n: usize, base_paddr: usize) -> Self {
        Self {
            slots: Arc::new(Mutex::new(vec![false; n])),
            cap: n,
            base_paddr,
        }
    }

    // AGENT: expose boot-time range seeding so the pool never assumes that the
    // whole QEMU RAM interval is allocatable.
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
        let mut slots = self.slots.lock().unwrap();
        for idx in first..last {
            slots[idx] = true;
        }
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
        let mut s = self.slots.lock().unwrap();
        if id < s.len() && s[id] {
            s[id] = false;
            Some(id)
        } else {
            None
        }
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
        let mut s = self.slots.lock().unwrap();
        for start in (first..s.len()).step_by(align_pages) {
            let Some(end) = start.checked_add(sz) else {
                break;
            };
            if end > s.len() {
                break;
            }
            if (start..end).all(|i| s[i]) {
                for i in start..end {
                    s[i] = false;
                }
                return Some(start);
            }
        }
        None
    }
    // AGENT: return a caller-owned frame id to the availability bitmap and
    // ignore duplicate/out-of-range releases.
    pub fn put(&self, idx: usize) {
        let mut s = self.slots.lock().unwrap();
        if idx < s.len() && !s[idx] {
            s[idx] = true;
        }
    }
    pub fn avail(&self, idx: usize) -> bool {
        let s = self.slots.lock().unwrap();
        idx < s.len() && s[idx]
    }
    pub fn free_count(&self) -> usize {
        self.slots.lock().unwrap().iter().filter(|&&f| f).count()
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
        PgFrame::from_allocated(id, self.slots.clone(), self.base_paddr)
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
        let mut s = self.slots.lock().unwrap();
        let mut result = Vec::with_capacity(count);
        for (i, f) in s.iter_mut().enumerate() {
            if result.len() >= count {
                break;
            }
            if *f {
                *f = false;
                result.push(i);
            }
        }
        result
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
