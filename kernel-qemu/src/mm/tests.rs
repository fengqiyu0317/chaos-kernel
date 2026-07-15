// AGENT: keep bit-helper regressions next to the QEMU MM helper module and
// expose the same checks to the optional QEMU boot self-test path.
use super::*;
use crate::kernel::{
    check_access, check_access_rw, hash_combine, BuddyAllocator, FramePool, VmMap, VmRegion,
    MEM_OFF, PAGE_SZ, USER_TOP, VM_READ, VM_WRITE,
};
use alloc::vec::Vec;
use core::sync::atomic::Ordering;

// AGENT: run the VMA boundary and checked-merge regressions with the existing
// QEMU MM checks.
pub fn run_all() {
    checked_align_up_rejects_invalid_results();
    bitwise_merge_replaces_only_masked_bits();
    rotate_bits_masks_zero_distance_rotation();
    hash_combine_mixes_zero_values();
    frame_pool_tracks_total_and_free_pages();
    frame_pool_bitmap_handles_partial_final_word();
    frame_pool_reclaims_dynamic_heap_pages();
    vm_region_and_map_preserve_range_semantics();
    vm_region_merge_rejects_invalid_endpoints();
    vm_map_insert_coalesces_both_neighbors();
    vm_map_insert_rejects_invalid_ranges_and_overlaps();
    user_range_checks_use_sv39_lower_half();
    vm_map_find_free_rejects_non_page_granular_lengths();
    buddy_allocator_alloc_free_smoke();
    buddy_free_merges_with_nonzero_base();
    buddy_free_rejects_duplicate_and_bad_ranges();
}

// AGENT: verify that VmRegion merging is directional, permission-preserving,
// and rejects an unrepresentable endpoint instead of publishing an invalid VMA.
#[cfg_attr(test, test)]
fn vm_region_merge_rejects_invalid_endpoints() {
    let base = 0x1800_0000;
    let left = VmRegion::new(base, PAGE_SZ, VM_READ);
    let right = VmRegion::new(base + PAGE_SZ, PAGE_SZ, VM_READ);
    let merged = left
        .merge_with(&right)
        .expect("adjacent regions with matching flags should merge");
    assert_eq!(merged.base, base);
    assert_eq!(merged.len, 2 * PAGE_SZ);
    assert_eq!(merged.flags, VM_READ);

    assert!(right.merge_with(&left).is_none());
    assert!(left
        .merge_with(&VmRegion::new(base + 2 * PAGE_SZ, PAGE_SZ, VM_READ))
        .is_none());
    assert!(left
        .merge_with(&VmRegion::new(base + PAGE_SZ, PAGE_SZ, VM_WRITE))
        .is_none());

    let high_base = usize::MAX - (3 * PAGE_SZ - 1);
    let high_left = VmRegion::new(high_base, PAGE_SZ, VM_READ);
    let overflowing_right = VmRegion::new(high_base + PAGE_SZ, 3 * PAGE_SZ, VM_READ);
    assert!(high_left.merge_with(&overflowing_right).is_none());
}

// AGENT: keep a boot-time regression at the new VmRegion/VmMap module boundary
// so the source split cannot silently change coalescing or range removal.
#[cfg_attr(test, test)]
fn vm_region_and_map_preserve_range_semantics() {
    let base = 0x1000_0000;
    let mut map = VmMap::new();

    map.insert(VmRegion::new(base, PAGE_SZ, VM_READ | VM_WRITE))
        .unwrap();
    map.insert(VmRegion::new(
        base + PAGE_SZ,
        2 * PAGE_SZ,
        VM_READ | VM_WRITE,
    ))
    .unwrap();

    let merged = map.find(base + PAGE_SZ).unwrap();
    assert_eq!(merged.base, base);
    assert_eq!(merged.len, 3 * PAGE_SZ);
    map.remove_range(base + PAGE_SZ, PAGE_SZ);
    let left = map.find(base).unwrap();
    assert_eq!(left.checked_end(), Some(base + PAGE_SZ));
    assert_eq!(left.len, PAGE_SZ);
    let right = map.find(base + 2 * PAGE_SZ).unwrap();
    assert_eq!(right.base, base + 2 * PAGE_SZ);
    assert_eq!(right.len, PAGE_SZ);
}

