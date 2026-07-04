// AGENT: fd-focused regressions split out from fd.rs while retaining the same
// module path for Rust tests and qemu-sync-selftest.
use super::*;

// AGENT: keep the QEMU boot selftest aggregator in the moved fd test module.
pub fn run_all() {
    set_len_tracks_byte_length_and_block_capacity();
    truncate_releases_blocks_for_reuse_without_old_contents();
    fallocate_validates_and_only_grows_regular_files();
    lookup_reports_node_local_errors();
    regular_file_poll_and_ioctl_are_explicit();
    splice_checks_permissions_before_moving_offsets();
    splice_uses_shared_append_status();
}

// AGENT: shared writable regular-file option used by moved fd regressions.
fn writable_opt() -> FdOpt {
    FdOpt {
        rd: true,
        wr: true,
        ap: false,
        nb: false,
    }
}

// AGENT: build a fd entry around a regular-file object with explicit
// open-description state; FHandle no longer carries fd status or offset.
fn file_entry(fh: &FHandle, opt: FdOpt) -> FdEntry {
    FdEntry::with_status(FLike::File(fh.clone()), opt, false)
}

// AGENT: observe a regular-file byte range through the public read path while
// keeping tests independent from block-rounded allocation length.
fn file_bytes(fh: &FHandle, off: usize, len: usize) -> Vec<u8> {
    let mut data = vec![0; len];
    fh.read_at(off, &mut data).unwrap();
    data
}

// AGENT: FileNode owns byte-precise EOF while still rounding allocation up to
// whole backend blocks.
#[cfg_attr(test, test)]
fn set_len_tracks_byte_length_and_block_capacity() {
    let fh = FHandle::with_data("/tmp/file", vec![1, 2, 3]);
    let entry = file_entry(&fh, writable_opt());
    assert_eq!(fh.len(), 3);
    assert_eq!(fh.node.allocated_len(), BLOCK_CACHE_BLOCK_SIZE);

    fh.write_at(1, &[9]).unwrap();
    assert_eq!(file_bytes(&fh, 0, 3).as_slice(), &[1, 9, 3]);
    assert_eq!(fh.len(), 3);

    entry.set_len(5).unwrap();
    assert_eq!(fh.len(), 5);
    assert_eq!(fh.node.allocated_len(), BLOCK_CACHE_BLOCK_SIZE);

    entry.set_len((BLOCK_CACHE_BLOCK_SIZE + 1) as u64).unwrap();
    assert_eq!(fh.len(), BLOCK_CACHE_BLOCK_SIZE + 1);
    assert_eq!(fh.node.allocated_len(), BLOCK_CACHE_BLOCK_SIZE * 2);

    let ro = FHandle::with_data("/tmp/ro", vec![1, 2, 3]);
    let ro_entry = file_entry(&ro, FdOpt::default());
    assert_eq!(ro_entry.set_len(0), Err("ebadf"));
}

// AGENT: truncation must return cleared blocks to the shared FileStorage
// allocator so later files can reuse space without observing stale contents.
#[cfg_attr(test, test)]
fn truncate_releases_blocks_for_reuse_without_old_contents() {
    let storage = FileStorage::new(
        Arc::new(BlockCache::new(1)),
        Arc::new(RamBlockDevice::empty()),
        Arc::new(FileBlockAllocator::new()),
    );
    let first_node = Arc::new(FileNode::regular(false));
    let first = FHandle::with_node_on_storage("/tmp/first", first_node, storage.clone());
    let first_entry = file_entry(&first, writable_opt());
    first
        .write_at(0, &vec![0x5a; BLOCK_CACHE_BLOCK_SIZE * 2])
        .unwrap();
    assert_eq!(storage.allocator_stats(), (3, 0));

    first_entry.set_len(0).unwrap();
    assert_eq!(first.len(), 0);
    assert_eq!(first.node.allocated_len(), 0);
    assert_eq!(storage.allocator_stats(), (3, 2));

    let second_node = Arc::new(FileNode::regular(false));
    let second = FHandle::with_node_on_storage("/tmp/second", second_node, storage.clone());
    let second_entry = file_entry(&second, writable_opt());
    second_entry.fallocate(0, BLOCK_CACHE_BLOCK_SIZE).unwrap();
    assert_eq!(storage.allocator_stats(), (3, 0));

    let reused = file_bytes(&second, 0, BLOCK_CACHE_BLOCK_SIZE);
    assert!(reused.iter().all(|&byte| byte == 0));
}

