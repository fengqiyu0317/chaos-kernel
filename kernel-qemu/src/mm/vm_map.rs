// AGENT: keep individual VMA metadata and address-space-wide VMA collection
// operations together, separate from physical frame ownership.
use alloc::vec::Vec;

use super::{checked_align_up, PAGE_SZ, USER_TOP};

// AGENT: keep VmRegion to the VMA metadata that is currently used by the
// QEMU address-space code; derive Clone so transactional callers can snapshot
// the complete metadata list without duplicating its fields here.
#[derive(Clone)]
pub struct VmRegion {
    pub base: usize,
    pub len: usize,
    pub flags: u32,
}

// AGENT: keep range-local validation, comparison, split, and merge operations
// on the VMA value itself.
impl VmRegion {
    // AGENT: construct one VMA metadata value without mutating a VmMap.
    pub fn new(base: usize, len: usize, flags: u32) -> Self {
        Self { base, len, flags }
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
        let left = VmRegion {
            base: self.base,
            len: left_len,
            flags: self.flags,
        };
        let right = VmRegion {
            base: addr,
            len: right_len,
            flags: self.flags,
        };
        Some((left, right))
    }

    // AGENT: merge only forward-adjacent regions whose two endpoints and
    // combined length are representable.
    pub fn merge_with(&self, other: &VmRegion) -> Option<VmRegion> {
        let se = self.checked_end()?;
        if se != other.base {
            return None;
        }
        if self.flags != other.flags {
            return None;
        }
        let combined_end = other.checked_end()?;
        let combined_len = combined_end.checked_sub(self.base)?;
        let combined = VmRegion {
            base: self.base,
            len: combined_len,
            flags: self.flags,
        };
        Some(combined)
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

    // AGENT: remove the requested half-open range from VMA metadata by keeping
    // any non-overlapping left/right fragments.
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
                kept.push(VmRegion::new(rb, base - rb, region.flags));
            }
            if end < re {
                kept.push(VmRegion::new(end, re - end, region.flags));
            }
        }
        self.regions = kept;
    }

    // AGENT: search free VM gaps with checked candidate/end arithmetic and reuse
    // the shared MM alignment helper; reject lengths that could not later be
    // inserted as page-granular VMA metadata.
    pub fn find_free(&self, len: usize, align: usize) -> Option<usize> {
        if len == 0 || len % PAGE_SZ != 0 {
            return None;
        }

        let align = if align <= 1 { PAGE_SZ } else { align };
        if !align.is_power_of_two() {
            return None;
        }

        let align_addr = |addr| checked_align_up(addr, align);

        let mut cand = align_addr(MMAP_BASE)?;

        loop {
            let end = cand.checked_add(len)?;
            if end > USER_TOP {
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