// AGENT: cover insertion before the first VMA and insertion that bridges two
// matching neighbors, preserving sorted and maximally coalesced metadata.
#[cfg_attr(test, test)]
fn vm_map_insert_coalesces_both_neighbors() {
    let base = 0x2000_0000;

    let mut prepend = VmMap::new();
    prepend
        .insert(VmRegion::new(base + PAGE_SZ, PAGE_SZ, VM_READ))
        .unwrap();
    prepend
        .insert(VmRegion::new(base, PAGE_SZ, VM_READ))
        .unwrap();
    let prepended = prepend.find(base + PAGE_SZ).unwrap();
    assert_eq!(prepended.base, base);
    assert_eq!(prepended.len, 2 * PAGE_SZ);

    let mut bridge = VmMap::new();
    bridge
        .insert(VmRegion::new(base, PAGE_SZ, VM_READ | VM_WRITE))
        .unwrap();
    bridge
        .insert(VmRegion::new(
            base + 2 * PAGE_SZ,
            PAGE_SZ,
            VM_READ | VM_WRITE,
        ))
        .unwrap();
    bridge
        .insert(VmRegion::new(base + PAGE_SZ, PAGE_SZ, VM_READ | VM_WRITE))
        .unwrap();
    let bridged = bridge.find(base + PAGE_SZ).unwrap();
    assert_eq!(bridged.base, base);
    assert_eq!(bridged.len, 3 * PAGE_SZ);
}

// AGENT: exercise every rejected range shape used by insert and both neighbor
// overlap checks, while retaining the legal half-open USER_TOP boundary.
#[cfg_attr(test, test)]
fn vm_map_insert_rejects_invalid_ranges_and_overlaps() {
    let base = 0x3000_0000;
    let mut map = VmMap::new();

    assert!(map.insert(VmRegion::new(base, 0, VM_READ)).is_err());
    assert!(map
        .insert(VmRegion::new(base + 1, PAGE_SZ, VM_READ))
        .is_err());
    assert!(map
        .insert(VmRegion::new(base, PAGE_SZ + 1, VM_READ))
        .is_err());

    let last_page = usize::MAX - (PAGE_SZ - 1);
    assert!(map
        .insert(VmRegion::new(last_page, PAGE_SZ, VM_READ))
        .is_err());
    assert!(map
        .insert(VmRegion::new(USER_TOP, PAGE_SZ, VM_READ))
        .is_err());

    map.insert(VmRegion::new(base + PAGE_SZ, 2 * PAGE_SZ, VM_READ))
        .unwrap();
    assert!(map
        .insert(VmRegion::new(base, 2 * PAGE_SZ, VM_WRITE))
        .is_err());
    assert!(map
        .insert(VmRegion::new(base + 2 * PAGE_SZ, 2 * PAGE_SZ, VM_WRITE))
        .is_err());

    let mut boundary = VmMap::new();
    boundary
        .insert(VmRegion::new(USER_TOP - PAGE_SZ, PAGE_SZ, VM_READ))
        .unwrap();
    assert!(boundary.find(USER_TOP - 1).is_some());
}

// AGENT: enforce the shared exclusive USER_TOP boundary in coarse user-access
// guards, including the legal final page and the first byte above it.
#[cfg_attr(test, test)]
fn user_range_checks_use_sv39_lower_half() {
    assert!(check_access(USER_TOP - PAGE_SZ, PAGE_SZ));
    assert!(check_access_rw(USER_TOP - PAGE_SZ, PAGE_SZ, true));
    assert!(!check_access(USER_TOP - PAGE_SZ, PAGE_SZ + 1));
    assert!(!check_access_rw(USER_TOP - PAGE_SZ, PAGE_SZ + 1, true));
    assert!(!check_access(USER_TOP, 1));
}

// AGENT: keep free-range selection consistent with VmMap::insert so it never
// returns a candidate for zero-length or non-page-granular VMA metadata.
#[cfg_attr(test, test)]
fn vm_map_find_free_rejects_non_page_granular_lengths() {
    let map = VmMap::new();

    assert_eq!(map.find_free(0, PAGE_SZ), None);
    assert_eq!(map.find_free(PAGE_SZ - 1, PAGE_SZ), None);
    assert_eq!(map.find_free(PAGE_SZ + 1, PAGE_SZ), None);
    assert_eq!(map.find_free(PAGE_SZ, PAGE_SZ), Some(0x7000_0000));
}

#[cfg_attr(test, test)]
// AGENT: checked alignment must distinguish valid results from overflow and
// invalid alignment instead of returning an ambiguous input value.
fn checked_align_up_rejects_invalid_results() {
    assert_eq!(checked_align_up(0x1003, PAGE_SZ), Some(0x2000));
    assert_eq!(checked_align_up(usize::MAX, PAGE_SZ), None);
    assert_eq!(checked_align_up(0x1000, 3), None);
}

