// AGENT: isolate physical-frame ownership from direct-map conversion and VMA
// metadata management.
use alloc::sync::Arc;
use core::sync::atomic::{AtomicBool, Ordering};

use super::{copy_page, FramePool, FramePoolState, MmapFileSource, Mutex, PAGE_SZ};

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

// AGENT: keep sticky dirty state shared by fork-related MAP_SHARED wrappers so
// any later unmap writes the latest bytes even after another alias wrote them.
pub(crate) struct SharedFilePageState {
    pub(super) source: MmapFileSource,
    pub(super) offset: usize,
    pub(super) valid_len: usize,
    dirty: AtomicBool,
}

// AGENT: centralize shared-file dirty publication and observation without
// coupling backing state to one address space's mapping-local PTE permissions.
impl SharedFilePageState {
    // AGENT: initialize one clean file-page state with its positioned EOF span.
    pub(super) fn new(source: MmapFileSource, offset: usize, valid_len: usize) -> Self {
        Self {
            source,
            offset,
            valid_len,
            dirty: AtomicBool::new(false),
        }
    }

    // AGENT: publish dirtiness monotonically so fork aliases cannot clear it.
    pub(super) fn mark_dirty(&self) {
        self.dirty.store(true, Ordering::Release);
    }

    // AGENT: acquire the sticky dirty publication before unmap copies bytes.
    pub(super) fn is_dirty(&self) -> bool {
        self.dirty.load(Ordering::Acquire)
    }
}

// AGENT: distinguish anonymous, private-file, and shared-file ownership at the
// resident-page boundary so fork, write fault, and unmap use one backing truth.
#[derive(Clone)]
pub(crate) enum PageBacking {
    Anonymous,
    FilePrivate {
        source: MmapFileSource,
        offset: usize,
        valid_len: usize,
    },
    FileShared(Arc<SharedFilePageState>),
}

// AGENT: SharedPage is one by-value page handle: clones share the PgFrame and
// file dirty state while keeping COW/write-enable state local to one mapping.
#[derive(Clone)]
pub struct SharedPage {
    pub(super) frame: PgFrame,
    pub(super) cow: bool,
    pub(super) backing: PageBacking,
    pub(super) shared_write_enabled: bool,
}

// AGENT: keep shared-frame identity and mapping-local COW transitions together.
impl SharedPage {
    // AGENT: construct an anonymous resident with ordinary non-COW ownership.
    pub fn new(frame: PgFrame) -> Self {
        Self {
            frame,
            cow: false,
            backing: PageBacking::Anonymous,
            shared_write_enabled: false,
        }
    }

    // AGENT: retain positioned file origin for a private resident page without
    // granting it any shared writeback behavior.
    pub(super) fn new_file_private(
        frame: PgFrame,
        source: MmapFileSource,
        offset: usize,
        valid_len: usize,
    ) -> Self {
        Self {
            frame,
            cow: false,
            backing: PageBacking::FilePrivate {
                source,
                offset,
                valid_len,
            },
            shared_write_enabled: false,
        }
    }

    // AGENT: attach one sticky shared-file state while leaving every new
    // writable mapping write-protected until its first observed store.
    pub(super) fn new_file_shared(
        frame: PgFrame,
        source: MmapFileSource,
        offset: usize,
        valid_len: usize,
    ) -> Self {
        Self {
            frame,
            cow: false,
            backing: PageBacking::FileShared(Arc::new(SharedFilePageState::new(
                source, offset, valid_len,
            ))),
            shared_write_enabled: false,
        }
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

    // AGENT: classify the write-protected shared-file state separately from
    // COW so the common store-fault handler can choose the correct transition.
    pub(super) fn needs_shared_write_fault(&self) -> bool {
        matches!(self.backing, PageBacking::FileShared(_)) && !self.shared_write_enabled
    }

    // AGENT: publish sticky file dirtiness before enabling this mapping's PTE
    // write bit, ensuring unmap cannot miss a completed user or usercopy write.
    pub(super) fn enable_shared_write(&mut self) -> Result<(), &'static str> {
        let PageBacking::FileShared(state) = &self.backing else {
            return Err("segfault");
        };
        state.mark_dirty();
        self.shared_write_enabled = true;
        Ok(())
    }

    // AGENT: stage a writable COW replacement while preserving its file backing
    // and mapping-local shared state until AddrSpace commits the Sv39 update.
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
        Ok(Self {
            frame,
            cow: false,
            backing: self.backing.clone(),
            shared_write_enabled: self.shared_write_enabled,
        })
    }
}
