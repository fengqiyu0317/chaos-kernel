// AGENT: isolate resident page-table metadata and leaf-flag policy from
// address-space orchestration.
use alloc::collections::BTreeMap;
use core::mem;

use super::{
    FramePool, PgFrame, SharedPage, PTE_A, PTE_R, PTE_U, PTE_W, PTE_X, VM_EXEC, VM_READ, VM_SHARED,
    VM_WRITE,
};

// AGENT: resident metadata owns the mapped frame and software-only COW state;
// VmRegion owns permission policy and Sv39 owns the live leaf flags.
pub struct ResidentPage {
    pub(super) frame: SharedPage,
    pub(super) cow: bool,
}

// AGENT: keep resident frame-ownership and COW transitions beside their data.
impl ResidentPage {
    // AGENT: wrap a caller-initialized anonymous frame; the caller must zero a
    // newly allocated frame before exposing the mapping to user space.
    pub fn new(frame: PgFrame) -> Self {
        Self {
            frame: SharedPage::new(frame),
            cow: false,
        }
    }

    // AGENT: attach an explicitly shared physical page without enabling COW,
    // and catch accidental use for a private mapping during development.
    pub(super) fn from_shared(frame: SharedPage, flags: u32) -> Self {
        debug_assert!(flags & VM_SHARED != 0);
        Self { frame, cow: false }
    }

    // AGENT: mark a private resident page as software COW after the Sv39 leaf
    // has already been made read-only by AddrSpace.
    pub(super) fn as_cow(&mut self) {
        self.cow = true;
    }

    // AGENT: stage replacement frame ownership without changing the live
    // resident entry, so a failed Sv39 update leaves the old state intact.
    pub(super) fn prepare_resolved_write(&self, pool: &FramePool) -> Result<Self, &'static str> {
        debug_assert!(self.cow);
        Ok(Self {
            frame: self.frame.prepare_cow_copy(pool)?,
            cow: false,
        })
    }

    // AGENT: expose the physical-frame identity while keeping ownership in the
    // resident page-table entry.
    pub fn frame_id(&self) -> usize {
        self.frame.frame_id()
    }

    // AGENT: clone resident ownership and software COW state for a child
    // mapping; the live Sv39 flags are copied separately by AddrSpace.
    pub(super) fn clone_mapping(&self) -> Self {
        Self {
            frame: self.frame.clone(),
            cow: self.cow,
        }
    }
}

// AGENT: store software resident-page metadata separately from the real Sv39
// page table so the BTreeMap is not mistaken for hardware page-table storage.
pub(super) struct ResidentPages {
    pub(super) entries: BTreeMap<usize, ResidentPage>,
}

// AGENT: own resident page-table initialization and bulk-detach operations.
impl ResidentPages {
    // AGENT: initialize the software resident-page table independently of VmMap
    // and Sv39 root allocation.
    pub(super) fn new() -> Self {
        Self {
            entries: BTreeMap::new(),
        }
    }

    // AGENT: detach all resident metadata only through an exclusive AddrSpace
    // borrow, keeping software metadata and Sv39 updates in one lock domain.
    pub(super) fn take_all(&mut self) -> BTreeMap<usize, ResidentPage> {
        mem::take(&mut self.entries)
    }
}

// AGENT: translate migrated VM flags into legal Sv39 leaf permissions while
// keeping PROT_NONE pages non-user and VM_WRITE leaves hardware-legal.
pub(super) fn vm_flags_to_pte_flags(flags: u32) -> usize {
    let can_read = flags & VM_READ != 0;
    let can_write = flags & VM_WRITE != 0;
    let can_exec = flags & VM_EXEC != 0;

    if !can_read && !can_write && !can_exec {
        return PTE_R;
    }

    let mut pte_flags = PTE_A | PTE_U;
    if can_read {
        pte_flags |= PTE_R;
    }
    if can_write {
        pte_flags |= PTE_R | PTE_W;
    }
    if can_exec {
        pte_flags |= PTE_X;
    }
    pte_flags
}

// AGENT: strip the write bit when software COW owns the next write fault.
pub(super) fn pte_flags_without_write(flags: usize) -> usize {
    flags & !PTE_W
}
