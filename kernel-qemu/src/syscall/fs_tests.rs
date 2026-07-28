// AGENT: QEMU filesystem syscall regressions that require the installed frame
// pool, current init task, Sv39 mappings, and authoritative AddrSpace usercopy.
use super::*;
use crate::syscall_abi::{
    map_riscv_nr, INTERNAL_SYS_MKDIRAT, INTERNAL_SYS_MOUNT, INTERNAL_SYS_OPENAT,
    INTERNAL_SYS_UMOUNT2, RISCV_SYS_MKDIRAT, RISCV_SYS_MOUNT, RISCV_SYS_OPENAT, RISCV_SYS_UMOUNT2,
};

const USER_STRINGS_BASE: usize = 0x4000_0000;
const OPEN_USER_BASE: usize = 0x4001_0000;
const MKDIR_USER_BASE: usize = 0x4002_0000;

// AGENT: Run filesystem ABI and semantic regressions after QEMU installs the
// real kernel frame pool, current init task, Sv39 mappings, and fd table.
pub fn run_all(kernel: &Kernel) {
    kernel
        .install_directory("/tmp")
        .expect("filesystem selftests require /tmp");
    mount_and_umount2_use_usercopy_and_mutate_mount_table(kernel);
    path_creation_requires_an_existing_directory_parent(kernel);
    pathname_lookup_returns_shared_file_node(kernel);
    mounted_open_and_exec_use_the_mounted_filesystem_storage(kernel);
    mkdirat_creates_only_new_absolute_directories(kernel);
    openat_uses_transactional_fd_and_path_state(kernel);
}

// AGENT: Write one NUL-terminated syscall string through the active address
// space so the test exercises the same usercopy path as a user ecall.
fn write_user_string(kernel: &Kernel, task: &Task, addr: usize, value: &str) {
    let mut bytes = Vec::from(value.as_bytes());
    bytes.push(0);
    task.process
        .addr_space
        .lock()
        .unwrap()
        .write_user_bytes(addr, &bytes, &kernel.pool)
        .expect("test user string should be writable");
}

