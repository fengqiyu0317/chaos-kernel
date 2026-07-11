// AGENT: isolate physical-frame ownership from direct-map conversion and VMA
// metadata management.
use alloc::sync::Arc;

use super::{copy_page, AllocatorState, FramePool, Mutex, PAGE_SZ};

// AGENT: PgFrame is the RAII mapping handle for a physical frame; cloning it
// represents another PTE sharing that frame.
#[derive(Clone)]
pub struct PgFrame {
    inner: Arc<PgFrameInner>,
}

// AGENT: return the frame to its pool when the final PgFrame mapping handle drops.
struct PgFrameInner {
    id: usize,
    allocator: Arc<Mutex<AllocatorState>>,
    base_paddr: usize,
}

// AGENT: keep physical-frame identity and shared ownership operations together.
impl PgFrame {
    // AGENT: construct a frame handle only for a slot already reserved by FramePool.
    pub(crate) fn from_allocated(
        id: usize,
        allocator: Arc<Mutex<AllocatorState>>,
        base_paddr: usize,
    ) -> Self {
        Self {
            inner: Arc::new(PgFrameInner {
                id,
                allocator,
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
        let released = self.allocator.lock().unwrap().release(self.id);
        assert!(
            released,
            "PgFrame {} was released twice or was never allocated",
            self.id
        );
    }
}

// AGENT: SharedPage is the resident physical page object shared by forked PTEs;
// keep COW frame splitting beside the PgFrame ownership it wraps.
#[derive(Clone)]
pub struct SharedPage {
    frame: PgFrame,
}

// AGENT: expose shared-frame identity and perform COW replacement as one
// ownership operation.
impl SharedPage {
    pub fn new(frame: PgFrame) -> Self {
        Self { frame }
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

    pub fn fault(&mut self, pool: &FramePool) -> Result<usize, &'static str> {
        if self.is_unique() {
            return Ok(self.paddr());
        }

        let old_paddr = self.paddr();
        let new_frame = pool.alloc_pg_frame().ok_or("oom")?;
        let new_paddr = new_frame.paddr();
        copy_page(new_paddr, old_paddr);
        self.frame = new_frame;
        Ok(new_paddr)
    }
}
