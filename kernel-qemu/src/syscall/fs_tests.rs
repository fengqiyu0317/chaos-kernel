// AGENT: QEMU filesystem syscall regressions that require the installed frame
// pool, current init task, Sv39 mappings, and authoritative AddrSpace usercopy.
use super::*;
use crate::syscall_abi::{
    map_riscv_nr, INTERNAL_SYS_DUP, INTERNAL_SYS_DUP3, INTERNAL_SYS_FSTAT, INTERNAL_SYS_MKDIRAT,
    INTERNAL_SYS_MOUNT, INTERNAL_SYS_NEWFSTATAT, INTERNAL_SYS_OPENAT, INTERNAL_SYS_PIPE,
    INTERNAL_SYS_READ, INTERNAL_SYS_UMOUNT2, RISCV_SYS_DUP, RISCV_SYS_DUP3, RISCV_SYS_FSTAT,
    RISCV_SYS_MKDIRAT, RISCV_SYS_MOUNT, RISCV_SYS_NEWFSTATAT, RISCV_SYS_OPENAT, RISCV_SYS_PIPE2,
    RISCV_SYS_READ, RISCV_SYS_UMOUNT2,
};

const USER_STRINGS_BASE: usize = 0x4000_0000;
const OPEN_USER_BASE: usize = 0x4001_0000;
const MKDIR_USER_BASE: usize = 0x4002_0000;
const READ_USER_BASE: usize = 0x4003_0000;
const UNMOUNT_USER_BASE: usize = 0x4004_0000;
const STAT_USER_BASE: usize = 0x4005_0000;
const PIPE_USER_BASE: usize = 0x4006_0000;

