// AGENT: fd-focused regressions split out from fd.rs while retaining the same
// module path for Rust tests and qemu-sync-selftest.
use super::*;

// AGENT: make direct FsInstance test fixtures cross the same validated-name
// boundary as VFS-produced child components.
fn child_name(name: &str) -> ChildName<'_> {
    ChildName::new(name).expect("test child name should be one ordinary component")
}

// AGENT: keep the QEMU boot selftest aggregator in the moved fd test module.
pub fn run_all() {
    circ_buf_peek_and_discard_preserve_wrapped_fifo_order();
    set_len_tracks_byte_length_and_block_capacity();
    metadata_blocks_expand_for_large_regular_file();
    metadata_blocks_expand_for_large_directory_entry();
    truncate_releases_blocks_for_reuse_without_old_contents();
    fallocate_validates_and_only_grows_regular_files();
    read_entry_uses_open_description_offset();
    regular_file_poll_and_ioctl_are_explicit();
    terminal_is_a_typed_nonseekable_fd_object();
    typed_regular_file_stays_a_regular_file();
}

// AGENT: pin the non-consuming peek and later discard contract used to keep
// pipe-to-file splice failure-atomic across a wrapped ring buffer.
#[cfg_attr(test, test)]
fn circ_buf_peek_and_discard_preserve_wrapped_fifo_order() {
    let mut buf = CircBuf::new(4);
    assert_eq!(buf.fill_from(b"abcd"), 4);
    assert_eq!(buf.pop(), Some(b'a'));
    assert_eq!(buf.pop(), Some(b'b'));
    assert_eq!(buf.fill_from(b"ef"), 2);

    let mut peeked = Vec::new();
    assert_eq!(buf.peek_to(&mut peeked, 3), 3);
    assert_eq!(&peeked, b"cde");
    assert_eq!(buf.len(), 4);
    assert_eq!(buf.discard(3), 3);
    let mut remaining = Vec::new();
    assert_eq!(buf.drain_to(&mut remaining, 4), 1);
    assert_eq!(&remaining, b"f");
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

// AGENT: construct an isolated managed regular file for fd-only regressions
// without exposing fixture creation as part of FInstance's production API.
fn regular_file(data: Vec<u8>) -> FInstance {
    let fs = FsInstance::new(0, FileStorage::standalone());
    let node = fs
        .install_regular_at(&fs.root(), child_name("file"), &data, false)
        .expect("standalone RAM file seed should fit");
    let mount = MountTable::new(fs).root();
    FInstance::new(mount, node)
}

// AGENT: construct an isolated managed directory locally for fd regressions.
fn directory_file() -> FInstance {
    let fs = FsInstance::new(0, FileStorage::standalone());
    let node = fs
        .install_directory_at(&fs.root(), child_name("dir"))
        .expect("standalone directory should install");
    let mount = MountTable::new(fs).root();
    FInstance::new(mount, node)
}

// AGENT: build a fd entry around a regular-file handle with explicit
// open-description status; FInstance remains only the backing object.
fn file_entry(instance: &FInstance, opt: FdOpt) -> FdEntry {
    FdEntry::with_status(FLike::File(FHandle::new(instance.clone())), opt, false)
}

// AGENT: observe a regular-file byte range through the public read path while
// keeping tests independent from block-rounded allocation length.
fn file_bytes(instance: &FInstance, off: usize, len: usize) -> Vec<u8> {
    let mut data = vec![0; len];
    instance.read_at(off, &mut data).unwrap();
    data
}

// AGENT: FileNode owns byte-precise EOF while still rounding allocation up to
// whole backend blocks.
#[cfg_attr(test, test)]
fn set_len_tracks_byte_length_and_block_capacity() {
    let instance = regular_file(vec![1, 2, 3]);
    let entry = file_entry(&instance, writable_opt());
    assert_eq!(instance.len(), 3);
    assert_eq!(instance.node.allocated_len(), BLOCK_CACHE_BLOCK_SIZE);

    instance.write_at(1, &[9]).unwrap();
    assert_eq!(file_bytes(&instance, 0, 3).as_slice(), &[1, 9, 3]);
    assert_eq!(instance.len(), 3);

    entry.set_len(5).unwrap();
    assert_eq!(instance.len(), 5);
    assert_eq!(instance.node.allocated_len(), BLOCK_CACHE_BLOCK_SIZE);

    entry.set_len((BLOCK_CACHE_BLOCK_SIZE + 1) as u64).unwrap();
    assert_eq!(instance.len(), BLOCK_CACHE_BLOCK_SIZE + 1);
    assert_eq!(instance.node.allocated_len(), BLOCK_CACHE_BLOCK_SIZE * 2);

    let ro = regular_file(vec![1, 2, 3]);
    let ro_entry = file_entry(&ro, FdOpt::default());
    assert_eq!(ro_entry.set_len(0), Err("ebadf"));
}

// AGENT: regular-file metadata must grow past one backend block when the data
// block id list no longer fits in the first metadata payload block.
#[cfg_attr(test, test)]
fn metadata_blocks_expand_for_large_regular_file() {
    let instance = regular_file(Vec::new());
    let data = vec![0x7a; BLOCK_CACHE_BLOCK_SIZE * 61];

    instance.write_at(0, &data).unwrap();

    assert_eq!(instance.len(), data.len());
    assert_eq!(instance.node.allocated_len(), data.len());
    assert_eq!(instance.node.metadata_block_count(), 2);
}

// AGENT: directory metadata uses the same multi-block payload path as regular
// files, with entry names contributing to metadata size.
#[cfg_attr(test, test)]
fn metadata_blocks_expand_for_large_directory_entry() {
    let dir = directory_file();
    let mut name = String::new();
    for _ in 0..BLOCK_CACHE_BLOCK_SIZE {
        name.push('x');
    }

    let fs = dir.mount.fs();
    fs.create_regular_at(&dir.node, child_name(&name), false)
        .unwrap();

    assert_eq!(dir.node.metadata_block_count(), 2);
    assert_eq!(dir.read_entry(0), Ok(name));
}

// AGENT: truncation must return cleared blocks to the shared FileStorage
// allocator so later files can reuse space without observing stale contents.
#[cfg_attr(test, test)]
fn truncate_releases_blocks_for_reuse_without_old_contents() {
    let device = Arc::new(RamBlockDevice::empty());
    let storage = FileStorage::new(
        Arc::new(BlockCache::new(1)),
        device.clone(),
        Arc::new(FileBlockAllocator::new(device.block_count())),
    );
    let fs = FsInstance::new(0, storage.clone());
    let mount = MountTable::new(fs.clone()).root();
    let first_node = fs
        .create_regular_at(&fs.root(), child_name("first"), false)
        .unwrap();
    let first = FInstance::new(mount.clone(), first_node);
    let first_entry = file_entry(&first, writable_opt());
    first
        .write_at(0, &vec![0x5a; BLOCK_CACHE_BLOCK_SIZE * 2])
        .unwrap();
    assert_eq!(storage.allocator_stats(), (4, 0));

    first_entry.set_len(0).unwrap();
    assert_eq!(first.len(), 0);
    assert_eq!(first.node.allocated_len(), 0);
    assert_eq!(storage.allocator_stats(), (4, 2));

    let second_node = fs
        .create_regular_at(&fs.root(), child_name("second"), false)
        .unwrap();
    let second = FInstance::new(mount, second_node);
    let second_entry = file_entry(&second, writable_opt());
    second_entry.fallocate(0, BLOCK_CACHE_BLOCK_SIZE).unwrap();
    assert_eq!(storage.allocator_stats(), (4, 0));

    let reused = file_bytes(&second, 0, BLOCK_CACHE_BLOCK_SIZE);
    assert!(reused.iter().all(|&byte| byte == 0));
}

// AGENT: fallocate grows visible EOF exactly while allocation stays block-rounded.
#[cfg_attr(test, test)]
fn fallocate_validates_and_only_grows_regular_files() {
    let instance = regular_file(vec![1, 2, 3]);
    let entry = file_entry(&instance, writable_opt());

    entry.fallocate(BLOCK_CACHE_BLOCK_SIZE + 5, 2).unwrap();
    assert_eq!(instance.len(), BLOCK_CACHE_BLOCK_SIZE + 7);
    assert_eq!(instance.node.allocated_len(), BLOCK_CACHE_BLOCK_SIZE * 2);
    assert_eq!(file_bytes(&instance, 0, 3).as_slice(), &[1, 2, 3]);

    entry.fallocate(1, 1).unwrap();
    assert_eq!(instance.len(), BLOCK_CACHE_BLOCK_SIZE + 7);
    assert_eq!(instance.node.allocated_len(), BLOCK_CACHE_BLOCK_SIZE * 2);
    assert_eq!(file_bytes(&instance, 0, 3).as_slice(), &[1, 2, 3]);

    assert_eq!(entry.fallocate(0, 0), Err("einval"));
    assert_eq!(entry.fallocate(usize::MAX, 1), Err("efbig"));

    let ro = regular_file(vec![1, 2, 3]);
    let ro_entry = file_entry(&ro, FdOpt::default());
    assert_eq!(ro_entry.fallocate(0, 1), Err("ebadf"));

    let dir = directory_file();
    let dir_entry = file_entry(&dir, writable_opt());
    assert_eq!(dir_entry.fallocate(0, 1), Err("enodev"));
}

// AGENT: directory entry reads advance the shared FHandle offset, while direct
// FInstance reads remain explicit-index helpers.
#[cfg_attr(test, test)]
fn read_entry_uses_open_description_offset() {
    let dir = directory_file();
    let fs = dir.mount.fs();
    fs.create_regular_at(&dir.node, child_name("alpha"), false)
        .unwrap();
    fs.create_regular_at(&dir.node, child_name("beta"), false)
        .unwrap();
    fs.create_regular_at(&dir.node, child_name("gamma"), false)
        .unwrap();

    assert_eq!(dir.read_entry(1), Ok(String::from("beta")));

    let entry = file_entry(&dir, FdOpt::default());
    assert_eq!(entry.read_entry(), Ok(String::from("alpha")));
    assert_eq!(entry.offset(), 1);

    let dup = entry.dup(false);
    assert_eq!(dup.read_entry(), Ok(String::from("beta")));
    assert_eq!(entry.read_entry(), Ok(String::from("gamma")));
    assert_eq!(entry.offset(), 3);
    assert_eq!(dup.offset(), 3);

    assert_eq!(entry.read_entry(), Err("enoent"));
    assert_eq!(entry.offset(), 3);

    let unreadable = file_entry(
        &dir,
        FdOpt {
            rd: false,
            wr: true,
            ap: false,
            nb: false,
        },
    );
    assert_eq!(unreadable.read_entry(), Err("ebadf"));
    assert_eq!(unreadable.offset(), 0);

    let file = regular_file(Vec::new());
    let file_entry = file_entry(&file, FdOpt::default());
    assert_eq!(file_entry.read_entry(), Err("enotdir"));
}

// AGENT: regular-file poll follows open-description access flags, while ioctl
// observes byte-precise EOF and rejects directory objects.
#[cfg_attr(test, test)]
fn regular_file_poll_and_ioctl_are_explicit() {
    let file = regular_file(vec![1, 2, 3, 4]);
    let entry = file_entry(&file, writable_opt());
    let poll = entry.poll();
    assert!(poll.readable);
    assert!(poll.writable);
    assert!(!poll.error);

    let read_only = file_entry(&file, FdOpt::default());
    let read_only_poll = read_only.poll();
    assert!(read_only_poll.readable);
    assert!(!read_only_poll.writable);

    let write_only = file_entry(
        &file,
        FdOpt {
            rd: false,
            wr: true,
            ap: false,
            nb: false,
        },
    );
    let write_only_poll = write_only.poll();
    assert!(!write_only_poll.readable);
    assert!(write_only_poll.writable);

    assert_eq!(entry.io_ctl(FIONREAD), Ok(4));
    let mut buf = [0; 2];
    assert_eq!(entry.read(0, &mut buf), Ok(2));
    assert_eq!(entry.io_ctl(TIOCINQ), Ok(2));
    assert_eq!(entry.io_ctl(0xDEAD), Err("enotty"));

    let directory = directory_file();
    let directory_entry = file_entry(&directory, FdOpt::default());
    assert_eq!(directory_entry.io_ctl(FIONREAD), Err("enotty"));

    let epoll_entry = FdEntry::new(FLike::Ep(EpInst::new()));
    assert_eq!(epoll_entry.io_ctl(FIONREAD), Err("enotty"));
}

// AGENT: verify that terminal behavior follows the concrete FLike variant and
// keeps character-device EOF, permissions, polling, and seek semantics explicit.
#[cfg_attr(test, test)]
fn terminal_is_a_typed_nonseekable_fd_object() {
    let read_only = FdEntry::with_status(FLike::Tty(TtyDevice), FdOpt::default(), false);
    assert!(read_only.is_tty());
    assert!(!read_only.is_regular_file());
    assert_eq!(read_only.offset(), 0);
    assert_eq!(read_only.seek(FSeek::Start(0)), Err("espipe"));
    assert_eq!(read_only.write(0, b"x"), Err("ebadf"));
    assert_eq!(read_only.io_ctl(TCGETS), Err("enotty"));

    let mut buf = [0xaa; 4];
    assert_eq!(read_only.read(0, &mut buf), Ok(0));
    assert_eq!(buf, [0xaa; 4]);

    let poll = read_only.poll();
    assert!(poll.readable);
    assert!(!poll.writable);
    assert!(!poll.error);
    assert!(!poll.closed);
}

// AGENT: prove only the explicit FLike variant selects terminal semantics;
// a managed regular FInstance always uses regular-file storage and offsets.
#[cfg_attr(test, test)]
fn typed_regular_file_stays_a_regular_file() {
    let instance = regular_file(Vec::new());
    let entry = file_entry(&instance, writable_opt());

    assert!(entry.is_regular_file());
    assert!(!entry.is_tty());
    assert_eq!(entry.write(0, b"file-data"), Ok(FdWriteOutcome::Written(9)));
    assert_eq!(entry.offset(), 9);
    assert_eq!(file_bytes(&instance, 0, 9), b"file-data");
}
