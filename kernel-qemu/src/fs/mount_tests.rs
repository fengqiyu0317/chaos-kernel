// AGENT: object-VFS topology regressions exercise mount identity, stacking,
// storage ownership, and post-detach lifetime without pathname-prefix aliases.
use super::{MountFlags, MountTable};
use crate::kernel::fs::{ChildName, FInstance, FileKind, FileStorage, FsInstance, Vfs};
use alloc::sync::Arc;

// AGENT: construct validated direct-child values for FsInstance-level topology
// fixtures that intentionally bypass full VFS pathname parsing.
fn child_name(name: &str) -> ChildName<'_> {
    ChildName::new(name).expect("test child name should be one ordinary component")
}

// AGENT: construct one isolated first-stage filesystem for topology tests.
fn test_fs(id: usize) -> Arc<FsInstance> {
    FsInstance::new(id, FileStorage::standalone())
}

// AGENT: create one direct regular child without reintroducing full-path
// helpers into FsInstance-focused topology tests.
fn create_regular(fs: &Arc<FsInstance>, name: &str) -> Arc<crate::kernel::fs::FileNode> {
    fs.create_regular_at(&fs.root(), child_name(name), false)
        .unwrap()
}

// AGENT: create one direct directory child from the filesystem root for mount
// topology fixtures that do not need to exercise VFS parsing.
fn create_directory(fs: &Arc<FsInstance>, name: &str) -> Arc<crate::kernel::fs::FileNode> {
    fs.create_directory_at(&fs.root(), child_name(name))
        .unwrap()
}