// AGENT: verify source-aware RISC-V mounts reuse one live FsInstance per source,
// isolate distinct sources, preserve mount stacking, and leave topology unchanged
// across all rejected source, type, target, flag, and usercopy boundaries.
#[cfg_attr(test, test)]
fn mount_and_umount2_use_usercopy_and_mutate_mount_table(kernel: &Kernel) {
    assert_eq!(map_riscv_nr(RISCV_SYS_MOUNT), Some(INTERNAL_SYS_MOUNT));
    assert_eq!(map_riscv_nr(RISCV_SYS_UMOUNT2), Some(INTERNAL_SYS_UMOUNT2));

    let task = kernel.cur_task(0).expect("init task should be current");
    task.process
        .addr_space
        .lock()
        .unwrap()
        .map_region(
            VmRegion::new(USER_STRINGS_BASE, PAGE_SZ, VM_READ | VM_WRITE),
            &kernel.pool,
        )
        .expect("filesystem syscall user page should map");

    let source_addr = USER_STRINGS_BASE;
    let target_addr = USER_STRINGS_BASE + 64;
    let filesystem_type_addr = USER_STRINGS_BASE + 128;
    let missing_target_addr = USER_STRINGS_BASE + 192;
    let regular_target_addr = USER_STRINGS_BASE + 256;
    let unknown_type_addr = USER_STRINGS_BASE + 320;
    let empty_addr = USER_STRINGS_BASE + 384;
    write_user_string(kernel, &task, source_addr, "dev0");
    write_user_string(kernel, &task, target_addr, "/mnt");
    write_user_string(kernel, &task, filesystem_type_addr, "chaosfs");
    write_user_string(kernel, &task, missing_target_addr, "/mount-missing");
    write_user_string(kernel, &task, regular_target_addr, "/mount-regular");
    write_user_string(kernel, &task, unknown_type_addr, "unknownfs");
    write_user_string(kernel, &task, empty_addr, "");
    kernel
        .install_directory("/mnt")
        .expect("mount syscall requires an existing directory mountpoint");
    kernel
        .install_file("/mount-regular", Vec::new(), false)
        .expect("mount syscall regular-target fixture should install");

    let dev0 = kernel.vfs.new_filesystem(FileStorage::standalone());
    let dev1 = kernel.vfs.new_filesystem(FileStorage::standalone());
    assert_eq!(kernel.vfs.register_source("", dev0.clone()), Err("einval"));
    kernel
        .vfs
        .register_source("dev0", dev0.clone())
        .expect("dev0 source should register");
    assert_eq!(
        kernel.vfs.register_source("dev0", dev0.clone()),
        Err("eexist")
    );
    kernel
        .vfs
        .register_source("dev1", dev1.clone())
        .expect("dev1 source should register");

    assert_eq!(
        kernel.dispatch_syscall_without_signal_delivery(
            SYS_MOUNT,
            source_addr,
            target_addr,
            filesystem_type_addr,
            0,
            0,
            0,
        ),
        Ok(0)
    );
    let first_mount = kernel
        .lookup_file_node("/mnt")
        .expect("mount should expose its filesystem root")
        .path_ref
        .mount;
    assert!(Arc::ptr_eq(first_mount.fs(), &dev0));
    assert_eq!(first_mount.fs().root().kind, FileKind::Directory);
    assert_eq!(kernel.vfs.mounts.mount_count(), 1);
    kernel
        .install_file("/mnt/file", b"dev0".to_vec(), false)
        .expect("dev0 file should install through the first mount");
    let dev0_file = kernel
        .lookup_file_node("/mnt/file")
        .expect("dev0 file should resolve through the first mount")
        .path_ref
        .node;

    // Mounting dev0 again creates a distinct attachment but selects the exact
    // same filesystem, inode namespace, storage, cache, and allocator state.
    assert_eq!(
        kernel.dispatch_syscall_without_signal_delivery(
            SYS_MOUNT,
            source_addr,
            target_addr,
            filesystem_type_addr,
            0,
            0,
            0,
        ),
        Ok(0)
    );
    let second_mount = kernel
        .lookup_file_node("/mnt")
        .expect("stacked mount should expose the repeated filesystem root")
        .path_ref
        .mount;
    assert!(!Arc::ptr_eq(&first_mount, &second_mount));
    assert!(Arc::ptr_eq(first_mount.fs(), second_mount.fs()));
    assert!(Arc::ptr_eq(second_mount.fs(), &dev0));
    let remounted_file = kernel
        .lookup_file_node("/mnt/file")
        .expect("the repeated dev0 mount should expose its existing inode")
        .path_ref
        .node;
    assert!(Arc::ptr_eq(&dev0_file, &remounted_file));
    assert_eq!(kernel.vfs.mounts.mount_count(), 2);

    assert_eq!(
        kernel.dispatch_syscall_without_signal_delivery(SYS_UMOUNT2, target_addr, 0, 0, 0, 0, 0,),
        Ok(0)
    );
    let revealed_first = kernel
        .lookup_file_node("/mnt")
        .expect("detaching the repeated mount should reveal the first attachment")
        .path_ref
        .mount;
    assert!(Arc::ptr_eq(&revealed_first, &first_mount));
    assert_eq!(kernel.vfs.mounts.mount_count(), 1);

    // A distinct source selects a distinct filesystem and hides dev0's inode
    // namespace until the dev1 attachment is removed.
    write_user_string(kernel, &task, source_addr, "dev1");
    assert_eq!(
        kernel.dispatch_syscall_without_signal_delivery(
            SYS_MOUNT,
            source_addr,
            target_addr,
            filesystem_type_addr,
            0,
            0,
            0,
        ),
        Ok(0)
    );
    let dev1_mount = kernel
        .lookup_file_node("/mnt")
        .expect("dev1 mount should expose its filesystem root")
        .path_ref
        .mount;
    assert!(Arc::ptr_eq(dev1_mount.fs(), &dev1));
    assert!(!Arc::ptr_eq(first_mount.fs(), dev1_mount.fs()));
    assert!(matches!(
        kernel.lookup_file_node("/mnt/file"),
        Err("enoent")
    ));
    assert_eq!(kernel.vfs.mounts.mount_count(), 2);

    assert_eq!(
        kernel.dispatch_syscall_without_signal_delivery(SYS_UMOUNT2, target_addr, 0, 0, 0, 0, 0,),
        Ok(0)
    );
    let revealed = kernel
        .lookup_file_node("/mnt/file")
        .expect("detaching dev1 should reveal dev0's existing file");
    assert!(Arc::ptr_eq(&revealed.path_ref.node, &dev0_file));
    assert!(Arc::ptr_eq(revealed.path_ref.mount.fs(), &dev0));
    assert_eq!(kernel.vfs.mounts.mount_count(), 1);

    // Every failed mount below must leave the one live dev0 attachment intact.
    write_user_string(kernel, &task, source_addr, "missing");
    assert_eq!(
        kernel.dispatch_syscall_without_signal_delivery(
            SYS_MOUNT,
            source_addr,
            target_addr,
            filesystem_type_addr,
            0,
            0,
            0,
        ),
        Err("enodev")
    );

    write_user_string(kernel, &task, source_addr, "dev0");
    assert_eq!(
        kernel.dispatch_syscall_without_signal_delivery(
            SYS_MOUNT,
            source_addr,
            target_addr,
            unknown_type_addr,
            0,
            0,
            0,
        ),
        Err("enodev")
    );
    assert_eq!(
        kernel.dispatch_syscall_without_signal_delivery(
            SYS_MOUNT,
            empty_addr,
            target_addr,
            filesystem_type_addr,
            0,
            0,
            0,
        ),
        Err("einval")
    );
    assert_eq!(
        kernel.dispatch_syscall_without_signal_delivery(
            SYS_MOUNT,
            source_addr,
            target_addr,
            empty_addr,
            0,
            0,
            0,
        ),
        Err("einval")
    );
    assert_eq!(
        kernel.dispatch_syscall_without_signal_delivery(
            SYS_MOUNT,
            source_addr,
            target_addr,
            0,
            0,
            0,
            0,
        ),
        Err("einval")
    );
    assert_eq!(
        kernel.dispatch_syscall_without_signal_delivery(
            SYS_MOUNT,
            source_addr,
            missing_target_addr,
            filesystem_type_addr,
            0,
            0,
            0,
        ),
        Err("enoent")
    );
    assert_eq!(
        kernel.dispatch_syscall_without_signal_delivery(
            SYS_MOUNT,
            source_addr,
            regular_target_addr,
            filesystem_type_addr,
            0,
            0,
            0,
        ),
        Err("enotdir")
    );
    assert_eq!(
        kernel.dispatch_syscall_without_signal_delivery(
            SYS_MOUNT,
            source_addr,
            target_addr,
            filesystem_type_addr,
            1,
            0,
            0,
        ),
        Err("enotsup")
    );
    assert_eq!(
        kernel.dispatch_syscall_without_signal_delivery(
            SYS_MOUNT,
            source_addr,
            target_addr,
            filesystem_type_addr,
            0,
            1,
            0,
        ),
        Err("enotsup")
    );
    assert_eq!(
        kernel.dispatch_syscall_without_signal_delivery(
            SYS_MOUNT,
            USER_STRINGS_BASE + PAGE_SZ * 2,
            target_addr,
            filesystem_type_addr,
            0,
            0,
            0,
        ),
        Err("efault")
    );
    assert_eq!(kernel.vfs.mounts.mount_count(), 1);

    let still_mounted = kernel
        .lookup_file_node("/mnt")
        .expect("failed mounts must not replace the existing attachment")
        .path_ref
        .mount;
    assert!(Arc::ptr_eq(&still_mounted, &first_mount));
    assert_eq!(
        kernel.dispatch_syscall_without_signal_delivery(SYS_UMOUNT2, target_addr, 0, 0, 0, 0, 0,),
        Ok(0)
    );
    assert_eq!(kernel.vfs.mounts.mount_count(), 0);
    assert_eq!(
        kernel.dispatch_syscall_without_signal_delivery(SYS_UMOUNT2, target_addr, 0, 0, 0, 0, 0,),
        Err("einval")
    );
}