// AGENT: retain the migrated masked-field helper with an executable contract
// showing that unmasked PTE/register-style bits remain unchanged.
#[cfg_attr(test, test)]
fn bitwise_merge_replaces_only_masked_bits() {
    assert_eq!(
        bitwise_merge(0b1010_0000, 0b0000_0101, 0b0000_1111),
        0b1010_0101
    );
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

// AGENT: FramePool retains allocated pages through RAII handles and rolls
// incomplete batches back without needing a separate permanent reservation API.
#[cfg_attr(test, test)]
fn frame_pool_tracks_total_and_free_pages() {
    let pool = FramePool::new(8, MEM_OFF);
    let boot_frames = pool.alloc_pg_frames(2).unwrap();
    let boot_frame_ids = boot_frames
        .iter()
        .map(|frame| frame.id())
        .collect::<Vec<_>>();
    assert_eq!(boot_frame_ids.as_slice(), &[0, 1]);

    assert_eq!(pool.total_pages(), 8);
    assert_eq!(pool.free_count(), 6);
    assert!(pool.get_pg_frame(1).is_none());

    let frame = pool.get_pg_frame(3).unwrap();
    assert_eq!(frame.id(), 3);
    assert_eq!(pool.free_count(), 5);
    drop(frame);
    assert_eq!(pool.free_count(), 6);

    assert!(pool.alloc_pg_frames(7).is_none());
    assert_eq!(pool.free_count(), 6);

    let frames = pool.alloc_pg_frames(3).unwrap();
    let frame_ids = frames.iter().map(|frame| frame.id()).collect::<Vec<_>>();
    assert_eq!(frame_ids.as_slice(), &[2, 3, 4]);
    assert_eq!(pool.free_count(), 3);
    drop(frames);
    assert_eq!(pool.free_count(), 6);
    drop(boot_frames);
    assert_eq!(pool.free_count(), 8);
}

// AGENT: a fixed physical-frame bitmap must expose exactly `cap` frames even
// when its final machine word contains unused tail bits.
#[cfg_attr(test, test)]
fn frame_pool_bitmap_handles_partial_final_word() {
    let pages = usize::BITS as usize + 1;
    let pool = FramePool::new(pages, MEM_OFF);
    let frames = pool.alloc_pg_frames(pages).unwrap();

    assert_eq!(frames.first().map(|frame| frame.id()), Some(0));
    assert_eq!(frames.last().map(|frame| frame.id()), Some(pages - 1));
    assert_eq!(pool.free_count(), 0);
    assert!(pool.alloc_pg_frame().is_none());

    drop(frames);
    assert_eq!(pool.free_count(), pages);
}

// AGENT: dynamic heap spans use the shared frame state and return their complete
// ownership range instead of permanently reserving one boot arena.
#[cfg_attr(test, test)]
fn frame_pool_reclaims_dynamic_heap_pages() {
    let pool = FramePool::new(8, MEM_OFF);
    let heap = pool.alloc_contiguous_pages(2, 2).unwrap();
    assert_eq!(heap, MEM_OFF);
    assert_eq!(pool.free_count(), 6);
    assert!(pool.get_pg_frame(0).is_none());
    assert!(pool.get_pg_frame(1).is_none());
    assert!(pool.release_contiguous_pages(heap, 2));
    assert_eq!(pool.free_count(), 8);

    let frame = pool.get_pg_frame(0).unwrap();
    assert_eq!(pool.free_count(), 7);
    drop(frame);
    assert_eq!(pool.free_count(), 8);

    let heap = pool.alloc_contiguous_pages(2, 2).unwrap();
    let frame = pool.get_pg_frame(2).unwrap();
    assert_eq!(pool.free_count(), 5);
    drop(frame);
    assert_eq!(pool.free_count(), 6);
    assert!(pool.release_contiguous_pages(heap, 2));
    assert_eq!(pool.free_count(), 8);
}

// AGENT: keep the buddy smoke check in the MM helper tests and call it from
// rust_main only when the explicit QEMU self-test feature is enabled.
#[cfg_attr(test, test)]
fn buddy_allocator_alloc_free_smoke() {
    let base = checked_align_up(0x8021_8123, PAGE_SZ).unwrap();
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
    assert_eq!(second, base + PAGE_SZ);
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
        alloc.free_order(base + PAGE_SZ / 2, 0),
        Err("unaligned address")
    );
    assert_eq!(
        alloc.free_order(base + 4 * PAGE_SZ, 0),
        Err("address outside managed range")
    );
}
