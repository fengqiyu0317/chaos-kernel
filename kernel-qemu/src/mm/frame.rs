// AGENT: isolate physical-frame ownership from direct-map conversion and VMA
// metadata management.
use alloc::{sync::Arc, vec::Vec};

use super::{Mutex, PAGE_SZ};

// AGENT: PgFrame is the RAII mapping handle for a physical frame; cloning it
// represents another PTE sharing that frame.
#[derive(Clone)]
pub struct PgFrame {
    inner: Arc<PgFrameInner>,
}

// AGENT: return the frame to its pool when the final PgFrame mapping handle drops.
struct PgFrameInner {
    id: usize,
    slots: Arc<Mutex<Vec<bool>>>,
    base_paddr: usize,
}

// AGENT: keep physical-frame identity and shared ownership operations together.
impl PgFrame {
    // AGENT: construct a frame handle only for a slot already reserved by FramePool.
    pub(crate) fn from_allocated(
        id: usize,
        slots: Arc<Mutex<Vec<bool>>>,
        base_paddr: usize,
    ) -> Self {
        Self {
            inner: Arc::new(PgFrameInner {
                id,
                slots,
                base_paddr,
            }),
        }
    }

    // AGENT: expose the pool-relative frame identifier without leaking slot storage.
    pub fn id(&self) -> usize {
        self.inner.id
    }

    // AGENT: derive the physical address from the owning pool's base address.
    pub fn paddr(&self) -> usize {
        self.inner
            .id
            .checked_mul(PAGE_SZ)
            .and_then(|offset| self.inner.base_paddr.checked_add(offset))
            .unwrap_or(usize::MAX)
    }

    // AGENT: report how many mapping handles currently share this frame.
    pub fn count(&self) -> usize {
        Arc::strong_count(&self.inner)
    }

    // AGENT: expose the ownership check used by COW frame replacement.
    pub fn is_unique(&self) -> bool {
        self.count() == 1
    }
}

// AGENT: implement final-owner reclamation at the frame ownership boundary.
impl Drop for PgFrameInner {
    fn drop(&mut self) {
        let mut slots = self.slots.lock().unwrap();
        if self.id < slots.len() && !slots[self.id] {
            slots[self.id] = true;
        }
    }
}
