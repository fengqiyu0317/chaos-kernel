// AGENT: isolate migrated heap-boundary helpers from physical-frame allocation.
use super::{align_down, checked_align_up, PAGE_SZ};

// AGENT: calculate the page-aligned end with the shared checked helpers and
// reject an impossible migrated heap interval instead of wrapping addresses.
pub fn heap_init(base: usize, sz: usize) -> Option<usize> {
    let aligned_base = checked_align_up(base, PAGE_SZ)?;
    let aligned_sz = align_down(sz, PAGE_SZ);
    let end = aligned_base.checked_add(aligned_sz)?;
    let _metadata_pages = (aligned_sz / PAGE_SZ + 63) / 64;
    Some(end)
}
