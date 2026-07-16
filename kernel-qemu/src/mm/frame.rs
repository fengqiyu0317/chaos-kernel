// AGENT: isolate physical-frame ownership from direct-map conversion and VMA
// metadata management.
use alloc::sync::Arc;

use super::{copy_page, FramePool, FramePoolState, Mutex, PAGE_SZ};

// AGENT: PgFrame is the RAII mapping handle for a physical frame; cloning it
// represents another PTE sharing that frame.
#[derive(Clone)]
pub struct PgFrame {
    inner: Arc<PgFrameInner>,
}

// AGENT: return the frame to the dedicated physical-frame bitmap when the final
// PgFrame mapping handle drops.
struct PgFrameInner {
    id: usize,
    state: Arc<Mutex<FramePoolState>>,
    base_paddr: usize,
}

// AGENT: keep physical-frame identity and shared ownership operations together.
impl PgFrame {
    // AGENT: construct a frame handle only for a slot already reserved by FramePool.
    pub(crate) fn from_allocated(
        id: usize,
        state: Arc<Mutex<FramePoolState>>,
        base_paddr: usize,
    ) -> Self {
        Self {
            inner: Arc::new(PgFrameInner {
                id,
                state,
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

// AGENT: reclaim the final owner's frame and surface allocator invariant
// violations instead of silently accepting invalid or duplicate releases.
impl Drop for PgFrameInner {
    fn drop(&mut self) {
        let released = self.state.lock().unwrap().release_one(self.id);
        assert!(
            released,
            "PgFrame {} was released twice or was never allocated",
            self.id
        );
    }
}

// AGENT: SharedPage is one by-value page handle: clones share the PgFrame while
// each wrapper keeps independent mapping-local COW state.
#[derive(Clone)]
pub struct SharedPage {
    pub(super) frame: PgFrame,
    pub(super) cow: bool,
}

// AGENT: keep shared-frame identity and mapping-local COW transitions together.
impl SharedPage {
    pub fn new(frame: PgFrame) -> Self {
        Self { frame, cow: false }
    }

    pub fn frame_id(&self) -> usize {
        self.frame.id()
    }

    pub fn paddr(&self) -> usize {
        self.frame.paddr()
    }

    pub fn is_unique(&self) -> bool {
        self.frame.is_unique()
    }

    pub fn sharers(&self) -> usize {
        self.frame.count()
    }

    // AGENT: mark this mapping handle as COW without changing sibling wrappers
    // that share the same PgFrame.
    pub(super) fn as_cow(&mut self) {
        self.cow = true;
    }

    // AGENT: stage a writable replacement wrapper without changing the live
    // mapping until AddrSpace has committed the corresponding Sv39 update.
    pub(super) fn prepare_resolved_write(&self, pool: &FramePool) -> Result<Self, &'static str> {
        debug_assert!(self.cow);
        let frame = if self.frame.is_unique() {
            self.frame.clone()
        } else {
            let old_paddr = self.frame.paddr();
            let new_frame = pool.alloc_pg_frame().ok_or("oom")?;
            copy_page(new_frame.paddr(), old_paddr);
            new_frame
        };
        Ok(Self { frame, cow: false })
    }
}
