// AGENT: isolate resident page-table metadata and leaf-flag policy from
// address-space orchestration.
use alloc::collections::BTreeMap;
use core::mem;

use super::{
    FramePool, PgFrame, SharedPage, PTE_A, PTE_D, PTE_R, PTE_U, PTE_W, PTE_X, VM_EXEC, VM_READ,
    VM_SHARED, VM_WRITE,
};

// AGENT: QEMU PTE metadata keeps current hardware leaf state while VmRegion
// remains the single source of VM flags.
pub struct PageTableEntry {
    pub(super) frame: SharedPage,
    pub(super) pte_flags: usize,
    pub(super) cow: bool,
}

// AGENT: keep resident leaf-state transitions beside the metadata they mutate.
impl PageTableEntry {
    // AGENT: wrap a caller-initialized anonymous frame; the caller must zero a
    // newly allocated frame before exposing the mapping to user space.
    pub fn new(frame: PgFrame, flags: u32) -> Self {
        Self {
            frame: SharedPage::new(frame),
            pte_flags: vm_flags_to_pte_flags(flags),
            cow: false,
        }
    }

    // AGENT: attach an explicitly shared physical page without enabling COW,
    // and catch accidental use for a private mapping during development.
    pub(super) fn from_shared(frame: SharedPage, flags: u32) -> Self {
        debug_assert!(flags & VM_SHARED != 0);
        Self {
            frame,
            pte_flags: vm_flags_to_pte_flags(flags),
            cow: false,
        }
    }

    // AGENT: mark a writable private mapping as software COW and mirror the
    // transition in its hardware-facing flags.
    pub(super) fn as_cow(&mut self) {
        self.cow = true;
        self.pte_flags = pte_flags_without_write(self.pte_flags);
    }

    // AGENT: stage COW frame ownership and writable flags without changing the
    // live resident entry, so a failed Sv39 update leaves the old state intact.
    pub(super) fn prepare_resolved_write(
        &self,
        flags: u32,
        pool: &FramePool,
    ) -> Result<Self, &'static str> {
        debug_assert!(self.cow);
        debug_assert!(flags & VM_WRITE != 0);
        Ok(Self {
            frame: self.frame.prepare_cow_copy(pool)?,
            pte_flags: vm_flags_to_pte_flags(flags),
            cow: false,
        })
    }

    // AGENT: require both the Sv39 write bit and the absence of a pending
    // software COW fault so an inconsistent entry fails closed.
    pub(super) fn is_writable(&self) -> bool {
        !self.cow && self.pte_flags & PTE_W != 0
    }

    // AGENT: expose the physical-frame identity while keeping ownership in the
    // resident page-table entry.
    pub fn frame_id(&self) -> usize {
        self.frame.frame_id()
    }

    // AGENT: clone only when a new PTE mapping should share the same frame;
    // reject propagation of a writable COW state in debug builds.
    pub(super) fn clone_mapping(&self) -> Self {
        debug_assert!(!self.cow || self.pte_flags & PTE_W == 0);
        Self {
            frame: self.frame.clone(),
            pte_flags: self.pte_flags,
            cow: self.cow,
        }
    }
}

// AGENT: store software resident-page metadata separately from the real Sv39
// page table so the BTreeMap is not mistaken for hardware page-table storage.
pub(super) struct ResidentPages {
    pub(super) entries: BTreeMap<usize, PageTableEntry>,
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
    pub(super) fn take_all(&mut self) -> BTreeMap<usize, PageTableEntry> {
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
        return PTE_A | PTE_R;
    }

    let mut pte_flags = PTE_A | PTE_U;
    if can_read {
        pte_flags |= PTE_R;
    }
    if can_write {
        pte_flags |= PTE_R | PTE_W | PTE_D;
    }
    if can_exec {
        pte_flags |= PTE_X;
    }
    pte_flags
}

// AGENT: strip write/dirty bits when software COW owns the next write fault.
pub(super) fn pte_flags_without_write(flags: usize) -> usize {
    flags & !(PTE_W | PTE_D)
}
