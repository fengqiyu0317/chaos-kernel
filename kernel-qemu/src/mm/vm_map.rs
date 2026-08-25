// AGENT: keep individual VMA metadata and address-space-wide VMA collection
// operations together, separate from physical frame ownership.
use alloc::vec::Vec;

use super::{checked_align_up, max, MmapFileSource, PAGE_SZ, USER_SIGTRAMP, USER_TOP, VM_HEAP};

// AGENT: retain either anonymous policy or a positioned regular-file owner in
// every VMA so fd close and partial VMA operations cannot detach its backing.
#[derive(Clone)]
pub(crate) enum VmBacking {
    Anonymous,
    File {
        source: MmapFileSource,
        offset: usize,
    },
}

// AGENT: keep VmRegion to the VMA metadata that is currently used by the
// QEMU address-space code; derive Clone so transactional callers can snapshot
// the complete metadata list without duplicating its fields here.
#[derive(Clone)]
pub struct VmRegion {
    pub base: usize,
    pub len: usize,
    pub flags: u32,
    pub(crate) backing: VmBacking,
}

// AGENT: keep range-local validation, comparison, split, and merge operations
// on the VMA value itself.
impl VmRegion {
    // AGENT: construct one VMA metadata value without mutating a VmMap.
    pub fn new(base: usize, len: usize, flags: u32) -> Self {
        Self {
            base,
            len,
            flags,
            backing: VmBacking::Anonymous,
        }
    }

    // AGENT: construct one positioned file VMA while retaining the stable
    // mount/inode owner independently from the descriptor used at mmap time.
    pub(crate) fn new_file(
        base: usize,
        len: usize,
        flags: u32,
        source: MmapFileSource,
        offset: usize,
    ) -> Self {
        Self {
            base,
            len,
            flags,
            backing: VmBacking::File { source, offset },
        }
    }

    // AGENT: expose file-backed classification to checkpoint and lifecycle
    // callers without leaking mutable backing state.
    pub(crate) fn is_file_backed(&self) -> bool {
        matches!(self.backing, VmBacking::File { .. })
    }

    // AGENT: preserve backing identity while deriving one page-aligned VMA
    // fragment, advancing positioned file offsets by the virtual displacement.
    fn subregion(&self, base: usize, len: usize) -> Option<Self> {
        let displacement = base.checked_sub(self.base)?;
        let backing = match &self.backing {
            VmBacking::Anonymous => VmBacking::Anonymous,
            VmBacking::File { source, offset } => VmBacking::File {
                source: source.clone(),
                offset: offset.checked_add(displacement)?,
            },
        };
        Some(Self {
            base,
            len,
            flags: self.flags,
            backing,
        })
    }

    // AGENT: expose a checked end for callers that must reject overflowed VM ranges.
    pub fn checked_end(&self) -> Option<usize> {
        self.base.checked_add(self.len)
    }

    // AGENT: keep the legacy usize-returning end helper panic-free for read-only scans.
    pub fn end(&self) -> usize {
        self.checked_end().unwrap_or(usize::MAX)
    }

    // AGENT: do not let overflowed regions claim low addresses through wrapped ends.
    pub fn contains(&self, addr: usize) -> bool {
        match self.checked_end() {
            Some(end) => addr >= self.base && addr < end,
            None => false,
        }
    }

    // AGENT: treat overflowed regions as conflicting so insertion fails closed.
    pub fn overlaps(&self, other: &VmRegion) -> bool {
        let Some(a_end) = self.checked_end() else {
            return true;
        };
        let Some(b_end) = other.checked_end() else {
            return true;
        };
        // HUMAN: change "<" to "<=" to treat adjacent regions as non-overlapping
        let no_overlap = a_end <= other.base || b_end <= self.base;
        !no_overlap
    }

    // AGENT: split a valid interior address into two regions that preserve
    // the original VMA metadata.
    pub fn split_at(&self, addr: usize) -> Option<(VmRegion, VmRegion)> {
        let end = self.checked_end()?;
        if addr <= self.base || addr >= end {
            return None;
        }

        let left_len = addr - self.base;
        let right_len = end - addr;
        let left = self.subregion(self.base, left_len)?;
        let right = self.subregion(addr, right_len)?;
        Some((left, right))
    }