// AGENT: fallocate grows visible EOF exactly while allocation stays block-rounded.
#[cfg_attr(test, test)]
fn fallocate_validates_and_only_grows_regular_files() {
    let fh = FHandle::with_data("/tmp/file", vec![1, 2, 3]);
    let entry = file_entry(&fh, writable_opt());

    entry.fallocate(BLOCK_CACHE_BLOCK_SIZE + 5, 2).unwrap();
    assert_eq!(fh.len(), BLOCK_CACHE_BLOCK_SIZE + 7);
    assert_eq!(fh.node.allocated_len(), BLOCK_CACHE_BLOCK_SIZE * 2);
    assert_eq!(file_bytes(&fh, 0, 3).as_slice(), &[1, 2, 3]);

    entry.fallocate(1, 1).unwrap();
    assert_eq!(fh.len(), BLOCK_CACHE_BLOCK_SIZE + 7);
    assert_eq!(fh.node.allocated_len(), BLOCK_CACHE_BLOCK_SIZE * 2);
    assert_eq!(file_bytes(&fh, 0, 3).as_slice(), &[1, 2, 3]);

    assert_eq!(entry.fallocate(0, 0), Err("einval"));
    assert_eq!(entry.fallocate(usize::MAX, 1), Err("efbig"));

    let ro = FHandle::with_data("/tmp/ro", vec![1, 2, 3]);
    let ro_entry = file_entry(&ro, FdOpt::default());
    assert_eq!(ro_entry.fallocate(0, 1), Err("ebadf"));

    let dir = FHandle::with_node("/tmp", Arc::new(FileNode::directory()));
    let dir_entry = file_entry(&dir, writable_opt());
    assert_eq!(dir_entry.fallocate(0, 1), Err("enodev"));
}

// AGENT: moved node-local lookup regression out of fd.rs without changing behavior.
#[cfg_attr(test, test)]
fn lookup_reports_node_local_errors() {
    let file = FHandle::with_data("/tmp/file", Vec::new());
    assert_eq!(file.lookup(".", 0), Err("enotdir"));

    let dir = FHandle::with_node("/tmp", Arc::new(FileNode::directory()));
    assert_eq!(dir.lookup(".", 0), Ok(()));
    assert_eq!(dir.lookup("", 0), Ok(()));
    dir.node.add_dir_entry(&dir.storage, "child").unwrap();
    assert_eq!(dir.lookup("child", 0), Ok(()));
    assert_eq!(dir.lookup("missing", 0), Err("enoent"));
    assert_eq!(dir.lookup(".", 41), Err("eloop"));
    assert_eq!(dir.lookup("bad\0name", 0), Err("einval"));
    assert_eq!(dir.lookup("bad/name", 0), Err("einval"));
}

// AGENT: regular-file ioctl observes FileNode's byte-precise visible length.
#[cfg_attr(test, test)]
fn regular_file_poll_and_ioctl_are_explicit() {
    let file = FHandle::with_data("", vec![1, 2, 3, 4]);
    let entry = file_entry(&file, writable_opt());
    let poll = entry.poll();
    assert!(poll.readable);
    assert!(poll.writable);
    assert!(!poll.error);

    assert_eq!(entry.io_ctl(FIONREAD, 0), Ok(4));
    let mut buf = [0; 2];
    assert_eq!(entry.read(&mut buf), Ok(2));
    assert_eq!(entry.io_ctl(TIOCINQ, 0), Ok(2));
    assert_eq!(entry.io_ctl(0xDEAD, 0), Err("enotty"));
}

// AGENT: moved splice permission regression out of fd.rs unchanged.
#[cfg_attr(test, test)]
fn splice_checks_permissions_before_moving_offsets() {
    let src = FHandle::with_data("/src", vec![1, 2, 3]);
    let dst = FHandle::with_data("/dst", Vec::new());
    let src_entry = file_entry(&src, FdOpt::default());
    let dst_entry = file_entry(&dst, FdOpt::default());

    assert_eq!(src_entry.splice_to(&dst_entry, 2), Err("ebadf"));
    assert_eq!(src_entry.offset(), 0);
    assert_eq!(dst_entry.offset(), 0);
    assert_eq!(dst.len(), 0);

    let unreadable = FdOpt {
        rd: false,
        wr: true,
        ap: false,
        nb: false,
    };
    let src = FHandle::with_data("/src", vec![1, 2, 3]);
    let dst = FHandle::with_data("/dst", Vec::new());
    let src_entry = file_entry(&src, unreadable);
    let dst_entry = file_entry(&dst, writable_opt());

    assert_eq!(src_entry.splice_to(&dst_entry, 2), Err("ebadf"));
    assert_eq!(src_entry.offset(), 0);
    assert_eq!(dst.len(), 0);
}

// AGENT: append-status splice appends at the byte-precise FileNode EOF.
#[cfg_attr(test, test)]
fn splice_uses_shared_append_status() {
    let src = FHandle::with_data("/src", vec![1, 2, 3]);
    let dst = FHandle::with_data("/dst", vec![9, 9]);
    let src_entry = file_entry(&src, FdOpt::default());
    let dst_entry = file_entry(&dst, writable_opt());
    dst_entry.seek(FSeek::Start(1)).unwrap();

    dst_entry.set_status_flags(O_APPEND).unwrap();
    assert_eq!(src_entry.splice_to(&dst_entry, 2), Ok(2));
    assert_eq!(src_entry.offset(), 2);
    assert_eq!(dst_entry.offset(), 4);
    assert_eq!(file_bytes(&dst, 0, 4).as_slice(), &[9, 9, 1, 2]);
}
