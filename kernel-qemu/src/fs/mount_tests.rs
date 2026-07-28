// AGENT: object-VFS topology regressions exercise mount identity, stacking,
// storage ownership, and post-detach lifetime without pathname-prefix aliases.
use super::{MountFlags, MountState, MountTable, UnmountMode};
use crate::kernel::fs::{
    BlockCache, BlockDevice, ChildName, FInstance, FileBlockAllocator, FileKind, FileStorage,
    FsInstance, RamBlockDevice, Vfs,
};
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicBool, Ordering};

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
    stacked_mounts_reveal_the_previous_layer_after_unmount();
    active_path_blocks_normal_unmount_until_last_pin_drops();
    normal_unmount_rejects_child_mount_without_topology_changes();
    lazy_detach_removes_the_complete_subtree_and_preserves_pins();
    busy_lower_mount_does_not_block_visible_top_unmount();
    flush_failure_restores_attached_topology();
    lazy_flush_failure_restores_complete_subtree();
    mounted_lookup_pins_before_releasing_the_topology_lock();
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
fn stacked_mounts_reveal_the_previous_layer_after_unmount() {
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

    let visible = table.mounted_on(&root, &mountpoint).unwrap().unwrap();
    assert!(Arc::ptr_eq(visible.mount(), &second));
    drop(visible);
    assert!(Arc::ptr_eq(
        &table
            .unmount_top(&root, &mountpoint, UnmountMode::Normal)
            .unwrap(),
        &second
    ));
    let revealed = table.mounted_on(&root, &mountpoint).unwrap().unwrap();
    assert!(Arc::ptr_eq(revealed.mount(), &first));
}

// AGENT: make ordinary unmount depend only on explicit FInstance pins and
// preserve topology until the final path/fd reference is released.
#[cfg_attr(test, test)]
fn active_path_blocks_normal_unmount_until_last_pin_drops() {
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
    let cloned = path.clone();
    assert_eq!(mount.active_refs(), 2);
    drop(cloned);

    assert!(matches!(
        table.unmount_top(&root, &mountpoint, UnmountMode::Normal),
        Err("ebusy")
    ));
    assert_eq!(table.mount_count(), 1);
    assert_eq!(mount.state(), MountState::Attached);
    drop(path);

    table
        .unmount_top(&root, &mountpoint, UnmountMode::Normal)
        .unwrap();
    assert_eq!(mount.state(), MountState::Detached);
    assert_eq!(table.mount_count(), 0);
}

// AGENT: reject ordinary unmount of a parent with any child-mount stack and
// leave both attachments and their visible traversal unchanged.
#[cfg_attr(test, test)]
fn normal_unmount_rejects_child_mount_without_topology_changes() {
    let root_fs = test_fs(30);
    let mountpoint = create_directory(&root_fs, "mnt");
    let first_fs = test_fs(31);
    let child_mountpoint = create_directory(&first_fs, "sub");
    let second_fs = test_fs(32);
    let table = MountTable::new(root_fs);
    let root = table.root();
    let first = table
        .attach(&root, mountpoint.clone(), first_fs, MountFlags::empty())
        .unwrap();
    let second = table
        .attach(&first, child_mountpoint, second_fs, MountFlags::empty())
        .unwrap();

    assert!(matches!(
        table.unmount_top(&root, &mountpoint, UnmountMode::Normal),
        Err("ebusy")
    ));
    assert_eq!(table.mount_count(), 2);
    assert_eq!(first.state(), MountState::Attached);
    assert_eq!(second.state(), MountState::Attached);
    let visible_parent = table.mounted_on(&root, &mountpoint).unwrap().unwrap();
    assert!(Arc::ptr_eq(visible_parent.mount(), &first));
    drop(visible_parent);
}

