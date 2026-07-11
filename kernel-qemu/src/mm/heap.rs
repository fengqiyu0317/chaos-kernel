// AGENT: isolate migrated heap-boundary helpers from physical-frame allocation.
use alloc::vec::Vec;

use super::{p2v, FramePool, PAGE_SZ};

// AGENT: calculate the page-aligned end of the migrated heap interval.
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