// AGENT: run every mount topology regression in the QEMU sync selftest suite.
pub fn run_all() {
    root_mount_owns_the_root_filesystem();
    filesystem_namespaces_and_inode_allocators_are_isolated();
    one_filesystem_can_be_attached_multiple_times();
    stacked_mounts_reveal_the_previous_layer_after_detach();
    finstance_survives_detach_with_its_storage();
    regular_files_cannot_be_mountpoints();
    detached_parents_cannot_receive_new_mounts();
    direct_child_names_are_validated_and_unique();
    ordered_component_walk_preserves_lookup_errors();
    direct_child_identity_is_parent_local_and_stable();
    nested_mounts_are_crossed_component_by_component();
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

// AGENT: prove directory namespaces and runtime inode allocation are local to
// each FsInstance rather than shared through Kernel.
#[cfg_attr(test, test)]
fn filesystem_namespaces_and_inode_allocators_are_isolated() {
    let first = test_fs(11);
    let second = test_fs(12);
    let first_node = create_regular(&first, "file");
    let second_node = create_regular(&second, "file");

    assert_eq!(first_node.id(), second_node.id());
    assert!(!Arc::ptr_eq(&first_node, &second_node));
    assert!(Arc::ptr_eq(
        &first
            .lookup_child(&first.root(), child_name("file"))
            .unwrap(),
        &first_node
    ));
    assert!(Arc::ptr_eq(
        &second
            .lookup_child(&second.root(), child_name("file"))
            .unwrap(),
        &second_node
    ));
}

// AGENT: attach one FsInstance at two inode locations without cloning its node
// namespace or storage backend.
#[cfg_attr(test, test)]
fn one_filesystem_can_be_attached_multiple_times() {
    let root_fs = test_fs(13);
    let left = create_directory(&root_fs, "left");
    let right = create_directory(&root_fs, "right");
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
    let mountpoint = create_directory(&root_fs, "mnt");
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

// AGENT: keep a detached mount and inode usable through FInstance, including its
// filesystem-specific storage backend.
#[cfg_attr(test, test)]
fn finstance_survives_detach_with_its_storage() {
    let root_fs = test_fs(18);
    let mountpoint = create_directory(&root_fs, "mnt");
    let mounted_fs = test_fs(19);
    let file = mounted_fs
        .install_regular_at(&mounted_fs.root(), child_name("file"), b"detached", false)
        .unwrap();
    let table = MountTable::new(root_fs);
    let root = table.root();
    let mount = table
        .attach(&root, mountpoint.clone(), mounted_fs, MountFlags::empty())
        .unwrap();
    let path = FInstance::new(mount.clone(), file);

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
    let regular = create_regular(&root_fs, "file");
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
    let mountpoint = create_directory(&root_fs, "mnt");
    let child_fs = test_fs(23);
    let child_mountpoint = create_directory(&child_fs, "child");
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

// AGENT: enforce the single-component namespace contract and strict duplicate
// handling before allocating or publishing another inode object.
#[cfg_attr(test, test)]
fn direct_child_names_are_validated_and_unique() {
    let fs = test_fs(25);
    let root = fs.root();
    let file = fs
        .create_regular_at(&root, child_name("file"), false)
        .unwrap();

    assert!(matches!(
        fs.create_regular_at(&root, child_name("file"), false),
        Err("eexist")
    ));
    for invalid in ["", ".", "..", "child/name"] {
        assert!(matches!(ChildName::new(invalid), Err("einval")));
    }
    assert!(matches!(
        fs.create_regular_at(&file, child_name("child"), false),
        Err("enotdir")
    ));
    assert!(Arc::ptr_eq(&fs.lookup_inode(file.id()).unwrap(), &file));
}

// AGENT: prove dot-dot is applied only after each earlier component lookup and
// after verifying that the current object is a directory.
#[cfg_attr(test, test)]
fn ordered_component_walk_preserves_lookup_errors() {
    let root_fs = test_fs(26);
    let vfs = Vfs::new(root_fs);
    vfs.install_regular("/file", &[], false).unwrap();
    vfs.install_regular("/regular", &[], false).unwrap();

    assert!(matches!(vfs.resolve("/missing/../file"), Err("enoent")));
    assert!(matches!(vfs.resolve("/regular/../file"), Err("enotdir")));
}

// AGENT: prove identical names under different directory inodes resolve to
// different objects while repeated traversal returns the same managed inode.
#[cfg_attr(test, test)]
fn direct_child_identity_is_parent_local_and_stable() {
    let root_fs = test_fs(27);
    let vfs = Vfs::new(root_fs);
    vfs.create_directory("/a").unwrap();
    vfs.create_directory("/b").unwrap();
    vfs.install_regular("/a/file", &[], false).unwrap();
    vfs.install_regular("/b/file", &[], false).unwrap();

    let a_file = vfs.resolve("/a/file").unwrap();
    let b_file = vfs.resolve("/b/file").unwrap();
    let repeated = vfs.resolve("/a/./file").unwrap();
    assert!(!Arc::ptr_eq(&a_file.path_ref.node, &b_file.path_ref.node));
    assert!(Arc::ptr_eq(&a_file.path_ref.node, &repeated.path_ref.node));
}

// AGENT: prove each ordinary component crosses the visible identity-keyed mount
// before the following child lookup, including a mount nested inside another.
#[cfg_attr(test, test)]
fn nested_mounts_are_crossed_component_by_component() {
    let root_fs = test_fs(28);
    let vfs = Vfs::new(root_fs);
    vfs.create_directory("/mnt").unwrap();

    let first = vfs.new_filesystem(FileStorage::standalone());
    first
        .create_directory_at(&first.root(), child_name("sub"))
        .unwrap();
    vfs.attach("/mnt", first, MountFlags::empty()).unwrap();

    let second = vfs.new_filesystem(FileStorage::standalone());
    second
        .install_regular_at(&second.root(), child_name("file"), b"nested", false)
        .unwrap();
    let nested = vfs
        .attach("/mnt/sub", second.clone(), MountFlags::empty())
        .unwrap();

    let resolved = vfs.resolve("/mnt/sub/file").unwrap();
    assert!(Arc::ptr_eq(&resolved.path_ref.mount, &nested));
    assert!(Arc::ptr_eq(resolved.path_ref.mount.fs(), &second));
}
