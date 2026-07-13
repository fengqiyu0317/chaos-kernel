// AGENT: keep bit-helper regressions next to the QEMU MM helper module and
// expose the same checks to the optional QEMU boot self-test path.
use super::*;
use crate::kernel::{FramePool, VmMap, VmRegion, MEM_OFF, PAGE_SZ, VM_READ, VM_WRITE};

// AGENT: run the new VMA boundary regression with the existing QEMU MM checks.
pub fn run_all() {
    align_up_rejects_overflow();
    rotate_bits_masks_zero_distance_rotation();
    hash_combine_mixes_zero_values();
    frame_pool_tracks_total_and_free_pages();
    frame_pool_bitmap_handles_partial_final_word();
    frame_pool_reclaims_dynamic_heap_pages();
    vm_region_and_map_preserve_range_semantics();
    buddy_allocator_alloc_free_smoke();
    buddy_free_merges_with_nonzero_base();
    buddy_free_rejects_duplicate_and_bad_ranges();
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

    assert_eq!(map.regions.len(), 1);
    assert!(map.find(base + PAGE_SZ).is_some());
    map.remove_range(base + PAGE_SZ, PAGE_SZ);
    assert_eq!(map.regions.len(), 2);
    assert_eq!(map.regions[0].checked_end(), Some(base + PAGE_SZ));
    assert_eq!(map.regions[1].base, base + 2 * PAGE_SZ);
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
