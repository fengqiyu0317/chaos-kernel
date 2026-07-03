// AGENT: fd-focused regressions split out from fd.rs while retaining the same
// module path for Rust tests and qemu-sync-selftest.
use super::*;

// AGENT: keep the QEMU boot selftest aggregator in the moved fd test module.
pub fn run_all() {
    set_len_and_sync_update_dirty_state();
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

// AGENT: moved dirty-state regression out of fd.rs without changing behavior.
#[cfg_attr(test, test)]
fn set_len_and_sync_update_dirty_state() {
    let fh = FHandle::with_data("/tmp/file", writable_opt(), vec![1, 2, 3]);
    assert_eq!(fh.node.dirty_state(), FileDirty::clean());

    fh.set_len(5).unwrap();
    assert_eq!(fh.node.data.lock().unwrap().as_slice(), &[1, 2, 3, 0, 0]);
    assert_eq!(
        fh.node.dirty_state(),
        FileDirty {
            data: true,
            metadata: true
        }
    );

    fh.sync_data().unwrap();
    assert_eq!(
        fh.node.dirty_state(),
        FileDirty {
            data: false,
            metadata: true
        }
    );

    fh.sync_all().unwrap();
    assert_eq!(fh.node.dirty_state(), FileDirty::clean());

    let ro = FHandle::with_data("/tmp/ro", FdOpt::default(), vec![1, 2, 3]);
    assert_eq!(ro.set_len(0), Err("ebadf"));
}

// AGENT: moved fallocate regression out of fd.rs without changing behavior.
#[cfg_attr(test, test)]
fn fallocate_validates_and_only_grows_regular_files() {
    let fh = FHandle::with_data("/tmp/file", writable_opt(), vec![1, 2, 3]);

    fh.fallocate(5, 2).unwrap();
    assert_eq!(
        fh.node.data.lock().unwrap().as_slice(),
        &[1, 2, 3, 0, 0, 0, 0]
    );
    assert_eq!(
        fh.node.dirty_state(),
        FileDirty {
            data: true,
            metadata: true
        }
    );

    fh.sync_all().unwrap();
    fh.fallocate(1, 1).unwrap();
    assert_eq!(fh.node.data.lock().unwrap().len(), 7);
    assert_eq!(fh.node.dirty_state(), FileDirty::clean());

    assert_eq!(fh.fallocate(0, 0), Err("einval"));
    assert_eq!(fh.fallocate(usize::MAX, 1), Err("efbig"));

    let ro = FHandle::with_data("/tmp/ro", FdOpt::default(), vec![1, 2, 3]);
    assert_eq!(ro.fallocate(0, 1), Err("ebadf"));

    let dir = FHandle::with_node("/tmp", writable_opt(), Arc::new(FileNode::directory()));
    assert_eq!(dir.fallocate(0, 1), Err("enodev"));
}

// AGENT: moved node-local lookup regression out of fd.rs without changing behavior.
#[cfg_attr(test, test)]
fn lookup_reports_node_local_errors() {
    let file = FHandle::with_data("/tmp/file", writable_opt(), Vec::new());
    assert_eq!(file.lookup(".", 0), Err("enotdir"));

    let dir = FHandle::with_node("/tmp", FdOpt::default(), Arc::new(FileNode::directory()));
    assert_eq!(dir.lookup(".", 0), Ok(()));
    assert_eq!(dir.lookup("", 0), Ok(()));
    dir.node.add_dir_entry("child").unwrap();
    assert_eq!(dir.lookup("child", 0), Ok(()));
    assert_eq!(dir.lookup("missing", 0), Err("enoent"));
    assert_eq!(dir.lookup(".", 41), Err("eloop"));
    assert_eq!(dir.lookup("bad\0name", 0), Err("einval"));
    assert_eq!(dir.lookup("bad/name", 0), Err("einval"));
}

// AGENT: moved regular-file poll/ioctl regression out of fd.rs unchanged.
#[cfg_attr(test, test)]
fn regular_file_poll_and_ioctl_are_explicit() {
    let file = FHandle::with_data("", writable_opt(), vec![1, 2, 3, 4]);
    let poll = file.poll_status();
    assert!(poll.readable);
    assert!(poll.writable);
    assert!(!poll.error);

    assert_eq!(file.io_ctl(FIONREAD, 0), Ok(4));
    let mut buf = [0; 2];
    assert_eq!(file.read(&mut buf), Ok(2));
    assert_eq!(file.io_ctl(TIOCINQ, 0), Ok(2));
    assert_eq!(file.io_ctl(0xDEAD, 0), Err("enotty"));
}

// AGENT: moved splice permission regression out of fd.rs unchanged.
#[cfg_attr(test, test)]
fn splice_checks_permissions_before_moving_offsets() {
    let src = FHandle::with_data("/src", FdOpt::default(), vec![1, 2, 3]);
    let dst = FHandle::with_data("/dst", FdOpt::default(), Vec::new());
    let src_entry = FdEntry::new(FLike::File(src.clone()));
    let dst_entry = FdEntry::new(FLike::File(dst.clone()));

    assert_eq!(src_entry.splice_to(&dst_entry, 2), Err("ebadf"));
    assert_eq!(src.offset(), 0);
    assert_eq!(dst.offset(), 0);
    assert!(dst.node.data.lock().unwrap().is_empty());

    let unreadable = FdOpt {
        rd: false,
        wr: true,
        ap: false,
        nb: false,
    };
    let src = FHandle::with_data("/src", unreadable, vec![1, 2, 3]);
    let dst = FHandle::with_data("/dst", writable_opt(), Vec::new());
    let src_entry = FdEntry::new(FLike::File(src.clone()));
    let dst_entry = FdEntry::new(FLike::File(dst.clone()));

    assert_eq!(src_entry.splice_to(&dst_entry, 2), Err("ebadf"));
    assert_eq!(src.offset(), 0);
    assert!(dst.node.data.lock().unwrap().is_empty());
}

// AGENT: moved shared append-status splice regression out of fd.rs unchanged.
#[cfg_attr(test, test)]
fn splice_uses_shared_append_status() {
    let src = FHandle::with_data("/src", FdOpt::default(), vec![1, 2, 3]);
    let dst = FHandle::with_data("/dst", writable_opt(), vec![9, 9]);
    dst.seek(FSeek::Start(1)).unwrap();
    let src_entry = FdEntry::new(FLike::File(src.clone()));
    let dst_entry = FdEntry::new(FLike::File(dst.clone()));

    dst_entry.set_status_flags(O_APPEND).unwrap();
    assert_eq!(src_entry.splice_to(&dst_entry, 2), Ok(2));
    assert_eq!(src.offset(), 2);
    assert_eq!(dst.offset(), 4);
    assert_eq!(dst.node.data.lock().unwrap().as_slice(), &[9, 9, 1, 2]);
}
