// AGENT: bootstrap Rust alloc users from the linker heap, then switch to a
// reclaiming direct-map heap whose physical pages come from FramePool on demand.
use alloc::boxed::Box;
use alloc::collections::BTreeMap;
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::alloc::{GlobalAlloc, Layout};
use core::ptr::null_mut;
use core::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use crate::irq_lock::Mutex;
use crate::kernel::{p2v, v2p, FramePool, PAGE_SZ};
use crate::println;

const HEAP_ALIGN: usize = 16;

unsafe extern "C" {
    static mut sheap: u8;
    static mut eheap: u8;
}

// AGENT: preserve a monotonic linker-backed allocator only for the boot cycle
// in which FramePool and the kernel Sv39 direct map do not exist yet.
struct EarlyBump {
    start: AtomicUsize,
    next: AtomicUsize,
    end: AtomicUsize,
    ready: AtomicBool,
}

impl EarlyBump {
    // AGENT: construct inert bootstrap state for static initialization.
    const fn new() -> Self {
        Self {
            start: AtomicUsize::new(0),
            next: AtomicUsize::new(0),
            end: AtomicUsize::new(0),
            ready: AtomicBool::new(false),
        }
    }

    // AGENT: bind the bootstrap allocator to the linker-provided heap range.
    fn init(&self, base: usize, size: usize) -> usize {
        assert!(
            !self.ready.swap(true, Ordering::AcqRel),
            "early heap initialized more than once"
        );
        let start = align_up(base, HEAP_ALIGN).expect("early heap base overflow");
        let raw_end = base.checked_add(size).expect("early heap range overflow");
        let end = align_down(raw_end, HEAP_ALIGN);
        assert!(start < end, "early heap range is empty");

        self.start.store(start, Ordering::Relaxed);
        self.next.store(start, Ordering::Relaxed);
        self.end.store(end, Ordering::Release);
        end
    }

    // AGENT: monotonically allocate only during the pre-direct-map boot phase.
    fn alloc(&self, layout: Layout) -> *mut u8 {
        if !self.ready.load(Ordering::Acquire) {
            return null_mut();
        }

        let align = layout.align().max(HEAP_ALIGN);
        let size = layout.size().max(1);
        let heap_end = self.end.load(Ordering::Acquire);
        let mut cur = self.next.load(Ordering::Relaxed);
        loop {
            let Some(alloc_start) = align_up(cur, align) else {
                return null_mut();
            };
            let Some(alloc_end) = alloc_start.checked_add(size) else {
                return null_mut();
            };
            if alloc_end > heap_end {
                return null_mut();
            }
            match self
                .next
                .compare_exchange(cur, alloc_end, Ordering::AcqRel, Ordering::Relaxed)
            {
                Ok(_) => return alloc_start as *mut u8,
                Err(actual) => cur = actual,
            }
        }
    }

    // AGENT: recognize bootstrap pointers, whose lifetime remains permanent.
    fn contains(&self, ptr: *mut u8) -> bool {
        let addr = ptr as usize;
        addr >= self.start.load(Ordering::Acquire) && addr < self.end.load(Ordering::Acquire)
    }

    // AGENT: report bootstrap consumption for the promotion diagnostic.
    fn used(&self) -> usize {
        self.next
            .load(Ordering::Relaxed)
            .saturating_sub(self.start.load(Ordering::Relaxed))
    }

    // AGENT: report the fixed linker bootstrap capacity.
    fn capacity(&self) -> usize {
        self.end
            .load(Ordering::Relaxed)
            .saturating_sub(self.start.load(Ordering::Relaxed))
    }
}

// AGENT: keep the live heap deliberately simple: every allocation owns a
// page-aligned physical run, and deallocation derives that run from ptr+Layout.
struct DynamicHeap {
    pool: Option<Arc<FramePool>>,
    live_allocations: usize,
    owned_pages: usize,
}

impl DynamicHeap {
    // AGENT: initialize the page-backed heap without allocating metadata.
    const fn new() -> Self {
        Self {
            pool: None,
            live_allocations: 0,
            owned_pages: 0,
        }
    }

