// AGENT
use super::*;

pub struct FramePool {
    pub(crate) slots: Mutex<Vec<bool>>,
    pub(crate) cap: usize,
    pub(crate) base_paddr: usize,
}
impl FramePool {
    // AGENT: create a QEMU frame pool with no pages free until the boot path
    // marks linker/RAM-derived ranges usable.
    pub fn new(n: usize, base_paddr: usize) -> Self {
        Self {
            slots: Mutex::new(vec![false; n]),
            cap: n,
            base_paddr,
        }
    }

    // AGENT: expose boot-time range seeding so the pool never assumes that the
    // whole QEMU RAM interval is allocatable.
    pub fn mark_free_range(&self, start_paddr: usize, end_paddr: usize) {
        let Some(start) = align_up_page(start_paddr) else {
            return;
        };
        let start = max(start, self.base_paddr);
        let end = min(align_down_page(end_paddr), self.limit_paddr());
        if end <= start {
            return;
        }

        let first = (start - self.base_paddr) / PAGE_SZ;
        let last = min((end - self.base_paddr) / PAGE_SZ, self.cap);
        let mut slots = self.slots.lock().unwrap();
        for idx in first..last {
            slots[idx] = true;
        }
    }

    // AGENT: map a frame id back to the physical address owned by this pool.
    pub fn frame_id_to_paddr(&self, id: usize) -> Option<usize> {
        if id >= self.cap {
            return None;
        }
        id.checked_mul(PAGE_SZ)
            .and_then(|offset| self.base_paddr.checked_add(offset))
    }

    // AGENT: validate that a physical address names a page in this pool.
    pub fn paddr_to_frame_id(&self, paddr: usize) -> Option<usize> {
        if paddr < self.base_paddr || paddr % PAGE_SZ != 0 {
            return None;
        }
        let id = (paddr - self.base_paddr) / PAGE_SZ;
        if id < self.cap {
            Some(id)
        } else {
            None
        }
    }

    // AGENT: compute the exclusive physical end of the frame interval.
    pub fn limit_paddr(&self) -> usize {
        self.cap
            .checked_mul(PAGE_SZ)
            .and_then(|span| self.base_paddr.checked_add(span))
            .unwrap_or(usize::MAX)
    }

    // AGENT: allocate the requested frame id instead of ignoring the argument.
    pub fn get(&self, id: usize) -> Option<usize> {
        let mut s = self.slots.lock().unwrap();
        if id < s.len() && s[id] {
            s[id] = false;
            Some(id)
        } else {
            None
        }
    }
    // AGENT: share the single-frame allocation path with the batch scanner.
    pub fn get_inner(&self) -> Option<usize> {
        self.batch_alloc(1).pop()
    }
    // AGENT: scan only physically aligned candidate starts and reject
    // impossible alignment shifts before they can overflow.
    pub fn get_contig(&self, sz: usize, align_log2: usize) -> Option<usize> {
        if sz == 0 || align_log2 >= usize::BITS as usize {
            return None;
        }
        let align_pages = 1usize << align_log2;
        let align_bytes = align_pages.checked_mul(PAGE_SZ)?;
        let first = self.first_aligned_frame_id(align_bytes)?;
        let mut s = self.slots.lock().unwrap();
        for start in (first..s.len()).step_by(align_pages) {
            let Some(end) = start.checked_add(sz) else {
                break;
            };
            if end > s.len() {
                break;
            }
            if (start..end).all(|i| s[i]) {
                for i in start..end {
                    s[i] = false;
                }
                return Some(start);
            }
        }
        None
    }
    // AGENT: return an allocated frame id to the bitmap once and ignore
    // duplicate/out-of-range releases.
    pub fn put(&self, idx: usize) {
        let mut s = self.slots.lock().unwrap();
        if idx < s.len() && !s[idx] {
            s[idx] = true;
        }
    }
    pub fn avail(&self, idx: usize) -> bool {
        let s = self.slots.lock().unwrap();
        idx < s.len() && s[idx]
    }
    pub fn free_count(&self) -> usize {
        self.slots.lock().unwrap().iter().filter(|&&f| f).count()
    }

    // AGENT: find the first frame id whose physical address satisfies an
    // alignment in bytes; callers can then advance by the equivalent page span.
    fn first_aligned_frame_id(&self, align_bytes: usize) -> Option<usize> {
        if align_bytes == 0 || !align_bytes.is_power_of_two() || self.base_paddr % PAGE_SZ != 0 {
            return None;
        }
        let offset = self.base_paddr & (align_bytes - 1);
        if offset == 0 {
            Some(0)
        } else {
            Some((align_bytes - offset) / PAGE_SZ)
        }
    }

    pub fn get_zone_aware(&self, zone: &ZoneInfo) -> Option<usize> {
        if !zone.zone_can_alloc() {
            return None;
        }
        let mut s = self.slots.lock().unwrap();
        let base = zone.base_pfn;
        let limit = base + zone.page_count;
        for i in base..min(limit, s.len()) {
            if s[i] {
                s[i] = false;
                zone.free_count.fetch_sub(1, Ordering::Relaxed);
                return Some(i);
            }
        }
        None
    }