// AGENT: enforce the path-table invariant that root exists and every non-root
// node has one visible, existing directory parent.
#[cfg_attr(test, test)]
fn path_creation_requires_an_existing_directory_parent(kernel: &Kernel) {
    let root = kernel
        .lookup_file_node("/")
        .expect("the namespace root should always exist");
    assert_eq!(root.path_ref.node.kind, FileKind::Directory);

    assert_eq!(
        kernel.install_file("/qemu-missing-parent/file", Vec::new(), false),
        Err("enoent")
    );
    assert!(matches!(
        kernel.lookup_file_node("/qemu-missing-parent/file"),
        Err("enoent")
    ));
    assert!(matches!(
        kernel.open_regular_node(
            "/qemu-open-missing-parent/file",
            CreateDisposition::CreateIfMissing,
        ),
        Err("enoent")
    ));
    assert!(matches!(
        kernel.lookup_file_node("/qemu-open-missing-parent/file"),
        Err("enoent")
    ));

    kernel
        .install_directory("/qemu-parent-dir")
        .expect("test parent directory should install");
    kernel
        .install_file("/qemu-parent-dir/child", b"child".to_vec(), false)
        .expect("child below an existing directory should install");
    let parent = kernel
        .lookup_file_node("/qemu-parent-dir")
        .expect("test parent directory should remain registered");
    assert_eq!(
        parent.path_ref.node.dir_entry_at(0),
        Ok(String::from("child"))
    );
    assert_eq!(parent.path_ref.node.dir_entry_at(1), Err("enoent"));

    kernel
        .install_directory("/qemu-open-parent")
        .expect("open test parent directory should install");
    let created = kernel
        .open_regular_node(
            "/qemu-open-parent/child",
            CreateDisposition::CreateIfMissing,
        )
        .expect("open should create below an existing directory");
    let reopened = kernel
        .open_regular_node(
            "/qemu-open-parent/child",
            CreateDisposition::CreateIfMissing,
        )
        .expect("reopen should return the existing file node");
    assert!(Arc::ptr_eq(&created.path_ref.node, &reopened.path_ref.node));
    let open_parent = kernel
        .lookup_file_node("/qemu-open-parent")
        .expect("open test parent should remain registered");
    assert_eq!(
        open_parent.path_ref.node.dir_entry_at(0),
        Ok(String::from("child"))
    );
    assert_eq!(open_parent.path_ref.node.dir_entry_at(1), Err("enoent"));

    kernel
        .install_file("/qemu-regular-parent", Vec::new(), false)
        .expect("regular parent fixture should install");
    assert_eq!(
        kernel.install_directory("/qemu-regular-parent/child"),
        Err("enotdir")
    );
    assert!(matches!(
        kernel.open_regular_node(
            "/qemu-regular-parent/open-child",
            CreateDisposition::CreateIfMissing,
        ),
        Err("enotdir")
    ));
    assert!(matches!(
        kernel.lookup_file_node("/qemu-regular-parent/child"),
        Err("enotdir")
    ));
}