// AGENT: detach every descendant stack atomically while allowing existing
// FInstance pins to keep detached files and their filesystem storage usable.
#[cfg_attr(test, test)]
fn lazy_detach_removes_the_complete_subtree_and_preserves_pins() {
    let root_fs = test_fs(33);
    let mountpoint = create_directory(&root_fs, "mnt");
    let first_fs = test_fs(34);
    let child_mountpoint = create_directory(&first_fs, "sub");
    let second_fs = test_fs(35);
    let file = second_fs
        .install_regular_at(&second_fs.root(), child_name("file"), b"detached", false)
        .unwrap();
    let table = MountTable::new(root_fs);
    let root = table.root();
    let first = table
        .attach(&root, mountpoint.clone(), first_fs, MountFlags::empty())
        .unwrap();
    let second = table
        .attach(&first, child_mountpoint, second_fs, MountFlags::empty())
        .unwrap();
    let pinned_file = FInstance::new(second.clone(), file);

    table
        .unmount_top(&root, &mountpoint, UnmountMode::Lazy)
        .unwrap();

    assert_eq!(table.mount_count(), 0);
    assert!(table.mounted_on(&root, &mountpoint).unwrap().is_none());
    assert_eq!(first.state(), MountState::Detached);
    assert_eq!(second.state(), MountState::Detached);
    assert_eq!(
        pinned_file
            .node
            .read_all(pinned_file.mount.fs().storage())
            .unwrap(),
        b"detached"
    );
    let cloned_after_detach = pinned_file.clone();
    let mut cloned_bytes = [0u8; 8];
    assert_eq!(
        cloned_after_detach.read_at(0, &mut cloned_bytes),
        Ok(cloned_bytes.len())
    );
    assert_eq!(&cloned_bytes, b"detached");
}

// AGENT: make a busy covered mount irrelevant to ordinary unmount of the
// distinct visible attachment stacked above it.
#[cfg_attr(test, test)]
fn busy_lower_mount_does_not_block_visible_top_unmount() {
    let root_fs = test_fs(36);
    let mountpoint = create_directory(&root_fs, "mnt");
    let lower_fs = test_fs(37);
    let table = MountTable::new(root_fs);
    let root = table.root();
    let lower = table
        .attach(
            &root,
            mountpoint.clone(),
            lower_fs.clone(),
            MountFlags::empty(),
        )
        .unwrap();
    let lower_pin = FInstance::new(lower.clone(), lower_fs.root());
    let upper = table
        .attach(&root, mountpoint.clone(), test_fs(38), MountFlags::empty())
        .unwrap();

    let removed = table
        .unmount_top(&root, &mountpoint, UnmountMode::Normal)
        .unwrap();
    assert!(Arc::ptr_eq(&removed, &upper));
    assert_eq!(upper.state(), MountState::Detached);
    assert_eq!(lower.state(), MountState::Attached);
    assert_eq!(lower.active_refs(), 1);
    drop(lower_pin);
}

// AGENT: inject a stable-write failure and prove both topology and lifecycle
// state roll back before a later successful ordinary unmount commits.
#[cfg_attr(test, test)]
fn flush_failure_restores_attached_topology() {
    let root_fs = test_fs(39);
    let mountpoint = create_directory(&root_fs, "mnt");
    let device = Arc::new(FailingFlushDevice::new());
    let storage = FileStorage::new(
        Arc::new(BlockCache::new(1)),
        device.clone(),
        Arc::new(FileBlockAllocator::new(device.block_count())),
    );
    let table = MountTable::new(root_fs);
    let root = table.root();
    let mount = table
        .attach(
            &root,
            mountpoint.clone(),
            FsInstance::new(40, storage),
            MountFlags::empty(),
        )
        .unwrap();

    assert!(matches!(
        table.unmount_top(&root, &mountpoint, UnmountMode::Normal),
        Err("eio")
    ));
    assert_eq!(mount.state(), MountState::Attached);
    assert_eq!(table.mount_count(), 1);
    let visible = table.mounted_on(&root, &mountpoint).unwrap().unwrap();
    assert!(Arc::ptr_eq(visible.mount(), &mount));
    drop(visible);

    device.fail_flush.store(false, Ordering::Release);
    table
        .unmount_top(&root, &mountpoint, UnmountMode::Normal)
        .unwrap();
    assert_eq!(mount.state(), MountState::Detached);
}

