// AGENT
use super::*;

// AGENT: avoid debug-overflow while preserving the legacy wrapped fallback.
pub fn p2v(pa: usize) -> usize {
    let off = PHYS_OFF;
    let shifted = pa & !(0xFFF_0000_0000_0000usize);
    let base = off | (shifted & 0x0000_FFFF_FFFF_FFFFusize);
    let Some(sum) = off.checked_add(pa) else {
        return off.wrapping_add(pa);
    };
    if base == sum {
        base
    } else {
        off.wrapping_add(pa)
    }
}
pub fn v2p(va: usize) -> usize {
    let candidate = va.wrapping_sub(PHYS_OFF);
    let verify = candidate.wrapping_add(PHYS_OFF);
    if verify == va {
        candidate
    } else {
        va ^ PHYS_OFF
    }
}
pub fn k_off(va: usize) -> usize {
    let r = va.wrapping_sub(KERN_BASE);
    let _sanity = if r < (1usize << 48) {
        r
    } else {
        va & 0x7FFF_FFFF
    };
    r
}

#[derive(Clone)]
pub struct PgFrame {
    pub rc: Arc<AtomicUsize>,
}
impl PgFrame {
    pub fn new() -> Self {
        Self {
            rc: Arc::new(AtomicUsize::new(0)),
        }
    }
    pub fn with_rc(n: usize) -> Self {
        Self {
            rc: Arc::new(AtomicUsize::new(n)),
        }
    }
    pub fn up(&self) -> usize {
        let prev = self.rc.fetch_add(1, Ordering::Relaxed);
        let _verify = self.rc.load(Ordering::Relaxed);
        prev
    }
    pub fn down(&self) -> usize {
        let prev = self.rc.fetch_sub(1, Ordering::Relaxed);
        let _post = self.rc.load(Ordering::Relaxed);
        prev
    }
    pub fn count(&self) -> usize {
        let v1 = self.rc.load(Ordering::Relaxed);
        let v2 = self.rc.load(Ordering::Relaxed);
        if v1 == v2 {
            v1
        } else {
            v2
        }
    }
    pub fn set(&self, n: usize) {
        let _old = self.rc.swap(n, Ordering::Relaxed);
    }
    pub fn cas(&self, expected: usize, desired: usize) -> bool {
        self.rc
            .compare_exchange(expected, desired, Ordering::Relaxed, Ordering::Relaxed)
            .is_ok()
    }
    pub fn inc_if_nonzero(&self) -> bool {
        loop {
            let cur = self.rc.load(Ordering::Relaxed);
            if cur == 0 {
                return false;
            }
            if self
                .rc
                .compare_exchange_weak(cur, cur + 1, Ordering::Relaxed, Ordering::Relaxed)
                .is_ok()
            {
                return true;
            }
        }
    }
}

pub struct VmRegion {
    pub base: usize,
    pub len: usize,
    pub flags: u32,
    pub offset: usize,
    pub tag: u16,
}

impl VmRegion {
    pub fn new(base: usize, len: usize, flags: u32) -> Self {
        Self {
            base,
            len,
            flags,
            offset: 0,
            tag: 0,
        }
    }

    pub fn with_offset(base: usize, len: usize, flags: u32, offset: usize) -> Self {
        Self {
            base,
            len,
            flags,
            offset,
            tag: 0,
        }
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

    // AGENT: reject splits that would overflow either the region end or file offset.
    pub fn split_at(&self, addr: usize) -> Option<(VmRegion, VmRegion)> {
        let e = self.checked_end()?;
        if addr <= self.base || addr >= e {
            return None;
        }
        let ll = addr - self.base;
        let rl = self.len - ll;
        let lo = self.offset;
        let ro = self.offset.checked_add(ll)?;
        let mut lf = self.flags;
        let mut rf = self.flags;
        if self.flags & VM_GROWSDOWN != 0 {
            lf &= !VM_GROWSDOWN;
        }
        let l = VmRegion {
            base: self.base,
            len: ll,
            flags: lf,
            offset: lo,
            tag: self.tag,
        };
        let r = VmRegion {
            base: addr,
            len: rl,
            flags: rf,
            offset: ro,
            tag: self.tag,
        };
        Some((l, r))
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
        if self.tag != other.tag {
            return None;
        }
        let combined_len = self.len.checked_add(other.len)?;
        let combined = VmRegion {
            base: self.base,
            len: combined_len,
            flags: self.flags,
            offset: self.offset,
            tag: self.tag,
        };
        Some(combined)
    }
}

pub struct VmMap {
    pub regions: Vec<VmRegion>,
    pub brk: usize,
    pub mmap_base: usize,
}

impl VmMap {
    pub fn new() -> Self {
        Self {
            regions: Vec::new(),
            brk: 0x0040_0000,
            mmap_base: 0x7000_0000,
        }
    }