// AGENT: verify that pathname lookup walks valid aliases in order, crosses
// mounts, returns one shared inode-like FileNode, and reports absence.
#[cfg_attr(test, test)]
fn pathname_lookup_returns_shared_file_node(kernel: &Kernel) {
    kernel
        .install_file("/tmp/qemu-lookup", b"node".to_vec(), false)
        .expect("lookup test file should install");
    kernel
        .install_directory("/tmp/qemu-lookup-alias-dir")
        .expect("lookup alias directory should install");

    let direct = kernel
        .lookup_file_node("/tmp/qemu-lookup")
        .expect("canonical lookup path should resolve");
    let alias = kernel
        .lookup_file_node("/tmp//qemu-lookup-alias-dir/.././qemu-lookup")
        .expect("component-walk lookup alias should resolve");
    assert_eq!(direct.display_path, "/tmp/qemu-lookup");
    assert_eq!(alias.display_path, direct.display_path);
    assert!(Arc::ptr_eq(&direct.path_ref.node, &alias.path_ref.node));
    assert!(matches!(
        kernel.lookup_file_node("/tmp/qemu-lookup-missing"),
        Err("enoent")
    ));

    kernel
        .install_directory("/lookup-mnt")
        .expect("lookup test mountpoint should install");
    let mounted_fs = kernel.vfs.new_filesystem(FileStorage::standalone());
    kernel
        .vfs
        .attach("/lookup-mnt", mounted_fs, MountFlags::empty())
        .expect("lookup test mount should attach");
    kernel
        .install_file("/lookup-mnt/file", b"mounted".to_vec(), false)
        .expect("mounted lookup test file should install");
    let mounted = kernel
        .lookup_file_node("/lookup-mnt/./file")
        .expect("mounted lookup path should resolve");
    assert_eq!(mounted.display_path, "/lookup-mnt/file");
    kernel
        .vfs
        .detach_top("/lookup-mnt")
        .expect("lookup test mount should uninstall");
}

