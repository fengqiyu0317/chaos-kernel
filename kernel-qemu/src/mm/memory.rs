// AGENT
use super::*;

// AGENT: translate a physical address through the current high-half direct map.
pub fn p2v(pa: usize) -> usize {
    PHYS_OFF.checked_add(pa).expect("p2v overflow")
}

// AGENT: reverse p2v() and reject addresses outside the high-half direct map.
pub fn v2p(va: usize) -> usize {
    va.checked_sub(PHYS_OFF).expect("v2p below direct map")
}

// AGENT: compute an offset from the kernel virtual base without wrapping.
pub fn k_off(va: usize) -> usize {
    va.checked_sub(KERN_BASE)
        .expect("kernel address below KERN_BASE")
}

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

impl PgFrame {
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

    pub fn id(&self) -> usize {
        self.inner.id
    }

    pub fn paddr(&self) -> usize {
        self.inner
            .id
            .checked_mul(PAGE_SZ)
            .and_then(|offset| self.inner.base_paddr.checked_add(offset))
            .unwrap_or(usize::MAX)
    }

    pub fn count(&self) -> usize {
        Arc::strong_count(&self.inner)
    }

    pub fn is_unique(&self) -> bool {
        self.count() == 1
    }
}

impl Drop for PgFrameInner {
    fn drop(&mut self) {
        let mut slots = self.slots.lock().unwrap();
        if self.id < slots.len() && !slots[self.id] {
            slots[self.id] = true;
        }
    }
}

// AGENT: keep VmRegion to the VMA metadata that is currently used by the
// QEMU address-space code; unused region tag/offset fields were removed.
pub struct VmRegion {
    pub base: usize,
    pub len: usize,
    pub flags: u32,
}

impl VmRegion {
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

    // AGENT: merge only when both endpoints and combined length are representable.
    pub fn merge_with(&self, other: &VmRegion) -> Option<VmRegion> {
        let se = self.checked_end()?;
        if se != other.base {
            return None;
        }
        if self.flags != other.flags {
            return None;
        }
        let combined_len = self.len.checked_add(other.len)?;
        let combined = VmRegion {
            base: self.base,
            len: combined_len,
            flags: self.flags,
        };
        Some(combined)
    }
}

// AGENT: keep VmMap to mutable per-address-space state; the fixed mmap search
// base is not stored per address space.
pub struct VmMap {
    pub regions: Vec<VmRegion>,
    pub brk: usize,
}

const MMAP_BASE: usize = 0x7000_0000;

impl VmMap {
    // AGENT: initialize only the VM metadata that can differ by address space.
    pub fn new() -> Self {
        Self {
            regions: Vec::new(),
            brk: 0x0040_0000,
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
        if re > KERN_BASE {
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
    // the shared MM alignment helper with stricter failure checks.
    pub fn find_free(&self, len: usize, align: usize) -> Option<usize> {
        if len == 0 {
            return Some(MMAP_BASE);
        }

        let align = if align <= 1 { PAGE_SZ } else { align };
        if !align.is_power_of_two() {
            return None;
        }

        let align_addr = |addr| {
            let aligned = align_up(addr, align);
            (aligned >= addr && aligned % align == 0).then_some(aligned)
        };

        let mut cand = align_addr(MMAP_BASE)?;

        loop {
            let end = cand.checked_add(len)?;
            if end > KERN_BASE {
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

    // AGENT: report a saturated total instead of wrapping mapped byte counts.
    pub fn total_mapped(&self) -> usize {
        let mut s = 0usize;
        for r in self.regions.iter() {
            s = s.saturating_add(r.len);
        }
        s
    }

    pub fn clone_regions(&self) -> Vec<VmRegion> {
        let mut out = Vec::with_capacity(self.regions.len());
        for r in self.regions.iter() {
            let nr = VmRegion {
                base: r.base,
                len: r.len,
                flags: r.flags,
            };
            out.push(nr);
        }
        out
    }

    pub fn gap_after(&self, idx: usize) -> usize {
        if idx >= self.regions.len() {
            return 0;
        }
        let re = self.regions[idx].end();
        if idx + 1 < self.regions.len() {
            self.regions[idx + 1].base.saturating_sub(re)
        } else {
            KERN_BASE.saturating_sub(re)
        }
    }
}
