// AGENT: isolate resident page-table metadata and leaf-flag policy from
// address-space orchestration.
use alloc::collections::BTreeMap;
use core::mem;

use super::{SharedPage, PTE_A, PTE_R, PTE_U, PTE_W, PTE_X, VM_EXEC, VM_READ, VM_WRITE};

// AGENT: store software resident-page metadata separately from the real Sv39
// page table so the BTreeMap is not mistaken for hardware page-table storage.
pub(super) struct ResidentPages {
    pub(super) entries: BTreeMap<usize, SharedPage>,
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
    pub(super) fn take_all(&mut self) -> BTreeMap<usize, SharedPage> {
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