// AGENT: prove both the open-file constructor and exec snapshot select storage
// from ResolvedPath::path_ref rather than from the root Kernel filesystem.
#[cfg_attr(test, test)]
fn mounted_open_and_exec_use_the_mounted_filesystem_storage(kernel: &Kernel) {
    kernel
        .install_directory("/storage-mnt")
        .expect("storage test mountpoint should install");
    let root_storage = kernel.vfs.root_fs().storage().clone();
    let mounted_storage = FileStorage::standalone();
    let mounted_fs = kernel.vfs.new_filesystem(mounted_storage.clone());
    kernel
        .vfs
        .attach("/storage-mnt", mounted_fs.clone(), MountFlags::empty())
        .expect("storage test filesystem should attach");

    kernel
        .install_file("/storage-mnt/file", b"mounted-open".to_vec(), false)
        .expect("mounted regular file should install");
    let resolved = kernel
        .lookup_file_node("/storage-mnt/file")
        .expect("mounted regular file should resolve");
    assert!(Arc::ptr_eq(resolved.path_ref.mount.fs(), &mounted_fs));
    let instance = resolved.path_ref;
    assert!(Arc::ptr_eq(instance.mount.fs(), &mounted_fs));
    assert!(instance.storage().shares_backend_with(&mounted_storage));
    assert!(!instance.storage().shares_backend_with(&root_storage));
    let mut bytes = [0u8; 12];
    assert_eq!(instance.read_at(0, &mut bytes), Ok(bytes.len()));
    assert_eq!(&bytes, b"mounted-open");

    kernel
        .install_exec_file("/storage-mnt/exec", b"mounted-exec".to_vec())
        .expect("mounted exec fixture should install");
    let (display_path, exec_bytes) = kernel
        .read_file_for_exec("/storage-mnt/exec")
        .expect("exec should read through the mounted filesystem storage");
    assert_eq!(display_path, "/storage-mnt/exec");
    assert_eq!(exec_bytes, b"mounted-exec");

    kernel
        .vfs
        .detach_top("/storage-mnt")
        .expect("storage test filesystem should detach");
    let mut bytes_after_detach = [0u8; 12];
    assert_eq!(
        instance.read_at(0, &mut bytes_after_detach),
        Ok(bytes_after_detach.len())
    );
    assert_eq!(&bytes_after_detach, b"mounted-open");
}

