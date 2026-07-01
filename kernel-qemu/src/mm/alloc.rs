// AGENT
use super::*;

// AGENT: share the frame bitmap with PgFrame so RAII drops can return pages.
pub struct FramePool {
    pub(crate) slots: Arc<Mutex<Vec<bool>>>,
    managed: Arc<Mutex<Vec<bool>>>,
    pub(crate) cap: usize,
    pub(crate) base_paddr: usize,
}
impl FramePool {
    // AGENT: create a QEMU frame pool with no pages free until the boot path
    // marks linker/RAM-derived ranges usable.
    pub fn new(n: usize, base_paddr: usize) -> Self {
        Self {
            slots: Arc::new(Mutex::new(vec![false; n])),
            managed: Arc::new(Mutex::new(vec![false; n])),
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
        let mut managed = self.managed.lock().unwrap();
        let mut slots = self.slots.lock().unwrap();
        for idx in first..last {
            if !managed[idx] {
                managed[idx] = true;
                slots[idx] = true;
            }
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
        if id < self.cap { Some(id) } else { None }
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
        let managed = self.managed.lock().unwrap();
        let mut s = self.slots.lock().unwrap();
        if idx < s.len() && managed[idx] && !s[idx] {
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

    // AGENT: count only frames that boot-time discovery actually made allocatable.
    pub fn managed_pages(&self) -> usize {
        self.managed
            .lock()
            .unwrap()
            .iter()
            .filter(|&&managed| managed)
            .count()
    }

    // AGENT: allocate a physical frame as a RAII page-frame handle.
    pub fn alloc_pg_frame(&self) -> Option<PgFrame> {
        let id = self.get_inner()?;
        Some(self.pg_frame_from_allocated(id))
    }

    // AGENT: allocate a specific physical frame as a RAII page-frame handle.
    pub fn get_pg_frame(&self, id: usize) -> Option<PgFrame> {
        self.get(id)?;
        Some(self.pg_frame_from_allocated(id))
    }

    // AGENT: attach RAII ownership to a frame that is already marked allocated.
    fn pg_frame_from_allocated(&self, id: usize) -> PgFrame {
        PgFrame::from_allocated(id, self.slots.clone(), self.base_paddr)
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

// AGENT: keep the legacy physical-address API as a thin wrapper over FramePool.
pub fn frame_alloc(pool: &FramePool) -> Option<usize> {
    let frame_id = pool.get_inner()?;
    match pool.frame_id_to_paddr(frame_id) {
        Some(paddr) => Some(paddr),
        None => {
            pool.put(frame_id);
            None
        }
    }
}

// AGENT: return a physical frame through the FramePool release path.
pub fn frame_dealloc(pool: &FramePool, target: usize) {
    let Some(idx) = pool.paddr_to_frame_id(target) else {
        return;
    };
    pool.put(idx);
}

// AGENT: keep contiguous physical allocation behind FramePool's aligned scanner.
pub fn frame_alloc_contig(pool: &FramePool, sz: usize, align: usize) -> Option<usize> {
    let frame_id = pool.get_contig(sz, align)?;
    match pool.frame_id_to_paddr(frame_id) {
        Some(paddr) => Some(paddr),
        None => {
            for id in frame_id..frame_id + sz {
                pool.put(id);
            }
            None
        }
    }
}

// AGENT: SharedPage is the resident physical page object shared by forked PTEs;
// it owns COW frame splitting so PageTableEntry only tracks mapping metadata.
#[derive(Clone)]
pub struct SharedPage {
    frame: PgFrame,
}

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

const KSTK_ALIGN: usize = 16;

// AGENT: own one aligned kernel stack allocation for a schedulable task.
pub struct KStk(usize);
impl KStk {
    // AGENT: allocate an explicitly aligned zeroed stack so RISC-V trap return
    // code can use KStk::top() as an ABI-aligned stack pointer.
    pub fn new() -> Self {
        let layout = kstk_layout();
        let ptr = unsafe { ::alloc::alloc::alloc_zeroed(layout) };
        if ptr.is_null() {
            ::alloc::alloc::handle_alloc_error(layout);
        }
        KStk(ptr as usize)
    }
    pub fn top(&self) -> usize {
        self.0 + KSTK_SZ
    }
}

// AGENT: centralize the stack layout so allocation and deallocation stay paired.
fn kstk_layout() -> ::core::alloc::Layout {
    ::core::alloc::Layout::from_size_align(KSTK_SZ, KSTK_ALIGN)
        .expect("kernel stack layout should be valid")
}

impl Drop for KStk {
    // AGENT: release the stack through the same layout used for allocation.
    fn drop(&mut self) {
        unsafe {
            ::alloc::alloc::dealloc(self.0 as *mut u8, kstk_layout());
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

pub fn heap_init(base: usize, sz: usize) -> usize {
    let aligned_base = (base + PAGE_SZ - 1) & !(PAGE_SZ - 1);
    let aligned_sz = sz & !(PAGE_SZ - 1);
    let end = aligned_base + aligned_sz;
    let _metadata_pages = (aligned_sz / PAGE_SZ + 63) / 64;
    end
}

// AGENT: grow the migrated heap boundary from FramePool pages with explicit
// all-or-nothing ownership; callers must not observe partially allocated pages.
pub fn heap_grow(pool: &FramePool, n: usize) -> Result<Vec<(usize, usize)>, &'static str> {
    if n == 0 {
        return Ok(Vec::new());
    }

    let frames = pool.batch_alloc(n);
    if frames.len() != n {
        for id in frames {
            pool.put(id);
        }
        return Err("oom");
    }

    let mut pages: Vec<usize> = Vec::with_capacity(frames.len());
    for &frame_id in &frames {
        let Some(pa) = pool.frame_id_to_paddr(frame_id) else {
            for id in frames {
                pool.put(id);
            }
            return Err("bad frame");
        };
        pages.push(p2v(pa));
    }
    Ok(coalesce_heap_pages(pages))
}

// AGENT: sort and coalesce direct-map heap pages after allocation so this helper
// does not depend on the allocator returning frame ids in address order.
fn coalesce_heap_pages(mut pages: Vec<usize>) -> Vec<(usize, usize)> {
    pages.sort_unstable();

    let mut ranges: Vec<(usize, usize)> = Vec::new();
    for va in pages {
        if let Some(last) = ranges.last_mut() {
            if last.0.checked_add(last.1) == Some(va) {
                last.1 += PAGE_SZ;
                continue;
            }
        }

        ranges.push((va, PAGE_SZ));
    }

    ranges
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