    // AGENT: merge only forward-adjacent regions whose two endpoints and
    // combined length are representable.
    pub fn merge_with(&self, other: &VmRegion) -> Option<VmRegion> {
        let se = self.checked_end()?;
        if se != other.base {
            return None;
        }
        if self.flags != other.flags || !self.backing_merges_with(other) {
            return None;
        }
        let combined_end = other.checked_end()?;
        let combined_len = combined_end.checked_sub(self.base)?;
        let combined = VmRegion {
            base: self.base,
            len: combined_len,
            flags: self.flags,
            backing: self.backing.clone(),
        };
        Some(combined)
    }

    // AGENT: merge file VMAs only when they retain the same inode and their
    // positioned byte ranges are exactly contiguous; anonymous VMAs need no key.
    fn backing_merges_with(&self, other: &VmRegion) -> bool {
        match (&self.backing, &other.backing) {
            (VmBacking::Anonymous, VmBacking::Anonymous) => true,
            (
                VmBacking::File {
                    source: left,
                    offset: left_offset,
                },
                VmBacking::File {
                    source: right,
                    offset: right_offset,
                },
            ) => {
                left.file_identity() == right.file_identity()
                    && left_offset.checked_add(self.len) == Some(*right_offset)
            }
            _ => false,
        }
    }
}

// AGENT: keep VmMap focused on mutable VMA collection state; address-space-wide
// metadata such as the program break stays with AddrSpace.
pub struct VmMap {
    pub(super) regions: Vec<VmRegion>,
}

const MMAP_BASE: usize = 0x7000_0000;

// AGENT: keep sorted-region mutation and free-gap queries at the collection boundary.
impl VmMap {
    // AGENT: initialize an empty per-address-space VMA collection.
    pub fn new() -> Self {
        Self {
            regions: Vec::new(),
        }
    }

    // AGENT: validate, place, and coalesce one page-granular VMA while keeping
    // the region list sorted by base address.
    pub fn insert(&mut self, region: VmRegion) -> Result<(), &'static str> {
        if region.len == 0 || region.base % PAGE_SZ != 0 || region.len % PAGE_SZ != 0 {
            return Err("einval");
        }

        let rb = region.base;
        let re = region.checked_end().ok_or("overflow")?;
        if re > USER_TOP {
            return Err("efault");
        }

        let idx = self.regions.partition_point(|region| region.base < rb);
        if idx > 0 {
            let prev_end = self.regions[idx - 1].checked_end().ok_or("overflow")?;
            if rb < prev_end {
                return Err("overlap");
            }
        }
        if idx < self.regions.len() && self.regions[idx].base < re {
            return Err("overlap");
        }

        if idx > 0 {
            let prev_idx = idx - 1;
            if let Some(merged) = self.regions[prev_idx].merge_with(&region) {
                self.regions[prev_idx] = merged;
                if idx < self.regions.len() {
                    if let Some(merged) = self.regions[prev_idx].merge_with(&self.regions[idx]) {
                        self.regions[prev_idx] = merged;
                        self.regions.remove(idx);
                    }
                }
                return Ok(());
            }
        }

        if idx < self.regions.len() {
            if let Some(merged) = region.merge_with(&self.regions[idx]) {
                self.regions[idx] = merged;
                return Ok(());
            }
        }