// AGENT: verify the live mkdirat ABI, strict EEXIST behavior, parent errors,
// absolute-path-only contract, usercopy, and parent directory bookkeeping.
#[cfg_attr(test, test)]
fn mkdirat_creates_only_new_absolute_directories(kernel: &Kernel) {
    assert_eq!(map_riscv_nr(RISCV_SYS_MKDIRAT), Some(INTERNAL_SYS_MKDIRAT));

    let task = kernel.cur_task(0).expect("init task should be current");
    task.process
        .addr_space
        .lock()
        .unwrap()
        .map_region(
            VmRegion::new(MKDIR_USER_BASE, PAGE_SZ, VM_READ | VM_WRITE),
            &kernel.pool,
        )
        .expect("mkdirat user page should map");

    let parent_addr = MKDIR_USER_BASE;
    let child_addr = MKDIR_USER_BASE + 128;
    let missing_parent_addr = MKDIR_USER_BASE + 256;
    let regular_addr = MKDIR_USER_BASE + 384;
    let below_regular_addr = MKDIR_USER_BASE + 512;
    let relative_addr = MKDIR_USER_BASE + 640;
    let empty_addr = MKDIR_USER_BASE + 768;
    write_user_string(kernel, &task, parent_addr, "/qemu-mkdirat-parent");
    write_user_string(kernel, &task, child_addr, "/qemu-mkdirat-parent/child");
    write_user_string(
        kernel,
        &task,
        missing_parent_addr,
        "/qemu-mkdirat-missing/child",
    );
    write_user_string(kernel, &task, regular_addr, "/qemu-mkdirat-regular");
    write_user_string(
        kernel,
        &task,
        below_regular_addr,
        "/qemu-mkdirat-regular/child",
    );
    write_user_string(kernel, &task, relative_addr, "relative-mkdirat");
    write_user_string(kernel, &task, empty_addr, "");

    assert_eq!(
        kernel.dispatch_syscall_without_signal_delivery(
            SYS_MKDIRAT,
            usize::MAX - 99,
            parent_addr,
            0o750,
            0,
            0,
            0,
        ),
        Ok(0)
    );
    assert_eq!(
        kernel.dispatch_syscall_without_signal_delivery(
            SYS_MKDIRAT,
            usize::MAX - 99,
            child_addr,
            0o700,
            0,
            0,
            0,
        ),
        Ok(0)
    );
    let parent = kernel
        .lookup_file_node("/qemu-mkdirat-parent")
        .expect("mkdirat parent should exist");
    let child = kernel
        .lookup_file_node("/qemu-mkdirat-parent/child")
        .expect("mkdirat child should exist");
    assert_eq!(parent.path_ref.node.kind, FileKind::Directory);
    assert_eq!(
        parent.path_ref.node.dir_entry_at(0),
        Ok(String::from("child"))
    );
    assert_eq!(child.path_ref.node.kind, FileKind::Directory);

    assert_eq!(
        kernel.dispatch_syscall_without_signal_delivery(
            SYS_MKDIRAT,
            usize::MAX - 99,
            child_addr,
            0o700,
            0,
            0,
            0,
        ),
        Err("eexist")
    );
    assert_eq!(
        kernel.dispatch_syscall_without_signal_delivery(
            SYS_MKDIRAT,
            usize::MAX - 99,
            missing_parent_addr,
            0o700,
            0,
            0,
            0,
        ),
        Err("enoent")
    );

    kernel
        .install_file("/qemu-mkdirat-regular", Vec::new(), false)
        .expect("mkdirat regular-file fixture should install");
    assert_eq!(
        kernel.dispatch_syscall_without_signal_delivery(
            SYS_MKDIRAT,
            usize::MAX - 99,
            regular_addr,
            0o700,
            0,
            0,
            0,
        ),
        Err("eexist")
    );
    assert_eq!(
        kernel.dispatch_syscall_without_signal_delivery(
            SYS_MKDIRAT,
            usize::MAX - 99,
            below_regular_addr,
            0o700,
            0,
            0,
            0,
        ),
        Err("enotdir")
    );
    assert_eq!(
        kernel.dispatch_syscall_without_signal_delivery(
            SYS_MKDIRAT,
            usize::MAX - 99,
            relative_addr,
            0o700,
            0,
            0,
            0,
        ),
        Err("enotsup")
    );
    assert_eq!(
        kernel.dispatch_syscall_without_signal_delivery(
            SYS_MKDIRAT,
            usize::MAX - 99,
            empty_addr,
            0o700,
            0,
            0,
            0,
        ),
        Err("enoent")
    );
    assert_eq!(
        kernel.dispatch_syscall_without_signal_delivery(
            SYS_MKDIRAT,
            usize::MAX - 99,
            MKDIR_USER_BASE + PAGE_SZ * 2,
            0o700,
            0,
            0,
            0,
        ),
        Err("efault")
    );
}