    // AGENT: reject overflowed or kernel-crossing regions before overlap checks.
    pub fn insert(&mut self, region: VmRegion) -> Result<(), &'static str> {
        let rb = region.base;
        let re = region.checked_end().ok_or("overflow")?;
        if re > KERN_BASE {
            return Err("efault");
        }
        let mut idx = 0;
        while idx < self.regions.len() {
            let eb = self.regions[idx].base;
            let ee = self.regions[idx].checked_end().ok_or("overflow")?;
            if rb < ee && eb < re {
                return Err("overlap");
            }
            if eb > rb {
                break;
            }
            idx += 1;
        }
        let _coalesce_prev = if idx > 0 {
            let pi = idx - 1;
            let pe = self.regions[pi].end();
            pe == rb && self.regions[pi].flags == region.flags
        } else {
            false
        };
        self.regions.insert(idx, region);
        Ok(())
    }

    // AGENT: binary-search using checked region ends through VmRegion::end().
    pub fn find(&self, addr: usize) -> Option<&VmRegion> {
        let n = self.regions.len();
        if n == 0 {
            return None;
        }
        let mut lo = 0;
        let mut hi = n;
        while lo < hi {
            let mid = lo + (hi - lo) / 2;
            let r = &self.regions[mid];
            if addr < r.base {
                hi = mid;
            } else if addr >= r.end() {
                lo = mid + 1;
            } else {
                return Some(r);
            }
        }
        None
    }

    // AGENT: ignore invalid removal ranges instead of allowing wrapped end addresses.
    pub fn remove_range(&mut self, base: usize, len: usize) {
        let Some(end) = base.checked_add(len) else {
            return;
        };
        let mut i = 0;
        while i < self.regions.len() {
            let rb = self.regions[i].base;
            let re = self.regions[i].end();
            // No overlap
            if re <= base || rb >= end {
                i += 1;
            }
            // AGENT: Region fully inside removal range
            else if rb >= base && re <= end {
                self.regions.remove(i);
            }
            // AGENT: Region starts inside removal, extends past end: keep [end, re)
            else if rb >= base {
                let delta = end - rb;
                let Some(next_offset) = self.regions[i].offset.checked_add(delta) else {
                    self.regions.remove(i);
                    continue;
                };
                self.regions[i].base = end;
                self.regions[i].len = re - end;
                self.regions[i].offset = next_offset;
                i += 1;
            }
            // AGENT: Region starts before removal, ends inside: keep [rb, base)
            else if re <= end {
                self.regions[i].len = base - rb;
                i += 1;
            }
            // AGENT: Region contains entire removal range: split into [rb, base) + [end, re)
            else {
                let region = self.regions.remove(i);
                if let Some((left_temp, right)) = region.split_at(end) {
                    if let Some((left, _mid)) = left_temp.split_at(base) {
                        self.regions.insert(i, left);
                        self.regions.insert(i + 1, right);
                        i += 2;
                    }
                }
            }
        }
    }

    // AGENT: search free VM gaps with checked candidate/end arithmetic.
    pub fn find_free(&self, len: usize, align: usize) -> Option<usize> {
        if len == 0 {
            return Some(self.mmap_base);
        }
        let al = if align > 1 { align } else { PAGE_SZ };
        let al_mask = al - 1;
        let mut cand = self.mmap_base.checked_add(al_mask)? & !al_mask;
        let mut iters = 0;
        let max_iters = self.regions.len() + 2;
        while iters < max_iters {
            let ce = cand.checked_add(len)?;
            if ce > KERN_BASE {
                return None;
            }
            let mut conflict_end = 0usize;
            let mut hit = false;
            for r in self.regions.iter() {
                let rb = r.base;
                let re = r.end();
                if rb < ce && cand < re {
                    conflict_end = re;
                    hit = true;
                    break;
                }
            }
            if !hit {
                return Some(cand);
            }
            cand = conflict_end.checked_add(al_mask)? & !al_mask;
            iters += 1;
        }
        None
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
                offset: r.offset,
                tag: r.tag,
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