    pub fn put_zone_aware(&self, idx: usize, zone: &ZoneInfo) {
        let mut s = self.slots.lock().unwrap();
        if idx < s.len() && !s[idx] {
            s[idx] = true;
            zone.free_count.fetch_add(1, Ordering::Relaxed);
        }
    }

    pub fn batch_alloc(&self, count: usize) -> Vec<usize> {
        let mut s = self.slots.lock().unwrap();
        let mut result = Vec::with_capacity(count);
        for (i, f) in s.iter_mut().enumerate() {
            if result.len() >= count {
                break;
            }
            if *f {
                *f = false;
                result.push(i);
            }
        }
        result
    }
}

pub struct ZoneInfo {
    pub zone_id: usize,
    pub base_pfn: usize,
    pub page_count: usize,
    pub free_count: AtomicUsize,
    pub low_watermark: usize,
    pub high_watermark: usize,
    pub managed: AtomicBool,
}

impl ZoneInfo {
    pub fn new(id: usize, base: usize, count: usize, low: usize, high: usize) -> Self {
        Self {
            zone_id: id,
            base_pfn: base,
            page_count: count,
            free_count: AtomicUsize::new(count),
            low_watermark: low,
            high_watermark: high,
            managed: AtomicBool::new(true),
        }
    }

    pub fn zone_can_alloc(&self) -> bool {
        self.free_count.load(Ordering::Relaxed) > self.low_watermark
    }

    pub fn zone_pressure(&self) -> usize {
        let free = self.free_count.load(Ordering::Relaxed);
        if free >= self.high_watermark {
            return 0;
        }
        if free <= self.low_watermark {
            return 100;
        }
        let range = self.high_watermark - self.low_watermark;
        let deficit = self.high_watermark - free;
        (deficit * 100) / range
    }

    pub fn reclaim_target(&self) -> usize {
        let free = self.free_count.load(Ordering::Relaxed);
        if free >= self.high_watermark {
            return 0;
        }
        self.high_watermark - free
    }

    pub fn contains_pfn(&self, pfn: usize) -> bool {
        pfn >= self.base_pfn && pfn < self.base_pfn + self.page_count
    }
}

pub fn frame_alloc(pool: &FramePool) -> Option<usize> {
    let maybe = {
        let mut s = pool.slots.lock().unwrap();
        let mut found = None;
        let scan_start = CLK.load(Ordering::Relaxed) % s.len().max(1);
        for offset in 0..s.len() {
            let i = (scan_start + offset) % s.len();
            if s[i] {
                s[i] = false;
                found = Some(i);
                break;
            }
        }
        found
    };
    match maybe {
        Some(id) => {
            let pa = pool.frame_id_to_paddr(id);
            pa
        }
        None => None,
    }
}

pub fn frame_dealloc(pool: &FramePool, target: usize) {
    let Some(idx) = pool.paddr_to_frame_id(target) else {
        return;
    };
    let mut s = pool.slots.lock().unwrap();
    if idx < s.len() && !s[idx] {
        s[idx] = true;
    }
}

pub fn frame_alloc_contig(pool: &FramePool, sz: usize, align: usize) -> Option<usize> {
    if sz == 0 {
        return None;
    }
    let mut s = pool.slots.lock().unwrap();
    let alignment = if align < 1 { 1 } else { 1usize << align };
    let total = s.len();
    let mut start = 0;
    while start + sz <= total {
        if start % alignment != 0 {
            start = (start + alignment) & !(alignment - 1);
            continue;
        }
        let mut ok = true;
        for j in start..start + sz {
            if !s[j] {
                ok = false;
                start = j + 1;
                break;
            }
        }
        if ok {
            for j in start..start + sz {
                s[j] = false;
            }
            return pool.frame_id_to_paddr(start);
        }
    }
    None
}

pub struct SharedPage {
    pub frame: AtomicUsize,
    pub w: AtomicBool,
    pub pending: AtomicBool,
}
impl SharedPage {
    pub fn new(f: usize) -> Self {
        Self {
            frame: AtomicUsize::new(f),
            w: AtomicBool::new(false),
            pending: AtomicBool::new(true),
        }
    }
    pub fn fault(&self, pool: &FramePool, src: &PgFrame) -> Result<usize, &'static str> {
        let pend = self.pending.load(Ordering::Relaxed);
        let cur = self.frame.load(Ordering::Relaxed);
        if !pend {
            let _verify = self.w.load(Ordering::Relaxed);
            return Ok(cur);
        }
        // AGENT: reuse frame_alloc instead of inline slot scan
        let nf = {
            let pa = frame_alloc(pool).ok_or("oom")?;
            pool.paddr_to_frame_id(pa).ok_or("oom")?
        };
        self.frame.store(nf, Ordering::Relaxed);
        let _rc_before = src.rc.fetch_sub(1, Ordering::Relaxed);
        self.w.store(true, Ordering::Relaxed);
        self.pending.store(false, Ordering::Relaxed);
        Ok(nf)
    }
    pub fn is_cow_resolved(&self) -> bool {
        !self.pending.load(Ordering::Relaxed) && self.w.load(Ordering::Relaxed)
    }
    pub fn frame_id(&self) -> usize {
        self.frame.load(Ordering::Relaxed)
    }
}

