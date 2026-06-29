// AGENT: keep bit-helper regressions next to the QEMU MM helper module and
// expose the same checks to the optional QEMU boot self-test path.
use super::*;

pub fn run_all() {
    align_up_rejects_overflow();
    rotate_bits_masks_zero_distance_rotation();
    hash_combine_mixes_zero_values();
    buddy_allocator_alloc_free_smoke();
    buddy_free_merges_with_nonzero_base();
    buddy_free_rejects_duplicate_and_bad_ranges();
}

#[cfg_attr(test, test)]
fn align_up_rejects_overflow() {
    assert_eq!(align_up(0x1003, PAGE_SIZE), 0x2000);
    assert_eq!(align_up(usize::MAX, PAGE_SIZE), usize::MAX);
    assert_eq!(align_up(0x1000, 3), 0x1000);
}

// AGENT: rotate helpers must not leak bits outside the requested field.
#[cfg_attr(test, test)]
fn rotate_bits_masks_zero_distance_rotation() {
    assert_eq!(rotate_bits(0x1234, 0, 8), 0x34);
    assert_eq!(rotate_bits(0x1234, 8, 8), 0x34);
    assert_eq!(rotate_bits(0b1011, 1, 4), 0b0111);
}

// AGENT: zero-valued fields must still change a combined hash seed.
#[cfg_attr(test, test)]
fn hash_combine_mixes_zero_values() {
    assert_eq!(hash_combine(0, 0), 0x9e3779b97f4a7c15);
    assert_ne!(hash_combine(hash_combine(0, 0), 1), hash_combine(0, 1));
}

// AGENT: keep the buddy smoke check in the MM helper tests and call it from
// rust_main only when the explicit QEMU self-test feature is enabled.
#[cfg_attr(test, test)]
fn buddy_allocator_alloc_free_smoke() {
    let base = align_up(0x8021_8123, PAGE_SIZE);
    let mut alloc = BuddyAllocator::new(base, 4, 2);
    let frame = alloc.alloc_order(0).unwrap();

    assert_eq!(frame, base);
    assert_eq!(alloc.free_order(frame, 0), Ok(()));
    assert_eq!(alloc.free_pages_count(), 4);
    assert_eq!(alloc.largest_free_order(), Some(2));
}

#[cfg_attr(test, test)]
fn buddy_free_merges_with_nonzero_base() {
    let base = 0x8020_0000;
    let mut alloc = BuddyAllocator::new(base, 4, 2);
    let first = alloc.alloc_order(0).unwrap();
    let second = alloc.alloc_order(0).unwrap();

    assert_eq!(first, base);
    assert_eq!(second, base + PAGE_SIZE);
    assert_eq!(alloc.free_order(first, 0), Ok(()));
    assert_eq!(alloc.free_order(second, 0), Ok(()));
    assert!(alloc.free_lists[2].contains(&base));
    assert_eq!(alloc.free_pages_count(), 4);
    assert_eq!(alloc.allocated.load(Ordering::Relaxed), 0);
}

#[cfg_attr(test, test)]
fn buddy_free_rejects_duplicate_and_bad_ranges() {
    let base = 0x8020_0000;
    let mut alloc = BuddyAllocator::new(base, 4, 2);
    let block = alloc.alloc_order(1).unwrap();

    assert_eq!(alloc.free_order(block, 1), Ok(()));
    assert_eq!(alloc.free_order(block, 1), Err("double free"));
    assert_eq!(
        alloc.free_order(base + PAGE_SIZE / 2, 0),
        Err("unaligned address")
    );
    assert_eq!(
        alloc.free_order(base + 4 * PAGE_SIZE, 0),
        Err("address outside managed range")
    );
}