        // AGENT: keep VMA publication fallible under kernel-heap pressure instead
        // of letting Vec growth invoke the global allocation failure handler.
        self.regions.try_reserve(1).map_err(|_| "enomem")?;
        self.regions.insert(idx, region);
        Ok(())
    }

    // AGENT: find the last region whose base can contain addr, then verify
    // the upper bound through VmRegion::contains().
    pub fn find(&self, addr: usize) -> Option<&VmRegion> {
        let idx = self.regions.partition_point(|region| region.base <= addr);
        if idx == 0 {
            return None;
        }

        let region = &self.regions[idx - 1];
        region.contains(addr).then_some(region)
    }

    // AGENT: preflight a page-granular range before eager allocation so a brk
    // collision cannot consume frames merely to discover an existing VMA.
    pub(super) fn range_is_free(&self, base: usize, len: usize) -> bool {
        if len == 0 || base % PAGE_SZ != 0 || len % PAGE_SZ != 0 {
            return false;
        }
        let Some(end) = base.checked_add(len) else {
            return false;
        };
        end <= USER_TOP
            && !self
                .regions
                .iter()
                .any(|region| region.base < end && base < region.end())
    }

    // AGENT: reject brk shrink ranges containing any VMA not owned by the heap;
    // holes and VM_HEAP fragments remain safe to remove transactionally.
    pub(super) fn has_non_heap_overlap(&self, base: usize, len: usize) -> bool {
        if len == 0 || base % PAGE_SZ != 0 || len % PAGE_SZ != 0 {
            return true;
        }
        let Some(end) = base.checked_add(len) else {
            return true;
        };
        if end > USER_TOP {
            return true;
        }
        self.regions
            .iter()
            .any(|region| region.base < end && base < region.end() && region.flags & VM_HEAP == 0)
    }

    // AGENT: make an interior address a VMA collection boundary while leaving
    // existing boundaries and unmapped addresses unchanged.
    pub(super) fn split_at_boundary(&mut self, addr: usize) -> Result<(), &'static str> {
        let Some(idx) = self.regions.iter().position(|region| region.contains(addr)) else {
            return Ok(());
        };
        if self.regions[idx].base == addr {
            return Ok(());
        }

        let (left, right) = self.regions[idx].split_at(addr).ok_or("einval")?;
        self.regions[idx] = left;
        self.regions.insert(idx + 1, right);
        Ok(())
    }

    // AGENT: remove one half-open range while deriving left/right fragments
    // through backing-aware offset adjustment instead of anonymous rebuilding.
    pub fn remove_range(&mut self, base: usize, len: usize) {
        if len == 0 {
            return;
        }
        let Some(end) = base.checked_add(len) else {
            return;
        };

        let mut kept = Vec::with_capacity(self.regions.len());
        for region in self.regions.drain(..) {
            let rb = region.base;
            let re = region.end();
            if re <= base || rb >= end {
                kept.push(region);
                continue;
            }

            if rb < base {
                if let Some(left) = region.subregion(rb, base - rb) {
                    kept.push(left);
                }
            }
            if end < re {
                if let Some(right) = region.subregion(end, re - end) {
                    kept.push(right);
                }
            }
        }
        self.regions = kept;
    }

    // AGENT: preserve the default mmap-base policy while delegating the actual
    // bounded gap search to the hint-aware implementation.
    pub fn find_free(&self, len: usize, align: usize) -> Option<usize> {
        self.find_free_from(MMAP_BASE, len, align)
    }

    // AGENT: search upward from one caller hint without entering the reserved
    // signal-trampoline page; callers may retry from MMAP_BASE when a high hint
    // has no suitable successor gap.
    pub fn find_free_from(&self, start: usize, len: usize, align: usize) -> Option<usize> {
        if len == 0 || len % PAGE_SZ != 0 {
            return None;
        }

        let align = if align <= 1 { PAGE_SZ } else { align };
        if !align.is_power_of_two() {
            return None;
        }

        let align_addr = |addr| checked_align_up(addr, align);

        let mut cand = align_addr(max(start, MMAP_BASE))?;

        loop {
            let end = cand.checked_add(len)?;
            if end > USER_SIGTRAMP {
                return None;
            }

            let conflict = self
                .regions
                .iter()
                .find(|region| region.base < end && cand < region.end());
            let Some(conflict) = conflict else {
                return Some(cand);
            };

            cand = align_addr(conflict.end())?;
        }
    }

    // AGENT: snapshot VMA metadata for fork, checkpoint, and transactional
    // rollback callers without exposing mutable access to the source map.
    pub(super) fn clone_regions(&self) -> Vec<VmRegion> {
        self.regions.clone()
    }
}