// AGENT: Run filesystem ABI and semantic regressions after QEMU installs the
// real kernel frame pool, current init task, Sv39 mappings, and fd table.
pub fn run_all(kernel: &Kernel) {
    kernel
        .install_directory("/tmp")
        .expect("filesystem selftests require /tmp");
    mount_and_umount2_use_usercopy_and_mutate_mount_table(kernel);
    stat_syscalls_copy_real_inode_attributes_to_userspace(kernel);
    umount2_enforces_busy_close_and_lazy_subtree_lifecycles(kernel);
    path_creation_requires_an_existing_directory_parent(kernel);
    pathname_lookup_returns_shared_file_node(kernel);
    mounted_open_and_exec_use_the_mounted_filesystem_storage(kernel);
    mkdirat_creates_only_new_absolute_directories(kernel);
    openat_uses_transactional_fd_and_path_state(kernel);
    pipe2_copies_fds_and_publishes_them_transactionally(kernel);
    dup3_uses_the_shared_exact_target_implementation(kernel);
    read_uses_usercopy_and_shared_open_file_offsets(kernel);
    read_moves_pipe_bytes_and_reports_empty_states(kernel);
    write_to_pipe_without_readers_returns_epipe_and_queues_sigpipe(kernel);
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

// AGENT: decode the two native-width Linux int descriptors copied out by the
// RV64 pipe2 syscall instead of relying on the removed packed return value.
fn read_user_pipe_fds(task: &Task, addr: usize) -> (usize, usize) {
    let fd_size = mem::size_of::<i32>();
    let mut bytes = [0u8; 2 * mem::size_of::<i32>()];
    task.process
        .addr_space
        .lock()
        .unwrap()
        .read_user_bytes(addr, &mut bytes)
        .expect("pipe2 descriptor pair should be readable from userspace");
    let read_fd = i32::from_ne_bytes(
        bytes[..fd_size]
            .try_into()
            .expect("pipe2 read fd should occupy one int"),
    );
    let write_fd = i32::from_ne_bytes(
        bytes[fd_size..]
            .try_into()
            .expect("pipe2 write fd should occupy one int"),
    );
    (
        usize::try_from(read_fd).expect("pipe2 read fd should be non-negative"),
        usize::try_from(write_fd).expect("pipe2 write fd should be non-negative"),
    )
}

// AGENT: decode one little-endian u32 from the fixed RV64 stat regression image.
fn stat_u32(bytes: &[u8; RISCV64_STAT_SIZE], offset: usize) -> u32 {
    u32::from_le_bytes(
        bytes[offset..offset + mem::size_of::<u32>()]
            .try_into()
            .expect("stat u32 field should fit"),
    )
}

// AGENT: decode one little-endian u64 from the fixed RV64 stat regression image.
fn stat_u64(bytes: &[u8; RISCV64_STAT_SIZE], offset: usize) -> u64 {
    u64::from_le_bytes(
        bytes[offset..offset + mem::size_of::<u64>()]
            .try_into()
            .expect("stat u64 field should fit"),
    )
}

// AGENT: copy one complete stat image back through AddrSpace so assertions audit
// the same Sv39 usercopy result returned to a real user ecall.
fn read_user_stat(task: &Task, addr: usize) -> [u8; RISCV64_STAT_SIZE] {
    let mut bytes = [0u8; RISCV64_STAT_SIZE];
    task.process
        .addr_space
        .lock()
        .unwrap()
        .read_user_bytes(addr, &mut bytes)
        .expect("stat result should be readable");
    bytes
}

// AGENT: prove RV64 fstat/newfstatat reach distinct entries, return live ChaosFs
// identity and size fields, and reject every unsupported or invalid user boundary.
#[cfg_attr(test, test)]
fn stat_syscalls_copy_real_inode_attributes_to_userspace(kernel: &Kernel) {
    assert_eq!(map_riscv_nr(RISCV_SYS_FSTAT), Some(INTERNAL_SYS_FSTAT));
    assert_eq!(
        map_riscv_nr(RISCV_SYS_NEWFSTATAT),
        Some(INTERNAL_SYS_NEWFSTATAT)
    );

    let task = kernel.cur_task(0).expect("init task should be current");
    {
        let mut addr_space = task.process.addr_space.lock().unwrap();
        addr_space
            .map_region(
                VmRegion::new(STAT_USER_BASE, PAGE_SZ, VM_READ | VM_WRITE),
                &kernel.pool,
            )
            .expect("stat read-write user page should map");
        addr_space
            .map_region(
                VmRegion::new(STAT_USER_BASE + PAGE_SZ * 2, PAGE_SZ, VM_READ),
                &kernel.pool,
            )
            .expect("stat read-only user page should map");
    }

    let path_addr = STAT_USER_BASE;
    let relative_addr = STAT_USER_BASE + 64;
    let missing_addr = STAT_USER_BASE + 96;
    let directory_addr = STAT_USER_BASE + 128;
    let stat_addr = STAT_USER_BASE + 256;
    write_user_string(kernel, &task, path_addr, "/tmp/qemu-stat");
    write_user_string(kernel, &task, relative_addr, "relative-stat");
    write_user_string(kernel, &task, missing_addr, "/tmp/qemu-stat-missing");
    write_user_string(kernel, &task, directory_addr, "/tmp");
    kernel
        .install_file("/tmp/qemu-stat", vec![0x5a; 513], false)
        .expect("stat fixture should install");

    let resolved = kernel
        .vfs
        .resolve("/tmp/qemu-stat")
        .expect("stat fixture should resolve");
    let expected_dev = resolved.path_ref.mount.fs().id() as u64;
    let expected_ino = resolved.path_ref.node.id();
    assert_eq!(
        kernel.dispatch_syscall_without_signal_delivery(
            SYS_NEWFSTATAT,
            usize::MAX - 99,
            path_addr,
            stat_addr,
            0,
            0,
            0,
        ),
        Ok(0)
    );
    let path_stat = read_user_stat(&task, stat_addr);
    assert_eq!(stat_u64(&path_stat, 0), expected_dev);
    assert_eq!(stat_u64(&path_stat, 8), expected_ino);
    assert_eq!(stat_u32(&path_stat, 16) & S_IFMT, S_IFREG);
    assert_eq!(stat_u32(&path_stat, 20), 1);
    assert_eq!(stat_u32(&path_stat, 24), 0);
    assert_eq!(stat_u32(&path_stat, 28), 0);
    assert_eq!(stat_u64(&path_stat, 32), 0);
    assert!(path_stat[40..48].iter().all(|byte| *byte == 0));
    assert_eq!(stat_u64(&path_stat, 48), 513);
    assert_eq!(stat_u32(&path_stat, 56), BLOCK_CACHE_BLOCK_SIZE as u32);
    assert!(path_stat[60..64].iter().all(|byte| *byte == 0));
    assert_eq!(stat_u64(&path_stat, 64), 2);
    assert!(path_stat[72..].iter().all(|byte| *byte == 0));

    let fd = kernel
        .dispatch_syscall_without_signal_delivery(SYS_OPENAT, 0, path_addr, 0, 0, 0, 0)
        .expect("stat fixture should open");
    task.process
        .addr_space
        .lock()
        .unwrap()
        .write_user_bytes(stat_addr, &[0xa5; RISCV64_STAT_SIZE], &kernel.pool)
        .expect("fstat result should start poisoned");
    assert_eq!(
        kernel.dispatch_syscall_without_signal_delivery(SYS_FSTAT, fd, stat_addr, 0, 0, 0, 0,),
        Ok(0)
    );
    assert_eq!(read_user_stat(&task, stat_addr), path_stat);

    assert_eq!(
        kernel.dispatch_syscall_without_signal_delivery(
            SYS_NEWFSTATAT,
            usize::MAX - 99,
            directory_addr,
            stat_addr,
            0,
            0,
            0,
        ),
        Ok(0)
    );
    let directory_stat = read_user_stat(&task, stat_addr);
    assert_eq!(stat_u32(&directory_stat, 16) & S_IFMT, S_IFDIR);
    assert_eq!(stat_u64(&directory_stat, 48), 0);

    assert_eq!(
        kernel.dispatch_syscall_without_signal_delivery(
            SYS_NEWFSTATAT,
            usize::MAX - 99,
            missing_addr,
            stat_addr,
            0,
            0,
            0,
        ),
        Err("enoent")
    );
    assert_eq!(
        kernel.dispatch_syscall_without_signal_delivery(
            SYS_NEWFSTATAT,
            usize::MAX - 99,
            relative_addr,
            stat_addr,
            0,
            0,
            0,
        ),
        Err("enotsup")
    );
    assert_eq!(
        kernel.dispatch_syscall_without_signal_delivery(
            SYS_NEWFSTATAT,
            usize::MAX - 99,
            path_addr,
            stat_addr,
            1,
            0,
            0,
        ),
        Err("enotsup")
    );
    assert_eq!(
        kernel.dispatch_syscall_without_signal_delivery(SYS_FSTAT, MAX_FD, stat_addr, 0, 0, 0, 0,),
        Err("ebadf")
    );
    assert_eq!(
        kernel.dispatch_syscall_without_signal_delivery(SYS_FSTAT, fd, 0, 0, 0, 0, 0),
        Err("efault")
    );
    assert_eq!(
        kernel.dispatch_syscall_without_signal_delivery(
            SYS_FSTAT,
            fd,
            STAT_USER_BASE + PAGE_SZ,
            0,
            0,
            0,
            0,
        ),
        Err("efault")
    );
    assert_eq!(
        kernel.dispatch_syscall_without_signal_delivery(
            SYS_FSTAT,
            fd,
            STAT_USER_BASE + PAGE_SZ * 2,
            0,
            0,
            0,
            0,
        ),
        Err("efault")
    );

    let partial_addr = STAT_USER_BASE + PAGE_SZ - 64;
    task.process
        .addr_space
        .lock()
        .unwrap()
        .write_user_bytes(partial_addr, &[0x3c; 64], &kernel.pool)
        .expect("partial stat prefix should start poisoned");
    assert_eq!(
        kernel.dispatch_syscall_without_signal_delivery(SYS_FSTAT, fd, partial_addr, 0, 0, 0, 0,),
        Err("efault")
    );
    let mut partial = [0u8; 64];
    task.process
        .addr_space
        .lock()
        .unwrap()
        .read_user_bytes(partial_addr, &mut partial)
        .expect("failed stat should leave the writable prefix readable");
    assert_eq!(partial, [0x3c; 64]);

    let (read_end, write_end) = PipeNode::pair();
    let (read_fd, write_fd) = task
        .add_file_pair_with_cloexec(FLike::Pipe(read_end), FLike::Pipe(write_end), false)
        .expect("stat pipe fixture should allocate descriptors");
    assert_eq!(
        kernel.dispatch_syscall_without_signal_delivery(SYS_FSTAT, read_fd, stat_addr, 0, 0, 0, 0,),
        Err("enotsup")
    );
    task.close_fd(read_fd)
        .expect("stat pipe reader should close");
    task.close_fd(write_fd)
        .expect("stat pipe writer should close");
    task.close_fd(fd).expect("stat fixture fd should close");
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
    let first_attr = FInstance::new(first_mount.clone(), dev0_file.clone())
        .file_attr()
        .expect("first attachment should expose file attributes");

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
    let repeated_attr = FInstance::new(second_mount.clone(), remounted_file.clone())
        .file_attr()
        .expect("repeated attachment should expose file attributes");
    assert_eq!(repeated_attr.dev, first_attr.dev);
    assert_eq!(repeated_attr.ino, first_attr.ino);
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
    let dev1_attr = FInstance::new(dev1_mount.clone(), dev1_mount.fs().root())
        .file_attr()
        .expect("distinct filesystem should expose root attributes");
    assert_ne!(dev1_attr.dev, first_attr.dev);
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
    drop(revealed);

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
    for unsupported in [0x1, 0x4, 0x8, 0x10, MNT_DETACH | 0x1] {
        assert_eq!(
            kernel.dispatch_syscall_without_signal_delivery(
                SYS_UMOUNT2,
                target_addr,
                unsupported,
                0,
                0,
                0,
                0,
            ),
            Err("enotsup")
        );
        assert_eq!(kernel.vfs.mounts.mount_count(), 1);
    }
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

// AGENT: exercise the user-visible distinction between busy-checked ordinary
// unmount and subtree-wide MNT_DETACH while preserving open-file usability.
#[cfg_attr(test, test)]
fn umount2_enforces_busy_close_and_lazy_subtree_lifecycles(kernel: &Kernel) {
    let task = kernel.cur_task(0).expect("init task should be current");
    task.process
        .addr_space
        .lock()
        .unwrap()
        .map_region(
            VmRegion::new(UNMOUNT_USER_BASE, PAGE_SZ, VM_READ | VM_WRITE),
            &kernel.pool,
        )
        .expect("unmount lifecycle user page should map");
    let target_addr = UNMOUNT_USER_BASE;
    let file_addr = UNMOUNT_USER_BASE + 128;
    let baseline = kernel.vfs.mounts.mount_count();

    kernel
        .install_directory("/umount-busy")
        .expect("busy-unmount mountpoint should install");
    let busy_fs = kernel.vfs.new_filesystem(FileStorage::standalone());
    kernel
        .vfs
        .attach("/umount-busy", busy_fs, MountFlags::empty())
        .expect("busy-unmount filesystem should attach");
    kernel
        .install_file("/umount-busy/file", b"busy".to_vec(), false)
        .expect("busy-unmount file should install");
    write_user_string(kernel, &task, file_addr, "/umount-busy/file");
    let busy_fd = kernel
        .dispatch_syscall_without_signal_delivery(SYS_OPENAT, 0, file_addr, 0, 0, 0, 0)
        .expect("open file should pin its mount");
    write_user_string(kernel, &task, target_addr, "/umount-busy");

    assert_eq!(
        kernel.dispatch_syscall_without_signal_delivery(SYS_UMOUNT2, target_addr, 0, 0, 0, 0, 0,),
        Err("ebusy")
    );
    assert_eq!(kernel.vfs.mounts.mount_count(), baseline + 1);
    assert!(kernel.lookup_file_node("/umount-busy/file").is_ok());
    assert_eq!(
        kernel.dispatch_syscall_without_signal_delivery(SYS_CLOSE, busy_fd, 0, 0, 0, 0, 0),
        Ok(0)
    );
    assert_eq!(
        kernel.dispatch_syscall_without_signal_delivery(SYS_UMOUNT2, target_addr, 0, 0, 0, 0, 0,),
        Ok(0)
    );
    assert_eq!(kernel.vfs.mounts.mount_count(), baseline);

    kernel
        .install_directory("/umount-lazy")
        .expect("lazy-unmount mountpoint should install");
    let parent_fs = kernel.vfs.new_filesystem(FileStorage::standalone());
    parent_fs
        .create_directory_at(
            &parent_fs.root(),
            ChildName::new("sub").expect("sub should be a valid child name"),
        )
        .expect("lazy-unmount child mountpoint should install");
    kernel
        .vfs
        .attach("/umount-lazy", parent_fs, MountFlags::empty())
        .expect("lazy-unmount parent filesystem should attach");
    let child_fs = kernel.vfs.new_filesystem(FileStorage::standalone());
    kernel
        .vfs
        .attach("/umount-lazy/sub", child_fs, MountFlags::empty())
        .expect("lazy-unmount child filesystem should attach");
    kernel
        .install_file("/umount-lazy/sub/file", b"lazy-open".to_vec(), false)
        .expect("lazy-unmount file should install");
    write_user_string(kernel, &task, file_addr, "/umount-lazy/sub/file");
    let lazy_fd = kernel
        .dispatch_syscall_without_signal_delivery(SYS_OPENAT, 0, file_addr, 0, 0, 0, 0)
        .expect("open child file should pin the descendant mount");
    write_user_string(kernel, &task, target_addr, "/umount-lazy");
    assert_eq!(kernel.vfs.mounts.mount_count(), baseline + 2);

    assert_eq!(
        kernel.dispatch_syscall_without_signal_delivery(
            SYS_UMOUNT2,
            target_addr,
            MNT_DETACH,
            0,
            0,
            0,
            0,
        ),
        Ok(0)
    );
    assert_eq!(kernel.vfs.mounts.mount_count(), baseline);
    assert!(matches!(
        kernel.lookup_file_node("/umount-lazy/sub/file"),
        Err("enoent")
    ));
    let entry = task
        .get_fd_entry(lazy_fd)
        .expect("lazy-detached open fd should remain installed");
    let mut bytes = [0u8; 9];
    assert_eq!(entry.read(task.id(), &mut bytes), Ok(bytes.len()));
    assert_eq!(&bytes, b"lazy-open");
    drop(entry);
    assert_eq!(
        kernel.dispatch_syscall_without_signal_delivery(SYS_CLOSE, lazy_fd, 0, 0, 0, 0, 0),
        Ok(0)
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
    drop(mounted);
    kernel
        .vfs
        .unmount("/lookup-mnt", UnmountMode::Normal)
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
        .unmount("/storage-mnt", UnmountMode::Lazy)
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
    assert_eq!(
        entry.write(task.id(), b"abc"),
        Ok(FdWriteOutcome::Written(3))
    );

    let second_fd = kernel
        .dispatch_syscall_without_signal_delivery(SYS_OPENAT, 0, path_addr, 0, 0, 0, 0)
        .expect("a second open should create a fresh OFD");
    let second = task
        .get_fd_entry(second_fd)
        .expect("second openat fd should be installed");
    let mut bytes = [0u8; 3];
    assert_eq!(second.read(task.id(), &mut bytes), Ok(3));
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

// AGENT: verify RV64 pipe2 mapping, int[2] copy-out, initial OFD/fd flags,
// invalid-user/flag rejection, pending-pair cancellation, and EMFILE rollback.
#[cfg_attr(test, test)]
fn pipe2_copies_fds_and_publishes_them_transactionally(kernel: &Kernel) {
    assert_eq!(map_riscv_nr(RISCV_SYS_PIPE2), Some(INTERNAL_SYS_PIPE));

    let task = kernel.cur_task(0).expect("init task should be current");
    {
        let mut addr_space = task.process.addr_space.lock().unwrap();
        addr_space
            .map_region(
                VmRegion::new(PIPE_USER_BASE, PAGE_SZ, VM_READ | VM_WRITE),
                &kernel.pool,
            )
            .expect("pipe2 writable user page should map");
        addr_space
            .map_region(
                VmRegion::new(PIPE_USER_BASE + PAGE_SZ, PAGE_SZ, VM_READ),
                &kernel.pool,
            )
            .expect("pipe2 read-only user page should map");
    }

    assert_eq!(
        kernel.dispatch_syscall_without_signal_delivery(
            SYS_PIPE,
            PIPE_USER_BASE,
            O_NONBLOCK | O_CLOEXEC,
            0,
            0,
            0,
            0,
        ),
        Ok(0)
    );
    let first_pair = read_user_pipe_fds(&task, PIPE_USER_BASE);
    let read_entry = task
        .get_fd_entry(first_pair.0)
        .expect("pipe2 read fd should be installed");
    let read_status = read_entry.status_flags();
    assert!(read_status.rd);
    assert!(!read_status.wr);
    assert!(read_status.nb);
    assert!(read_entry.is_cloexec());
    let write_entry = task
        .get_fd_entry(first_pair.1)
        .expect("pipe2 write fd should be installed");
    let write_status = write_entry.status_flags();
    assert!(!write_status.rd);
    assert!(write_status.wr);
    assert!(write_status.nb);
    assert!(write_entry.is_cloexec());
    task.close_fd(first_pair.0)
        .expect("pipe2 read fd should close");
    task.close_fd(first_pair.1)
        .expect("pipe2 write fd should close");

    assert_eq!(
        kernel.dispatch_syscall_without_signal_delivery(
            SYS_PIPE,
            PIPE_USER_BASE,
            O_APPEND,
            0,
            0,
            0,
            0,
        ),
        Err("einval")
    );
    for bad_addr in [
        0,
        PIPE_USER_BASE + PAGE_SZ,
        PIPE_USER_BASE + PAGE_SZ - mem::size_of::<i32>(),
        PIPE_USER_BASE + 2 * PAGE_SZ,
    ] {
        assert_eq!(
            kernel.dispatch_syscall_without_signal_delivery(SYS_PIPE, bad_addr, 0, 0, 0, 0, 0,),
            Err("efault")
        );
    }

    assert_eq!(
        kernel.dispatch_syscall_without_signal_delivery(SYS_PIPE, PIPE_USER_BASE, 0, 0, 0, 0, 0,),
        Ok(0)
    );
    let reused_pair = read_user_pipe_fds(&task, PIPE_USER_BASE);
    assert_eq!(reused_pair, first_pair);
    task.close_fd(reused_pair.0)
        .expect("reused pipe2 read fd should close");
    task.close_fd(reused_pair.1)
        .expect("reused pipe2 write fd should close");

    let (failed_read, failed_write) = PipeNode::pair();
    let mut pending_pair = None;
    assert_eq!(
        task.add_file_pair_transaction(
            FdEntry::new(FLike::Pipe(failed_read)),
            FdEntry::new(FLike::Pipe(failed_write)),
            |read_fd, write_fd| {
                pending_pair = Some((read_fd, write_fd));
                Err("efault")
            },
        ),
        Err("efault")
    );
    let pending_pair = pending_pair.expect("failed transaction should reserve two fds");
    let (replacement_read, replacement_write) = PipeNode::pair();
    let replacement_pair = task
        .add_file_pair_with_cloexec(
            FLike::Pipe(replacement_read),
            FLike::Pipe(replacement_write),
            false,
        )
        .expect("cancelled pipe reservations should be reusable");
    assert_eq!(replacement_pair, pending_pair);
    task.close_fd(replacement_pair.0)
        .expect("replacement read fd should close");
    task.close_fd(replacement_pair.1)
        .expect("replacement write fd should close");

    let mut fillers = Vec::new();
    loop {
        match task.add_file(FLike::Tty(TtyDevice)) {
            Ok(fd) => fillers.push(fd),
            Err("emfile") => break,
            Err(err) => panic!("unexpected pipe2 fd fill error: {err}"),
        }
    }
    let only_free_fd = fillers.pop().expect("fd table should contain filler fds");
    task.close_fd(only_free_fd)
        .expect("one fd slot should be released for pair rollback");
    assert_eq!(
        kernel.dispatch_syscall_without_signal_delivery(SYS_PIPE, PIPE_USER_BASE, 0, 0, 0, 0, 0,),
        Err("emfile")
    );
    let recovered_fd = task
        .add_file(FLike::Tty(TtyDevice))
        .expect("failed pipe2 should return its first fd reservation");
    assert_eq!(recovered_fd, only_free_fd);
    task.close_fd(recovered_fd)
        .expect("recovered fd should close");
    for fd in fillers {
        task.close_fd(fd).expect("pipe2 filler fd should close");
    }
}

// AGENT: verify Linux RV64 dup3 mapping, exact-target replacement, OFD sharing,
// per-fd cloexec, and rejection ordering without exposing an unreachable dup2 ABI.
#[cfg_attr(test, test)]
fn dup3_uses_the_shared_exact_target_implementation(kernel: &Kernel) {
    assert_eq!(map_riscv_nr(RISCV_SYS_DUP3), Some(INTERNAL_SYS_DUP3));

    let task = kernel.cur_task(0).expect("init task should be current");
    let source_fd = task
        .add_file(FLike::Ep(EpInst::new()))
        .expect("dup3 source should allocate");
    task.set_cloexec(source_fd, true)
        .expect("dup3 source cloexec should set");

    assert_eq!(
        kernel
            .dispatch_syscall_without_signal_delivery(SYS_DUP3, source_fd, source_fd, 0, 0, 0, 0,),
        Err("einval")
    );

    let occupied_fd = task
        .add_file(FLike::Tty(TtyDevice))
        .expect("dup3 occupied target should allocate");
    let occupied_before = task
        .get_fd_entry(occupied_fd)
        .expect("dup3 occupied target should exist");
    assert_eq!(
        kernel.dispatch_syscall_without_signal_delivery(
            SYS_DUP3,
            source_fd,
            occupied_fd,
            O_NONBLOCK,
            0,
            0,
            0,
        ),
        Err("einval")
    );
    assert!(task
        .get_fd_entry(occupied_fd)
        .expect("invalid dup3 flags must preserve the target")
        .same_open_description(&occupied_before));

    assert_eq!(
        kernel.dispatch_syscall_without_signal_delivery(
            SYS_DUP3,
            source_fd,
            occupied_fd,
            O_CLOEXEC,
            0,
            0,
            0,
        ),
        Ok(occupied_fd)
    );
    let cloexec_target = task
        .get_fd_entry(occupied_fd)
        .expect("dup3 occupied target should be replaced");
    assert!(cloexec_target.same_open_description(
        &task
            .get_fd_entry(source_fd)
            .expect("dup3 source should remain installed")
    ));
    assert!(cloexec_target.is_cloexec());

    assert_eq!(
        kernel.dispatch_syscall_without_signal_delivery(
            SYS_DUP3,
            source_fd,
            occupied_fd,
            0,
            0,
            0,
            0,
        ),
        Ok(occupied_fd)
    );
    assert!(!task
        .get_fd_entry(occupied_fd)
        .expect("dup3 replacement without O_CLOEXEC should remain installed")
        .is_cloexec());

    let exact_fd = MAX_FD - 1;
    assert!(task.get_fd_entry(exact_fd).is_none());
    assert_eq!(
        kernel.dispatch_syscall_without_signal_delivery(
            SYS_DUP3, source_fd, exact_fd, O_CLOEXEC, 0, 0, 0,
        ),
        Ok(exact_fd)
    );
    let exact_entry = task
        .get_fd_entry(exact_fd)
        .expect("dup3 unused exact target should be installed");
    assert!(exact_entry.same_open_description(
        &task
            .get_fd_entry(source_fd)
            .expect("dup3 exact source should remain installed")
    ));
    assert!(exact_entry.is_cloexec());

    let exact_before = exact_entry.clone();
    assert_eq!(
        kernel.dispatch_syscall_without_signal_delivery(
            SYS_DUP3,
            source_fd + MAX_FD,
            exact_fd,
            0,
            0,
            0,
            0,
        ),
        Err("ebadf")
    );
    assert!(task
        .get_fd_entry(exact_fd)
        .expect("invalid dup3 source must preserve the target")
        .same_open_description(&exact_before));

    task.close_fd(source_fd).expect("dup3 source should close");
    task.close_fd(occupied_fd)
        .expect("dup3 occupied target should close");
    task.close_fd(exact_fd)
        .expect("dup3 exact target should close");
}

// AGENT: verify the live read/dup ABI, lowest-fd allocation, per-fd cloexec,
// shared OFD offsets, fd errors, EOF, and COW-safe copy-out.
#[cfg_attr(test, test)]
fn read_uses_usercopy_and_shared_open_file_offsets(kernel: &Kernel) {
    assert_eq!(map_riscv_nr(RISCV_SYS_READ), Some(INTERNAL_SYS_READ));
    assert_eq!(map_riscv_nr(RISCV_SYS_DUP), Some(INTERNAL_SYS_DUP));

    let task = kernel.cur_task(0).expect("init task should be current");
    task.process
        .addr_space
        .lock()
        .unwrap()
        .map_region(
            VmRegion::new(READ_USER_BASE, PAGE_SZ, VM_READ | VM_WRITE),
            &kernel.pool,
        )
        .expect("read syscall writable user page should map");
    task.process
        .addr_space
        .lock()
        .unwrap()
        .map_region(
            VmRegion::new(READ_USER_BASE + PAGE_SZ, PAGE_SZ, VM_READ),
            &kernel.pool,
        )
        .expect("read syscall read-only user page should map");

    let path_addr = READ_USER_BASE;
    let buf_addr = READ_USER_BASE + 512;
    let readonly_addr = READ_USER_BASE + PAGE_SZ;
    let partial_addr = READ_USER_BASE + PAGE_SZ - 2;
    let unmapped_addr = READ_USER_BASE + 2 * PAGE_SZ;
    write_user_string(kernel, &task, path_addr, "/tmp/qemu-sys-read");
    kernel
        .install_file("/tmp/qemu-sys-read", b"abcdef".to_vec(), false)
        .expect("read syscall fixture should install");

    let fd = kernel
        .dispatch_syscall_without_signal_delivery(SYS_OPENAT, 0, path_addr, 0, 0, 0, 0)
        .expect("read syscall fixture should open");
    assert_eq!(
        kernel.dispatch_syscall_without_signal_delivery(SYS_READ, fd, buf_addr, 3, 0, 0, 0),
        Ok(3)
    );
    let mut bytes = [0u8; 3];
    task.process
        .addr_space
        .lock()
        .unwrap()
        .read_user_bytes(buf_addr, &mut bytes)
        .expect("first read result should be copied to userspace");
    assert_eq!(&bytes, b"abc");

    let lowest_free_fd = task
        .add_file(FLike::Tty(TtyDevice))
        .expect("dup lowest-fd probe should allocate");
    task.close_fd(lowest_free_fd)
        .expect("dup lowest-fd probe should close");
    task.set_cloexec(fd, true)
        .expect("dup source should accept FD_CLOEXEC");
    let dup_fd = kernel
        .dispatch_syscall_without_signal_delivery(SYS_DUP, fd, 0, 0, 0, 0, 0)
        .expect("dup should share the read open-file description");
    assert_eq!(dup_fd, lowest_free_fd);
    let source_entry = task
        .get_fd_entry(fd)
        .expect("dup source should remain installed");
    let dup_entry = task
        .get_fd_entry(dup_fd)
        .expect("dup target should be installed");
    assert!(source_entry.same_open_description(&dup_entry));
    assert!(source_entry.is_cloexec());
    assert!(!dup_entry.is_cloexec());
    assert_eq!(
        kernel.dispatch_syscall_without_signal_delivery(SYS_READ, dup_fd, buf_addr, 3, 0, 0, 0),
        Ok(3)
    );
    task.process
        .addr_space
        .lock()
        .unwrap()
        .read_user_bytes(buf_addr, &mut bytes)
        .expect("dup read result should be copied to userspace");
    assert_eq!(&bytes, b"def");
    assert_eq!(
        kernel.dispatch_syscall_without_signal_delivery(SYS_READ, fd, buf_addr, 1, 0, 0, 0),
        Ok(0)
    );

    let missing_fd = task
        .add_file(FLike::Tty(TtyDevice))
        .expect("missing dup source probe should allocate");
    task.close_fd(missing_fd)
        .expect("missing dup source probe should close");
    assert_eq!(
        kernel.dispatch_syscall_without_signal_delivery(SYS_DUP, missing_fd, 0, 0, 0, 0, 0),
        Err("ebadf")
    );
    assert_eq!(
        kernel.dispatch_syscall_without_signal_delivery(SYS_DUP, MAX_FD, 0, 0, 0, 0, 0),
        Err("ebadf")
    );

    let mut dup_fillers = Vec::new();
    loop {
        match task.add_file(FLike::Tty(TtyDevice)) {
            Ok(filler) => dup_fillers.push(filler),
            Err("emfile") => break,
            Err(err) => panic!("unexpected dup fd fill error: {err}"),
        }
    }
    assert_eq!(
        kernel.dispatch_syscall_without_signal_delivery(SYS_DUP, fd, 0, 0, 0, 0, 0),
        Err("emfile")
    );
    for filler in dup_fillers {
        task.close_fd(filler).expect("dup filler fd should close");
    }
    task.close_fd(fd).expect("read fixture fd should close");
    task.close_fd(dup_fd)
        .expect("dup read fixture fd should close");

    let write_only_fd = kernel
        .dispatch_syscall_without_signal_delivery(SYS_OPENAT, 0, path_addr, 1, 0, 0, 0)
        .expect("write-only read fixture should open");
    assert_eq!(
        kernel.dispatch_syscall_without_signal_delivery(
            SYS_READ,
            write_only_fd,
            buf_addr,
            1,
            0,
            0,
            0,
        ),
        Err("ebadf")
    );
    assert_eq!(
        kernel.dispatch_syscall_without_signal_delivery(
            SYS_READ,
            write_only_fd + MAX_FD,
            buf_addr,
            1,
            0,
            0,
            0,
        ),
        Err("ebadf")
    );
    assert_eq!(
        kernel.dispatch_syscall_without_signal_delivery(
            SYS_READ,
            write_only_fd,
            unmapped_addr,
            1,
            0,
            0,
            0,
        ),
        Err("efault")
    );
    assert_eq!(
        kernel.dispatch_syscall_without_signal_delivery(
            SYS_READ,
            write_only_fd,
            readonly_addr,
            1,
            0,
            0,
            0,
        ),
        Err("efault")
    );
    assert_eq!(
        kernel.dispatch_syscall_without_signal_delivery(
            SYS_READ,
            write_only_fd + MAX_FD,
            0,
            0,
            0,
            0,
            0,
        ),
        Ok(0)
    );
    task.close_fd(write_only_fd)
        .expect("write-only read fixture fd should close");

    let partial_fd = kernel
        .dispatch_syscall_without_signal_delivery(SYS_OPENAT, 0, path_addr, 0, 0, 0, 0)
        .expect("short-read fixture should open");
    assert_eq!(
        kernel.dispatch_syscall_without_signal_delivery(
            SYS_READ,
            partial_fd,
            partial_addr,
            4,
            0,
            0,
            0,
        ),
        Ok(2)
    );
    let mut partial = [0u8; 2];
    task.process
        .addr_space
        .lock()
        .unwrap()
        .read_user_bytes(partial_addr, &mut partial)
        .expect("short read prefix should be copied to userspace");
    assert_eq!(&partial, b"ab");
    assert_eq!(
        kernel
            .dispatch_syscall_without_signal_delivery(SYS_READ, partial_fd, buf_addr, 2, 0, 0, 0,),
        Ok(2)
    );
    task.process
        .addr_space
        .lock()
        .unwrap()
        .read_user_bytes(buf_addr, &mut partial)
        .expect("read after short prefix should continue at the shared offset");
    assert_eq!(&partial, b"cd");
    task.close_fd(partial_fd)
        .expect("short-read fixture fd should close");

    let cow_fd = kernel
        .dispatch_syscall_without_signal_delivery(SYS_OPENAT, 0, path_addr, 0, 0, 0, 0)
        .expect("COW read fixture should open");
    task.process
        .addr_space
        .lock()
        .unwrap()
        .write_user_bytes(buf_addr, &[0u8; 3], &kernel.pool)
        .expect("COW destination should start cleared");
    let child_addr_space = {
        let mut parent = task.process.addr_space.lock().unwrap();
        AddrSpace::fork_from(&mut parent, &kernel.pool)
            .expect("read destination should become a COW mapping")
    };
    assert_eq!(
        kernel.dispatch_syscall_without_signal_delivery(SYS_READ, cow_fd, buf_addr, 3, 0, 0, 0),
        Ok(3)
    );
    task.process
        .addr_space
        .lock()
        .unwrap()
        .read_user_bytes(buf_addr, &mut bytes)
        .expect("sys_read should resolve the parent COW destination");
    assert_eq!(&bytes, b"abc");
    let mut child_bytes = [1u8; 3];
    child_addr_space
        .read_user_bytes(buf_addr, &mut child_bytes)
        .expect("child should retain the pre-read COW bytes");
    assert_eq!(child_bytes, [0u8; 3]);
    task.close_fd(cow_fd)
        .expect("COW read fixture fd should close");
}

// AGENT: verify that sys_read consumes queued pipe bytes, reports EAGAIN while
// an empty pipe still has a writer, and returns EOF after the last writer closes.
#[cfg_attr(test, test)]
fn read_moves_pipe_bytes_and_reports_empty_states(kernel: &Kernel) {
    let task = kernel.cur_task(0).expect("init task should be current");
    let buf_addr = READ_USER_BASE + 768;
    let (read_end, write_end) = PipeNode::pair();
    let (read_fd, write_fd) = task
        .add_file_pair_with_cloexec(FLike::Pipe(read_end), FLike::Pipe(write_end), false)
        .expect("pipe read fixture should allocate two descriptors");
    assert_eq!(
        task.get_fd_entry(write_fd)
            .expect("pipe write descriptor should exist")
            .write(task.id(), b"pipe"),
        Ok(FdWriteOutcome::Written(4))
    );
    assert_eq!(
        kernel.dispatch_syscall_without_signal_delivery(SYS_READ, read_fd, buf_addr, 8, 0, 0, 0),
        Ok(4)
    );
    let mut bytes = [0u8; 4];
    task.process
        .addr_space
        .lock()
        .unwrap()
        .read_user_bytes(buf_addr, &mut bytes)
        .expect("pipe read bytes should be copied to userspace");
    assert_eq!(&bytes, b"pipe");
    task.get_fd_entry(read_fd)
        .expect("pipe read descriptor should remain installed")
        .set_status_flags(O_NONBLOCK)
        .expect("pipe read descriptor should become nonblocking");
    assert_eq!(
        kernel.dispatch_syscall_without_signal_delivery(SYS_READ, read_fd, buf_addr, 1, 0, 0, 0),
        Err("eagain")
    );
    task.close_fd(write_fd)
        .expect("closing the last pipe writer should succeed");
    assert_eq!(
        kernel.dispatch_syscall_without_signal_delivery(SYS_READ, read_fd, buf_addr, 1, 0, 0, 0),
        Ok(0)
    );
    task.close_fd(read_fd)
        .expect("pipe read descriptor should close");
}

// AGENT: sys_write owns the ABI-visible EPIPE plus SIGPIPE pairing; PipeNode
// only reports a broken peer and must not reach into process signal state.
#[cfg_attr(test, test)]
fn write_to_pipe_without_readers_returns_epipe_and_queues_sigpipe(kernel: &Kernel) {
    let task = kernel.cur_task(0).expect("init task should be current");
    let buf_addr = READ_USER_BASE + 896;
    task.process
        .addr_space
        .lock()
        .unwrap()
        .write_user_bytes(buf_addr, b"x", &kernel.pool)
        .expect("pipe write source should be writable");
    task.process
        .sig_queue
        .lock()
        .unwrap()
        .retain(|(signo, _)| *signo != SIGPIPE as i32);

    let (read_end, write_end) = PipeNode::pair();
    let (read_fd, write_fd) = task
        .add_file_pair_with_cloexec(FLike::Pipe(read_end), FLike::Pipe(write_end), false)
        .expect("broken pipe fixture should allocate two descriptors");
    task.close_fd(read_fd)
        .expect("broken pipe fixture should close its last reader");

    assert_eq!(
        kernel.dispatch_syscall_without_signal_delivery(SYS_WRITE, write_fd, buf_addr, 1, 0, 0, 0,),
        Err("epipe")
    );
    assert!(
        task.process
            .sig_queue
            .lock()
            .unwrap()
            .iter()
            .any(|(signo, _)| *signo == SIGPIPE as i32),
        "broken pipe write should queue SIGPIPE"
    );

    task.process
        .sig_queue
        .lock()
        .unwrap()
        .retain(|(signo, _)| *signo != SIGPIPE as i32);
    task.close_fd(write_fd)
        .expect("broken pipe write descriptor should close");
}