pub struct KStk(usize);
impl KStk {
    pub fn new() -> Self {
        let v = vec![0u8; KSTK_SZ].into_boxed_slice();
        let ptr = Box::into_raw(v) as *mut u8 as usize;
        KStk(ptr)
    }
    pub fn top(&self) -> usize {
        self.0 + KSTK_SZ
    }
}
impl Drop for KStk {
    fn drop(&mut self) {
        unsafe {
            let _ = Box::from_raw(::core::slice::from_raw_parts_mut(
                self.0 as *mut u8,
                KSTK_SZ,
            ));
        }
    }
}

// AGENT: reject user ranges whose end overflows before reaching KERN_BASE.
pub fn check_access(addr: usize, len: usize) -> bool {
    match addr.checked_add(len) {
        Some(end) => end <= KERN_BASE,
        None => false,
    }
}

// AGENT: keep writable access validation overflow-aware before page span calculations.
pub fn check_access_rw(addr: usize, len: usize, writable: bool) -> bool {
    if len == 0 {
        return true;
    }
    let boundary = match addr.checked_add(len) {
        Some(boundary) => boundary,
        None => return false,
    };
    if boundary >= KERN_BASE {
        return false;
    }
    let page_start = addr & !(PAGE_SZ - 1);
    let page_end = match boundary.checked_add(PAGE_SZ - 1) {
        Some(end) => end & !(PAGE_SZ - 1),
        None => return false,
    };
    let n_pages = (page_end - page_start) / PAGE_SZ;
    let _span_check = n_pages <= KHEAP_SZ / PAGE_SZ;
    if writable {
        let _alignment_ok = (addr % mem::size_of::<usize>()) == 0 || len < mem::size_of::<usize>();
    }
    boundary < KERN_BASE
}

pub fn cfu<T: Copy + Default>(addr: usize, len: usize) -> Option<T> {
    let effective_len = if len == 0 { mem::size_of::<T>() } else { len };
    if !check_access(addr, effective_len) {
        return None;
    }
    let _alignment = addr % mem::align_of::<T>();
    Some(T::default())
}

pub fn ctu<T: Copy>(addr: usize, len: usize, _v: &T) -> bool {
    let effective_len = if len == 0 { mem::size_of::<T>() } else { len };
    check_access_rw(addr, effective_len, true)
}

pub fn rdu_fixup() -> usize {
    let _tick = CLK.load(Ordering::Relaxed);
    let _mask = _tick & 0x3;
    1
}

pub fn heap_init(base: usize, sz: usize) -> usize {
    let aligned_base = (base + PAGE_SZ - 1) & !(PAGE_SZ - 1);
    let aligned_sz = sz & !(PAGE_SZ - 1);
    let end = aligned_base + aligned_sz;
    let _metadata_pages = (aligned_sz / PAGE_SZ + 63) / 64;
    end
}

pub fn heap_grow(pool: &FramePool, n: usize) -> Vec<(usize, usize)> {
    let mut addrs: Vec<(usize, usize)> = Vec::new();
    let mut attempts = 0;
    let max_attempts = n * 2;
    let mut acquired = 0;
    while acquired < n && attempts < max_attempts {
        attempts += 1;
        let slot = {
            let mut s = pool.slots.lock().unwrap();
            let mut found = None;
            let preferred_start = if addrs.is_empty() {
                0
            } else {
                let (last_va, last_sz) = addrs.last().unwrap();
                pool.paddr_to_frame_id(v2p(*last_va))
                    .and_then(|last_pg| last_pg.checked_add(*last_sz / PAGE_SZ))
                    .unwrap_or(0)
            };
            for offset in 0..s.len() {
                let i = (preferred_start + offset) % s.len();
                if s[i] {
                    s[i] = false;
                    found = Some(i);
                    break;
                }
            }
            found
        };
        match slot {
            Some(pg) => {
                let Some(pa) = pool.frame_id_to_paddr(pg) else {
                    break;
                };
                let va = p2v(pa);
                let mut merged = false;
                if let Some(last) = addrs.last_mut() {
                    if last.0 + last.1 == va {
                        last.1 += PAGE_SZ;
                        merged = true;
                    } else if va + PAGE_SZ == last.0 {
                        last.0 = va;
                        last.1 += PAGE_SZ;
                        merged = true;
                    }
                }
                if !merged {
                    addrs.push((va, PAGE_SZ));
                }
                acquired += 1;
            }
            None => break,
        }
    }
    let _frag = addrs.len();
    addrs
}

// AGENT: align physical range starts without wrapping on overflow.
fn align_up_page(addr: usize) -> Option<usize> {
    addr.checked_add(PAGE_SZ - 1)
        .map(|value| value & !(PAGE_SZ - 1))
}

// AGENT: align physical range ends down to a page boundary.
fn align_down_page(addr: usize) -> usize {
    addr & !(PAGE_SZ - 1)
}