    // AGENT: install the shared physical frame owner exactly once.
    fn install_pool(&mut self, pool: Arc<FramePool>) {
        assert!(self.pool.is_none(), "kernel heap promoted more than once");
        self.pool = Some(pool);
    }

    // AGENT: allocate one contiguous page run and return its direct-map base;
    // small objects intentionally trade memory efficiency for simple reclaim.
    fn alloc(&mut self, layout: Layout) -> *mut u8 {
        let Some(pages) = allocation_pages(layout) else {
            return null_mut();
        };
        let align_pages = if layout.align() <= PAGE_SZ {
            1
        } else {
            layout.align() / PAGE_SZ
        };
        let Some(pool) = self.pool.as_ref() else {
            return null_mut();
        };
        let Some(paddr) = pool.alloc_contiguous_pages(pages, align_pages) else {
            return null_mut();
        };
        let payload = p2v(paddr);
        if payload % layout.align() != 0 {
            let _ = pool.release_contiguous_pages(paddr, pages);
            return null_mut();
        }
        self.live_allocations += 1;
        self.owned_pages += pages;
        payload as *mut u8
    }

    // AGENT: release exactly the page run reconstructed from the pointer and
    // the Layout that GlobalAlloc requires the caller to pass back unchanged.
    unsafe fn dealloc(&mut self, ptr: *mut u8, layout: Layout) {
        let pages = allocation_pages(layout).expect("invalid kernel heap layout");
        assert_eq!(
            ptr as usize % PAGE_SZ,
            0,
            "kernel heap pointer is not page aligned"
        );
        assert_eq!(
            ptr as usize % layout.align(),
            0,
            "kernel heap pointer violates its layout"
        );
        let paddr = v2p(ptr as usize);
        let released = self
            .pool
            .as_ref()
            .expect("promoted heap lost FramePool")
            .release_contiguous_pages(paddr, pages);
        assert!(
            released,
            "kernel heap span release violated FramePool ownership"
        );
        self.live_allocations -= 1;
        self.owned_pages -= pages;
    }
}

// AGENT: route allocations across the boot-only and live page-backed phases
// while leaving early pointers valid for the lifetime of the kernel.
struct KernelHeap {
    early: EarlyBump,
    dynamic: Mutex<DynamicHeap>,
    promoted: AtomicBool,
}

impl KernelHeap {
    // AGENT: construct both allocation phases without performing allocation.
    const fn new() -> Self {
        Self {
            early: EarlyBump::new(),
            dynamic: Mutex::new(DynamicHeap::new()),
            promoted: AtomicBool::new(false),
        }
    }

    // AGENT: publish the page-backed phase after the direct map is active.
    fn promote(&self, pool: Arc<FramePool>) {
        assert!(self.early.ready.load(Ordering::Acquire));
        self.dynamic.lock().install_pool(pool);
        self.promoted.store(true, Ordering::Release);
    }

    // AGENT: expose allocation and frame counts used by the QEMU smoke test.
    fn stats(&self) -> (usize, usize, usize) {
        let heap = self.dynamic.lock();
        (
            heap.live_allocations,
            heap.owned_pages,
            heap.pool.as_ref().map_or(0, |pool| pool.free_count()),
        )
    }
}

unsafe impl Sync for KernelHeap {}

unsafe impl GlobalAlloc for KernelHeap {
    // AGENT: route new allocations to the currently active boot phase.
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        if self.promoted.load(Ordering::Acquire) {
            self.dynamic.lock().alloc(layout)
        } else {
            self.early.alloc(layout)
        }
    }

    // AGENT: pass the original Layout through so the page run can be reclaimed
    // without an AllocHeader or a separately allocated ownership table.
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        if ptr.is_null() || self.early.contains(ptr) {
            return;
        }
        assert!(
            self.promoted.load(Ordering::Acquire),
            "non-early allocation released before heap promotion"
        );
        self.dynamic.lock().dealloc(ptr, layout);
    }
}

