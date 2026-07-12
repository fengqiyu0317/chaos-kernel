// AGENT: isolate migrated heap-boundary helpers from physical-frame allocation.
use super::PAGE_SZ;

// AGENT: calculate the page-aligned end of the migrated heap interval.
pub fn heap_init(base: usize, sz: usize) -> usize {
    let aligned_base = (base + PAGE_SZ - 1) & !(PAGE_SZ - 1);
    let aligned_sz = sz & !(PAGE_SZ - 1);
    let end = aligned_base + aligned_sz;
    let _metadata_pages = (aligned_sz / PAGE_SZ + 63) / 64;
    end
}