// AGENT: verify the live openat ABI, OFD flags, independent open offsets,
// atomic create errors, narrow absolute-path contract, and EMFILE rollback.
#[cfg_attr(test, test)]
fn openat_uses_transactional_fd_and_path_state(kernel: &Kernel) {
    assert_eq!(map_riscv_nr(RISCV_SYS_OPENAT), Some(INTERNAL_SYS_OPENAT));

    let task = kernel.cur_task(0).expect("init task should be current");
    task.process
        .addr_space
        .lock()
        .unwrap()
        .map_region(
            VmRegion::new(OPEN_USER_BASE, PAGE_SZ, VM_READ | VM_WRITE),
            &kernel.pool,
        )
        .expect("openat user page should map");

    let path_addr = OPEN_USER_BASE;
    let relative_addr = OPEN_USER_BASE + 128;
    let directory_addr = OPEN_USER_BASE + 256;
    let child_addr = OPEN_USER_BASE + 384;
    let truncate_addr = OPEN_USER_BASE + 512;
    write_user_string(kernel, &task, path_addr, "/tmp/qemu-openat");
    write_user_string(kernel, &task, relative_addr, "tmp/relative");
    write_user_string(kernel, &task, directory_addr, "/tmp/qemu-dir");
    write_user_string(kernel, &task, child_addr, "/tmp/qemu-parent/child");
    write_user_string(kernel, &task, truncate_addr, "/tmp/qemu-emfile");

    let open_flags = O_CREAT | O_CLOEXEC | O_APPEND | O_NONBLOCK | 1;
    let fd = kernel
        .dispatch_syscall_without_signal_delivery(
            SYS_OPENAT,
            usize::MAX - 99,
            path_addr,
            open_flags,
            0o640,
            0,
            0,
        )
        .expect("absolute openat should ignore dirfd and create a file");
    let entry = task
        .get_fd_entry(fd)
        .expect("openat fd should be installed");
    let status = entry.status_flags();
    assert!(!status.rd);
    assert!(status.wr);
    assert!(status.ap);
    assert!(status.nb);
    assert!(entry.is_cloexec());
    assert_eq!(entry.write(b"abc"), Ok(3));

    let second_fd = kernel
        .dispatch_syscall_without_signal_delivery(SYS_OPENAT, 0, path_addr, 0, 0, 0, 0)
        .expect("a second open should create a fresh OFD");
    let second = task
        .get_fd_entry(second_fd)
        .expect("second openat fd should be installed");
    let mut bytes = [0u8; 3];
    assert_eq!(second.read(&mut bytes), Ok(3));
    assert_eq!(&bytes, b"abc");
    assert_eq!(
        kernel.dispatch_syscall_without_signal_delivery(
            SYS_OPENAT,
            0,
            path_addr,
            O_CREAT | O_EXCL | 1,
            0o600,
            0,
            0,
        ),
        Err("eexist")
    );
    assert_eq!(
        kernel.dispatch_syscall_without_signal_delivery(
            SYS_OPENAT,
            0,
            path_addr,
            O_EXCL | 1,
            0,
            0,
            0,
        ),
        Err("einval")
    );

    task.close_fd(fd).expect("first openat fd should close");
    task.close_fd(second_fd)
        .expect("second openat fd should close");
    let trunc_fd = kernel
        .dispatch_syscall_without_signal_delivery(SYS_OPENAT, 0, path_addr, O_TRUNC | 1, 0, 0, 0)
        .expect("writable O_TRUNC should reopen the file");
    assert_eq!(
        kernel
            .lookup_file_node("/tmp/qemu-openat")
            .expect("truncated file should remain registered")
            .path_ref
            .node
            .len(),
        0
    );
    task.close_fd(trunc_fd)
        .expect("truncating openat fd should close");

    assert_eq!(
        kernel.dispatch_syscall_without_signal_delivery(
            SYS_OPENAT,
            usize::MAX - 99,
            relative_addr,
            O_CREAT | 1,
            0o600,
            0,
            0,
        ),
        Err("enotsup")
    );
    assert_eq!(
        kernel.dispatch_syscall_without_signal_delivery(
            SYS_OPENAT,
            0,
            path_addr,
            O_CREAT | 3,
            0,
            0,
            0,
        ),
        Err("einval")
    );
    assert_eq!(
        kernel.dispatch_syscall_without_signal_delivery(
            SYS_OPENAT,
            0,
            path_addr,
            O_CREAT | 0x100,
            0,
            0,
            0,
        ),
        Err("enotsup")
    );

    kernel
        .install_directory("/tmp/qemu-dir")
        .expect("directory test node should install");
    assert_eq!(
        kernel.dispatch_syscall_without_signal_delivery(SYS_OPENAT, 0, directory_addr, 0, 0, 0, 0,),
        Err("eisdir")
    );
    kernel
        .install_file("/tmp/qemu-parent", Vec::new(), false)
        .expect("regular parent test node should install");
    assert_eq!(
        kernel.dispatch_syscall_without_signal_delivery(
            SYS_OPENAT,
            0,
            child_addr,
            O_CREAT | 1,
            0o600,
            0,
            0,
        ),
        Err("enotdir")
    );

    kernel
        .install_file("/tmp/qemu-emfile", b"keep".to_vec(), false)
        .expect("EMFILE truncation target should install");
    let mut fillers = Vec::new();
    loop {
        match task.add_file(FLike::Tty(TtyDevice)) {
            Ok(fd) => fillers.push(fd),
            Err("emfile") => break,
            Err(err) => panic!("unexpected fd fill error: {err}"),
        }
    }
    assert_eq!(
        kernel.dispatch_syscall_without_signal_delivery(
            SYS_OPENAT,
            0,
            truncate_addr,
            O_TRUNC | 1,
            0,
            0,
            0,
        ),
        Err("emfile")
    );
    assert_eq!(
        kernel
            .lookup_file_node("/tmp/qemu-emfile")
            .expect("EMFILE target should remain registered")
            .path_ref
            .node
            .len(),
        4
    );
    for fd in fillers {
        task.close_fd(fd).expect("filler fd should close");
    }
}