#[global_allocator]
static HEAP: KernelHeap = KernelHeap::new();

// AGENT: keep the heap_init naming used by the migrated MM baseline while
// binding it only to the linker-provided bootstrap interval.
pub fn heap_init(base: usize, size: usize) -> usize {
    HEAP.early.init(base, size)
}

// AGENT: initialize the fixed bootstrap heap before FramePool construction.
pub fn init() {
    let base = core::ptr::addr_of_mut!(sheap) as usize;
    let end = core::ptr::addr_of_mut!(eheap) as usize;
    let size = end.saturating_sub(base);
    let heap_end = heap_init(base, size);
    println!(
        "[kernel-qemu] early heap ready base={:#x} end={:#x} bytes={}",
        base, heap_end, size
    );
}

// AGENT: publish the one shared FramePool only after the direct map is active;
// no physical heap pages are claimed until a later allocation needs one.
pub fn promote(pool: Arc<FramePool>) {
    let bootstrap_used = HEAP.early.used();
    HEAP.promote(pool);
    println!(
        "[kernel-qemu] dynamic heap ready bootstrap_used={}/{} owned_pages=0",
        bootstrap_used,
        HEAP.early.capacity()
    );
}

// AGENT: prove page-backed allocations of several sizes and alignments are
// returned to FramePool rather than consuming a fixed boot arena permanently.
pub fn smoke_check() {
    let (_, _, free_before) = HEAP.stats();
    {
        let mut values = Vec::new();
        values.push(1usize);
        values.push(2usize);

        let boxed = Box::new(41usize);
        let mut map = BTreeMap::new();
        map.insert("vec_len", values.len());
        map.insert("boxed", *boxed);
        let shared = Arc::new(values[0] + values[1] + map.len());

        let mut large = Vec::with_capacity(PAGE_SZ * 3 + 17);
        large.resize(PAGE_SZ * 3 + 17, 0x5a);

        let aligned_layout = Layout::from_size_align(73, PAGE_SZ * 2).unwrap();
        let aligned = unsafe { alloc::alloc::alloc(aligned_layout) };
        assert!(!aligned.is_null());
        assert_eq!(aligned as usize % (PAGE_SZ * 2), 0);

        let (live, owned, free_during) = HEAP.stats();
        println!(
            "[kernel-qemu] heap alloc smoke vec={} box={} map={} arc_strong={} large={} live={} owned_pages={} free_pages={}",
            values.len(),
            *boxed,
            map.len(),
            Arc::strong_count(&shared),
            large.len(),
            live,
            owned,
            free_during
        );
        assert_eq!(*shared, 5);
        assert!(live > 0 && owned > 0 && free_during < free_before);
        unsafe {
            alloc::alloc::dealloc(aligned, aligned_layout);
        }
    }

    let (live_after, owned_after, free_after) = HEAP.stats();
    assert_eq!(live_after, 0);
    assert_eq!(owned_after, 0);
    assert_eq!(free_after, free_before);
    println!(
        "[kernel-qemu] heap reclaim smoke passed free_pages={}",
        free_after
    );
}

#[alloc_error_handler]
// AGENT: retain layout details in the fatal no_std allocation diagnostic.
fn alloc_error(layout: Layout) -> ! {
    panic!(
        "kernel heap allocation failed: size={} align={}",
        layout.size(),
        layout.align()
    );
}

// AGENT: round one allocation up to the physical-page ownership granularity.
fn allocation_pages(layout: Layout) -> Option<usize> {
    layout
        .size()
        .max(1)
        .checked_add(PAGE_SZ - 1)
        .map(|size| size / PAGE_SZ)
}

// AGENT: align the bootstrap range end without overflow-prone addition.
const fn align_down(value: usize, align: usize) -> usize {
    value & !(align - 1)
}

// AGENT: align bootstrap allocation starts while rejecting address overflow.
fn align_up(value: usize, align: usize) -> Option<usize> {
    value.checked_add(align - 1).map(|v| v & !(align - 1))
}
