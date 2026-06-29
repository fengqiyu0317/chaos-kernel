// AGENT: provide the early QEMU global heap that backs alloc types before the
// migrated FramePool/Sv39 path owns real user and page-table frames.
use alloc::boxed::Box;
use alloc::collections::BTreeMap;
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::alloc::{GlobalAlloc, Layout};
use core::ptr::null_mut;
use core::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use crate::println;

const HEAP_ALIGN: usize = 16;

unsafe extern "C" {
    static mut sheap: u8;
    static mut eheap: u8;
}

// AGENT: monotonic allocator for early no_std alloc smoke and migrated metadata.
// It intentionally does not implement frame allocation or user-page ownership.
struct EarlyHeap {
    start: AtomicUsize,
    next: AtomicUsize,
    end: AtomicUsize,
    ready: AtomicBool,
}

impl EarlyHeap {
    const fn new() -> Self {
        Self {
            start: AtomicUsize::new(0),
            next: AtomicUsize::new(0),
            end: AtomicUsize::new(0),
            ready: AtomicBool::new(false),
        }
    }

    fn init(&self, base: usize, size: usize) -> usize {
        let start = align_up(base, HEAP_ALIGN);
        let raw_end = base.checked_add(size).expect("early heap range overflow");
        let end = align_down(raw_end, HEAP_ALIGN);
        assert!(start <= end, "early heap range is empty");

        self.start.store(start, Ordering::Relaxed);
        self.next.store(start, Ordering::Relaxed);
        self.end.store(end, Ordering::Relaxed);
        self.ready.store(true, Ordering::Release);
        end
    }

    fn alloc_inner(&self, layout: Layout) -> *mut u8 {
        if !self.ready.load(Ordering::Acquire) {
            return null_mut();
        }

        let align = layout.align().max(HEAP_ALIGN);
        let size = layout.size();
        let heap_end = self.end.load(Ordering::Acquire);
        let mut cur = self.next.load(Ordering::Relaxed);

        loop {
            let alloc_start = align_up(cur, align);
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

    fn used(&self) -> usize {
        self.next
            .load(Ordering::Relaxed)
            .saturating_sub(self.start.load(Ordering::Relaxed))
    }

    fn capacity(&self) -> usize {
        self.end
            .load(Ordering::Relaxed)
            .saturating_sub(self.start.load(Ordering::Relaxed))
    }
}

unsafe impl Sync for EarlyHeap {}

unsafe impl GlobalAlloc for EarlyHeap {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        self.alloc_inner(layout)
    }

    unsafe fn dealloc(&self, _ptr: *mut u8, _layout: Layout) {
        // AGENT: early bump allocations are not reclaimed; later frame/Sv39 work
        // must provide real page ownership instead of extending this allocator.
    }
}

#[global_allocator]
static HEAP: EarlyHeap = EarlyHeap::new();

// AGENT: keep the heap_init naming used by the migrated mm::alloc baseline while
// binding it to the QEMU global allocator range.
pub fn heap_init(base: usize, size: usize) -> usize {
    HEAP.init(base, size)
}

// AGENT: initialize the fixed linker-provided early heap before any alloc types run.
pub fn init() {
    let base = core::ptr::addr_of_mut!(sheap) as usize;
    let end = core::ptr::addr_of_mut!(eheap) as usize;
    let size = end.saturating_sub(base);
    let heap_end = heap_init(base, size);
    println!(
        "[kernel-qemu] heap ready base={:#x} end={:#x} bytes={}",
        base, heap_end, size
    );
}

// AGENT: exercise alloc::Vec, Box, BTreeMap and Arc so QEMU smoke proves the
// early global allocator is actually carrying alloc crate types.
pub fn smoke_check() {
    let mut values = Vec::new();
    values.push(1usize);
    values.push(2usize);

    let boxed = Box::new(41usize);
    let mut map = BTreeMap::new();
    map.insert("vec_len", values.len());
    map.insert("boxed", *boxed);

    let shared = Arc::new(values[0] + values[1] + map.len());
    println!(
        "[kernel-qemu] heap alloc smoke vec={} box={} map={} arc_strong={} used={}/{}",
        values.len(),
        *boxed,
        map.len(),
        Arc::strong_count(&shared),
        HEAP.used(),
        HEAP.capacity()
    );
    assert_eq!(*shared, 5);
}

#[alloc_error_handler]
fn alloc_error(layout: Layout) -> ! {
    panic!(
        "early heap allocation failed: size={} align={}",
        layout.size(),
        layout.align()
    );
}

const fn align_up(value: usize, align: usize) -> usize {
    (value + align - 1) & !(align - 1)
}

const fn align_down(value: usize, align: usize) -> usize {
    value & !(align - 1)
}