// AGENT: roll back every reserved descendant when a later filesystem in the
// synchronous lazy-flush sequence fails, leaving no partially hidden subtree.
#[cfg_attr(test, test)]
fn lazy_flush_failure_restores_complete_subtree() {
    let root_fs = test_fs(43);
    let mountpoint = create_directory(&root_fs, "mnt");
    let parent_fs = test_fs(44);
    let child_mountpoint = create_directory(&parent_fs, "sub");
    let device = Arc::new(FailingFlushDevice::new());
    let child_storage = FileStorage::new(
        Arc::new(BlockCache::new(1)),
        device.clone(),
        Arc::new(FileBlockAllocator::new(device.block_count())),
    );
    let table = MountTable::new(root_fs);
    let root = table.root();
    let parent = table
        .attach(&root, mountpoint.clone(), parent_fs, MountFlags::empty())
        .unwrap();
    let child = table
        .attach(
            &parent,
            child_mountpoint,
            FsInstance::new(45, child_storage),
            MountFlags::empty(),
        )
        .unwrap();

    assert!(matches!(
        table.unmount_top(&root, &mountpoint, UnmountMode::Lazy),
        Err("eio")
    ));
    assert_eq!(table.mount_count(), 2);
    assert_eq!(parent.state(), MountState::Attached);
    assert_eq!(child.state(), MountState::Attached);
    let visible = table.mounted_on(&root, &mountpoint).unwrap().unwrap();
    assert!(Arc::ptr_eq(visible.mount(), &parent));
    drop(visible);

    device.fail_flush.store(false, Ordering::Release);
    table
        .unmount_top(&root, &mountpoint, UnmountMode::Lazy)
        .unwrap();
    assert_eq!(table.mount_count(), 0);
    assert_eq!(parent.state(), MountState::Detached);
    assert_eq!(child.state(), MountState::Detached);
}

// AGENT: exercise the race boundary directly: a visible lookup returns with an
// already-counted pin, so ordinary unmount cannot succeed in a post-lock gap.
#[cfg_attr(test, test)]
fn mounted_lookup_pins_before_releasing_the_topology_lock() {
    let root_fs = test_fs(41);
    let mountpoint = create_directory(&root_fs, "mnt");
    let table = MountTable::new(root_fs);
    let root = table.root();
    let mount = table
        .attach(&root, mountpoint.clone(), test_fs(42), MountFlags::empty())
        .unwrap();

    let pin = table.mounted_on(&root, &mountpoint).unwrap().unwrap();
    assert_eq!(mount.active_refs(), 1);
    assert!(matches!(
        table.unmount_top(&root, &mountpoint, UnmountMode::Normal),
        Err("ebusy")
    ));
    drop(pin);
    table
        .unmount_top(&root, &mountpoint, UnmountMode::Normal)
        .unwrap();
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
    table
        .unmount_top(&root, &mountpoint, UnmountMode::Normal)
        .unwrap();

    assert!(matches!(
        table.attach(&child, child_mountpoint, test_fs(24), MountFlags::empty()),
        Err("einval")
    ));
}

// AGENT: wrap the RAM device with a controllable flush error while preserving
// ordinary cached reads, writes, and capacity for transactional-unmount tests.
struct FailingFlushDevice {
    backing: RamBlockDevice,
    fail_flush: AtomicBool,
}

impl FailingFlushDevice {
    fn new() -> Self {
        Self {
            backing: RamBlockDevice::empty(),
            fail_flush: AtomicBool::new(true),
        }
    }
}

impl BlockDevice for FailingFlushDevice {
    fn block_count(&self) -> usize {
        self.backing.block_count()
    }

    fn read_block(&self, block: usize) -> Result<Vec<u8>, &'static str> {
        self.backing.read_block(block)
    }

    fn write_block(&self, block: usize, data: &[u8]) -> Result<(), &'static str> {
        self.backing.write_block(block, data)
    }

    fn flush(&self) -> Result<(), &'static str> {
        if self.fail_flush.load(Ordering::Acquire) {
            Err("eio")
        } else {
            Ok(())
        }
    }
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
