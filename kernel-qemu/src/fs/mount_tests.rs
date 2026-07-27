// AGENT: object-VFS topology regressions exercise mount identity, stacking,
// storage ownership, and post-detach lifetime without pathname-prefix aliases.
use super::{MountFlags, MountTable};
use crate::kernel::fs::{FileKind, FileStorage, FsInstance, PathRef};
use alloc::sync::Arc;

// AGENT: construct one isolated first-stage filesystem for topology tests.
fn test_fs(id: usize) -> Arc<FsInstance> {
    FsInstance::new(id, FileStorage::standalone())
}

// AGENT: run every mount topology regression in the QEMU sync selftest suite.
pub fn run_all() {
    root_mount_owns_the_root_filesystem();
    filesystem_namespaces_and_inode_allocators_are_isolated();
    one_filesystem_can_be_attached_multiple_times();
    stacked_mounts_reveal_the_previous_layer_after_detach();
    path_ref_survives_detach_with_its_storage();
    regular_files_cannot_be_mountpoints();
    detached_parents_cannot_receive_new_mounts();
}

// AGENT: prove the root mount has no parent/mountpoint and retains the exact
// filesystem instance supplied at table construction.
#[cfg_attr(test, test)]
fn root_mount_owns_the_root_filesystem() {
    let fs = test_fs(10);
    let table = MountTable::new(fs.clone());
    let root = table.root();

    assert!(Arc::ptr_eq(root.fs(), &fs));
    assert!(Arc::ptr_eq(&root.fs().root(), &fs.root()));
    assert!(root.parent().is_none());
    assert!(root.mountpoint().is_none());
    assert_eq!(root.flags(), MountFlags::empty());
    assert_eq!(table.mount_count(), 0);
}

// AGENT: prove flat path keys and runtime inode allocation are local to each
// FsInstance rather than shared through Kernel.
#[cfg_attr(test, test)]
fn filesystem_namespaces_and_inode_allocators_are_isolated() {
    let first = test_fs(11);
    let second = test_fs(12);
    let first_node = first.create_regular("/file", false).unwrap();
    let second_node = second.create_regular("/file", false).unwrap();

    assert_eq!(first_node.id(), second_node.id());
    assert!(!Arc::ptr_eq(&first_node, &second_node));
    assert!(Arc::ptr_eq(&first.lookup("/file").unwrap(), &first_node));
    assert!(Arc::ptr_eq(&second.lookup("/file").unwrap(), &second_node));
}

// AGENT: attach one FsInstance at two inode locations without cloning its node
// namespace or storage backend.
#[cfg_attr(test, test)]
fn one_filesystem_can_be_attached_multiple_times() {
    let root_fs = test_fs(13);
    let left = root_fs.create_directory("/left").unwrap();
    let right = root_fs.create_directory("/right").unwrap();
    let shared = test_fs(14);
    let table = MountTable::new(root_fs);
    let root = table.root();

    let left_mount = table
        .attach(&root, left, shared.clone(), MountFlags::empty())
        .unwrap();
    let right_mount = table
        .attach(&root, right, shared.clone(), MountFlags::empty())
        .unwrap();

    assert_ne!(left_mount.id(), right_mount.id());
    assert!(Arc::ptr_eq(left_mount.fs(), &shared));
    assert!(Arc::ptr_eq(right_mount.fs(), &shared));
    assert_eq!(table.mount_count(), 2);
}

// AGENT: require last-attached visibility and reveal the lower attachment after
// popping only the top layer.
#[cfg_attr(test, test)]
fn stacked_mounts_reveal_the_previous_layer_after_detach() {
    let root_fs = test_fs(15);
    let mountpoint = root_fs.create_directory("/mnt").unwrap();
    let first_fs = test_fs(16);
    let second_fs = test_fs(17);
    let table = MountTable::new(root_fs);
    let root = table.root();
    let first = table
        .attach(&root, mountpoint.clone(), first_fs, MountFlags::empty())
        .unwrap();
    let second = table
        .attach(&root, mountpoint.clone(), second_fs, MountFlags::empty())
        .unwrap();

    assert!(Arc::ptr_eq(
        &table.mounted_on(&root, &mountpoint).unwrap(),
        &second
    ));
    assert!(Arc::ptr_eq(
        &table.detach_top(&root, &mountpoint).unwrap(),
        &second
    ));
    assert!(Arc::ptr_eq(
        &table.mounted_on(&root, &mountpoint).unwrap(),
        &first
    ));
}

// AGENT: keep a detached mount and inode usable through PathRef, including its
// filesystem-specific storage backend.
#[cfg_attr(test, test)]
fn path_ref_survives_detach_with_its_storage() {
    let root_fs = test_fs(18);
    let mountpoint = root_fs.create_directory("/mnt").unwrap();
    let mounted_fs = test_fs(19);
    let file = mounted_fs
        .install_regular("/file", b"detached", false)
        .unwrap();
    let table = MountTable::new(root_fs);
    let root = table.root();
    let mount = table
        .attach(&root, mountpoint.clone(), mounted_fs, MountFlags::empty())
        .unwrap();
    let path = PathRef {
        mount: mount.clone(),
        node: file,
    };

    table.detach_top(&root, &mountpoint).unwrap();

    assert!(table.mounted_on(&root, &mountpoint).is_none());
    assert_eq!(
        path.node.read_all(path.mount.fs().storage()).unwrap(),
        b"detached"
    );
}

// AGENT: reject regular-file mountpoints before allocating a child MountId.
#[cfg_attr(test, test)]
fn regular_files_cannot_be_mountpoints() {
    let root_fs = test_fs(20);
    let regular = root_fs.create_regular("/file", false).unwrap();
    let table = MountTable::new(root_fs);
    let root = table.root();

    assert!(matches!(
        table.attach(&root, regular, test_fs(21), MountFlags::empty()),
        Err("enotdir")
    ));
    assert_eq!(table.mount_count(), 0);
}

// AGENT: reject an Arc<Mount> after it has been removed from this table even if
// external references keep the detached object alive.
#[cfg_attr(test, test)]
fn detached_parents_cannot_receive_new_mounts() {
    let root_fs = test_fs(22);
    let mountpoint = root_fs.create_directory("/mnt").unwrap();
    let child_fs = test_fs(23);
    let child_mountpoint = child_fs.create_directory("/child").unwrap();
    let table = MountTable::new(root_fs);
    let root = table.root();
    let child = table
        .attach(&root, mountpoint.clone(), child_fs, MountFlags::empty())
        .unwrap();
    table.detach_top(&root, &mountpoint).unwrap();

    assert!(matches!(
        table.attach(&child, child_mountpoint, test_fs(24), MountFlags::empty()),
        Err("einval")
    ));
}
