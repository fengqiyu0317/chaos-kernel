// AGENT: QEMU filesystem syscall regressions that require the installed frame
// pool, current init task, Sv39 mappings, and authoritative AddrSpace usercopy.
use super::*;
use crate::syscall_abi::{
    decode_from_trap_frame, map_riscv_nr, INTERNAL_SYS_DUP, INTERNAL_SYS_DUP3, INTERNAL_SYS_FCNTL,
    INTERNAL_SYS_FSTAT, INTERNAL_SYS_IOCTL, INTERNAL_SYS_MKDIRAT, INTERNAL_SYS_MOUNT,
    INTERNAL_SYS_NEWFSTATAT, INTERNAL_SYS_OPENAT, INTERNAL_SYS_PIPE, INTERNAL_SYS_READ,
    INTERNAL_SYS_SPLICE, INTERNAL_SYS_UMOUNT2, RISCV_SYS_DUP, RISCV_SYS_DUP3, RISCV_SYS_FCNTL,
    RISCV_SYS_FSTAT, RISCV_SYS_IOCTL, RISCV_SYS_MKDIRAT, RISCV_SYS_MOUNT, RISCV_SYS_NEWFSTATAT,
    RISCV_SYS_OPENAT, RISCV_SYS_PIPE2, RISCV_SYS_READ, RISCV_SYS_SPLICE, RISCV_SYS_UMOUNT2,
};

const USER_STRINGS_BASE: usize = 0x4000_0000;
const OPEN_USER_BASE: usize = 0x4001_0000;
const MKDIR_USER_BASE: usize = 0x4002_0000;
const READ_USER_BASE: usize = 0x4003_0000;
const UNMOUNT_USER_BASE: usize = 0x4004_0000;
const STAT_USER_BASE: usize = 0x4005_0000;
const PIPE_USER_BASE: usize = 0x4006_0000;
const SPLICE_USER_BASE: usize = 0x4007_0000;
const IOCTL_USER_BASE: usize = 0x4008_0000;
const FCNTL_USER_BASE: usize = 0x4009_0000;

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
    ioctl_uses_usercopy_and_correct_fd_ownership(kernel);
    dup3_uses_the_shared_exact_target_implementation(kernel);
    read_uses_usercopy_and_shared_open_file_offsets(kernel);
    read_moves_pipe_bytes_and_reports_empty_states(kernel);
    write_to_pipe_without_readers_returns_epipe_and_queues_sigpipe(kernel);
    splice_moves_between_files_and_pipes_with_linux_offsets(kernel);
    crate::kernel::fs::record_lock::tests::run_all();
    fcntl_implements_fd_ofd_record_lock_and_lifecycle_semantics(kernel);
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
fn read_user_pipe_fds(kernel: &Kernel, task: &Task, addr: usize) -> (usize, usize) {
    let fd_size = mem::size_of::<i32>();
    let mut bytes = [0u8; 2 * mem::size_of::<i32>()];
    task.process
        .addr_space
        .lock()
        .unwrap()
        .read_user_bytes(addr, &mut bytes, &kernel.pool)
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

// AGENT: seed one signed RV64 off_t through the same writable user mapping used
// by sys_splice copy-in and copy-out.
fn write_user_off(kernel: &Kernel, task: &Task, addr: usize, value: i64) {
    task.process
        .addr_space
        .lock()
        .unwrap()
        .write_user_bytes(addr, &value.to_ne_bytes(), &kernel.pool)
        .expect("splice offset should be writable in userspace");
}

// AGENT: observe one sys_splice off_t copy-out without directly dereferencing
// the emulated user virtual address.
fn read_user_off(kernel: &Kernel, task: &Task, addr: usize) -> i64 {
    let mut bytes = [0u8; mem::size_of::<i64>()];
    task.process
        .addr_space
        .lock()
        .unwrap()
        .read_user_bytes(addr, &mut bytes, &kernel.pool)
        .expect("splice offset should be readable in userspace");
    i64::from_ne_bytes(bytes)
}

// AGENT: seed one ioctl int through the active address space instead of using
// a raw pointer into the QEMU user's virtual range.
fn write_user_ioctl_int(kernel: &Kernel, task: &Task, addr: usize, value: i32) {
    task.process
        .addr_space
        .lock()
        .unwrap()
        .write_user_bytes(addr, &value.to_ne_bytes(), &kernel.pool)
        .expect("ioctl integer argument should be writable");
}

// AGENT: observe one ioctl int result through authoritative Sv39 usercopy.
fn read_user_ioctl_int(kernel: &Kernel, task: &Task, addr: usize) -> i32 {
    let mut bytes = [0u8; mem::size_of::<i32>()];
    task.process
        .addr_space
        .lock()
        .unwrap()
        .read_user_bytes(addr, &mut bytes, &kernel.pool)
        .expect("ioctl integer result should be readable");
    i32::from_ne_bytes(bytes)
}

// AGENT: encode one complete RV64 flock fixture with explicit ABI offsets and
// zero padding, then publish it through the live user address space.
fn write_user_flock_fixture(kernel: &Kernel, task: &Task, addr: usize, flock: FlockArg) {
    let mut bytes = [0u8; RISCV64_FLOCK_SIZE];
    bytes[0..2].copy_from_slice(&flock.lock_type.to_le_bytes());
    bytes[2..4].copy_from_slice(&flock.whence.to_le_bytes());
    bytes[8..16].copy_from_slice(&flock.start.to_le_bytes());
    bytes[16..24].copy_from_slice(&flock.len.to_le_bytes());
    bytes[24..28].copy_from_slice(&flock.pid.to_le_bytes());
    task.process
        .addr_space
        .lock()
        .unwrap()
        .write_user_bytes(addr, &bytes, &kernel.pool)
        .expect("fcntl flock fixture should be writable");
}

// AGENT: decode a returned RV64 flock through AddrSpace rather than directly
// dereferencing its user virtual address in the kernel selftest.
fn read_user_flock_fixture(kernel: &Kernel, task: &Task, addr: usize) -> FlockArg {
    let mut bytes = [0u8; RISCV64_FLOCK_SIZE];
    task.process
        .addr_space
        .lock()
        .unwrap()
        .read_user_bytes(addr, &mut bytes, &kernel.pool)
        .expect("fcntl flock result should be readable");
    FlockArg {
        lock_type: i16::from_le_bytes(bytes[0..2].try_into().unwrap()),
        whence: i16::from_le_bytes(bytes[2..4].try_into().unwrap()),
        start: i64::from_le_bytes(bytes[8..16].try_into().unwrap()),
        len: i64::from_le_bytes(bytes[16..24].try_into().unwrap()),
        pid: i32::from_le_bytes(bytes[24..28].try_into().unwrap()),
    }
}

// AGENT: construct one concise flock request for syscall and lifecycle tests.
fn flock_fixture(lock_type: i16, whence: i16, start: i64, len: i64) -> FlockArg {
    FlockArg {
        lock_type,
        whence,
        start,
        len,
        pid: 0,
    }
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
fn read_user_stat(kernel: &Kernel, task: &Task, addr: usize) -> [u8; RISCV64_STAT_SIZE] {
    let mut bytes = [0u8; RISCV64_STAT_SIZE];
    task.process
        .addr_space
        .lock()
        .unwrap()
        .read_user_bytes(addr, &mut bytes, &kernel.pool)
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
    let path_stat = read_user_stat(kernel, &task, stat_addr);
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
    assert_eq!(read_user_stat(kernel, &task, stat_addr), path_stat);

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
    let directory_stat = read_user_stat(kernel, &task, stat_addr);
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
        .read_user_bytes(partial_addr, &mut partial, &kernel.pool)
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
    let first_pair = read_user_pipe_fds(kernel, &task, PIPE_USER_BASE);
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
    let reused_pair = read_user_pipe_fds(kernel, &task, PIPE_USER_BASE);
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

// AGENT: prove ioctl is RV64-reachable, uses Sv39-backed integer usercopy,
// shares FIONBIO through the OFD, and keeps close-on-exec descriptor-local.
#[cfg_attr(test, test)]
fn ioctl_uses_usercopy_and_correct_fd_ownership(kernel: &Kernel) {
    assert_eq!(map_riscv_nr(RISCV_SYS_IOCTL), Some(INTERNAL_SYS_IOCTL));

    let task = kernel.cur_task(0).expect("init task should be current");
    {
        let mut addr_space = task.process.addr_space.lock().unwrap();
        addr_space
            .map_region(
                VmRegion::new(IOCTL_USER_BASE, PAGE_SZ, VM_READ | VM_WRITE),
                &kernel.pool,
            )
            .expect("ioctl read-write user page should map");
        addr_space
            .map_region(
                VmRegion::new(IOCTL_USER_BASE + PAGE_SZ, PAGE_SZ, VM_READ),
                &kernel.pool,
            )
            .expect("ioctl read-only user page should map");
    }
    let flag_addr = IOCTL_USER_BASE;
    let result_addr = IOCTL_USER_BASE + mem::size_of::<i32>();

    let (read_end, write_end) = PipeNode::pair();
    let (read_fd, write_fd) = task
        .add_file_pair_with_cloexec(FLike::Pipe(read_end), FLike::Pipe(write_end), false)
        .expect("ioctl pipe fixture should allocate descriptors");
    assert_eq!(
        task.get_fd_entry(write_fd)
            .expect("ioctl pipe writer should exist")
            .write(task.id(), b"abc"),
        Ok(FdWriteOutcome::Written(3))
    );

    assert_eq!(
        kernel.dispatch_syscall_without_signal_delivery(
            SYS_IOCTL,
            read_fd,
            FIONREAD,
            result_addr,
            0,
            0,
            0,
        ),
        Ok(0)
    );
    assert_eq!(read_user_ioctl_int(kernel, &task, result_addr), 3);
    for bad_output in [IOCTL_USER_BASE + PAGE_SZ, IOCTL_USER_BASE + 2 * PAGE_SZ] {
        assert_eq!(
            kernel.dispatch_syscall_without_signal_delivery(
                SYS_IOCTL, read_fd, FIONREAD, bad_output, 0, 0, 0,
            ),
            Err("efault")
        );
    }

    let dup_fd = task
        .dup_fd(read_fd, false)
        .expect("ioctl OFD-sharing probe should duplicate the read descriptor");
    write_user_ioctl_int(kernel, &task, flag_addr, 1);
    assert_eq!(
        kernel.dispatch_syscall_without_signal_delivery(
            SYS_IOCTL, read_fd, FIONBIO, flag_addr, 0, 0, 0,
        ),
        Ok(0)
    );
    assert!(
        task.get_fd_entry(dup_fd)
            .expect("duplicated ioctl descriptor should remain installed")
            .status_flags()
            .nb
    );
    assert!(
        !task
            .get_fd_entry(write_fd)
            .expect("separate pipe write OFD should remain installed")
            .status_flags()
            .nb
    );
    write_user_ioctl_int(kernel, &task, flag_addr, 0);
    assert_eq!(
        kernel.dispatch_syscall_without_signal_delivery(
            SYS_IOCTL, dup_fd, FIONBIO, flag_addr, 0, 0, 0,
        ),
        Ok(0)
    );
    assert!(
        !task
            .get_fd_entry(read_fd)
            .expect("original ioctl descriptor should remain installed")
            .status_flags()
            .nb
    );
    assert_eq!(
        kernel.dispatch_syscall_without_signal_delivery(
            SYS_IOCTL,
            read_fd,
            FIONBIO,
            IOCTL_USER_BASE + 2 * PAGE_SZ,
            0,
            0,
            0,
        ),
        Err("efault")
    );

    assert_eq!(
        kernel.dispatch_syscall_without_signal_delivery(SYS_IOCTL, read_fd, FIOCLEX, 0, 0, 0, 0,),
        Ok(0)
    );
    assert!(task
        .get_fd_entry(read_fd)
        .expect("FIOCLEX target should remain installed")
        .is_cloexec());
    assert!(!task
        .get_fd_entry(dup_fd)
        .expect("FIOCLEX duplicate should remain installed")
        .is_cloexec());
    assert_eq!(
        kernel.dispatch_syscall_without_signal_delivery(SYS_IOCTL, read_fd, FIONCLEX, 0, 0, 0, 0,),
        Ok(0)
    );
    assert!(!task
        .get_fd_entry(read_fd)
        .expect("FIONCLEX target should remain installed")
        .is_cloexec());

    assert_eq!(
        kernel.dispatch_syscall_without_signal_delivery(SYS_IOCTL, 0, TCGETS, 0, 0, 0, 0),
        Err("enotty")
    );
    assert_eq!(
        kernel.dispatch_syscall_without_signal_delivery(
            SYS_IOCTL,
            read_fd,
            0xDEAD,
            result_addr,
            0,
            0,
            0,
        ),
        Err("enotty")
    );
    assert_eq!(
        kernel.dispatch_syscall_without_signal_delivery(
            SYS_IOCTL,
            MAX_FD,
            FIONREAD,
            IOCTL_USER_BASE + 2 * PAGE_SZ,
            0,
            0,
            0,
        ),
        Err("ebadf")
    );

    task.close_fd(read_fd)
        .expect("ioctl read descriptor should close");
    task.close_fd(write_fd)
        .expect("ioctl write descriptor should close");
    task.close_fd(dup_fd)
        .expect("ioctl duplicated descriptor should close");
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

// AGENT: exercise all nine declared fcntl commands through the internal syscall
// dispatcher, fixed RV64 flock usercopy, process lock table, and close lifecycle.
#[cfg_attr(test, test)]
fn fcntl_implements_fd_ofd_record_lock_and_lifecycle_semantics(kernel: &Kernel) {
    assert_eq!(map_riscv_nr(RISCV_SYS_FCNTL), Some(INTERNAL_SYS_FCNTL));
    assert_eq!(INTERNAL_SYS_FCNTL, SYS_FCNTL);
    let mut frame = TrapFrame::new();
    frame.regs[10..16].copy_from_slice(&[17, F_SETLK, FCNTL_USER_BASE, 4, 5, 6]);
    frame.regs[17] = RISCV_SYS_FCNTL;
    let decoded = decode_from_trap_frame(&frame);
    assert_eq!(decoded.internal_nr, Some(SYS_FCNTL));
    assert_eq!(decoded.args, [17, F_SETLK, FCNTL_USER_BASE, 4, 5, 6]);

    let task = kernel.cur_task(0).expect("init task should be current");
    {
        let mut addr_space = task.process.addr_space.lock().unwrap();
        addr_space
            .map_region(
                VmRegion::new(FCNTL_USER_BASE, PAGE_SZ, VM_READ | VM_WRITE),
                &kernel.pool,
            )
            .expect("fcntl read-write user page should map");
        addr_space
            .map_region(
                VmRegion::new(FCNTL_USER_BASE + 2 * PAGE_SZ, PAGE_SZ, VM_READ | VM_WRITE),
                &kernel.pool,
            )
            .expect("fcntl protection-transition page should map");
    }

    kernel
        .install_file("/tmp/qemu-fcntl-a", vec![0u8; 128], false)
        .expect("fcntl primary fixture should install");
    kernel
        .install_file("/tmp/qemu-fcntl-b", vec![0u8; 64], false)
        .expect("fcntl replacement fixture should install");
    let fd = do_open(kernel, &task, "/tmp/qemu-fcntl-a", 2, 0)
        .expect("fcntl primary fixture should open read-write");
    let flock_addr = FCNTL_USER_BASE + 256;

    assert_eq!(
        kernel.dispatch_syscall_without_signal_delivery(SYS_FCNTL, fd, F_GETFD, 0, 0, 0, 0),
        Ok(0)
    );
    assert_eq!(
        kernel
            .dispatch_syscall_without_signal_delivery(SYS_FCNTL, fd, F_SETFD, FD_CLOEXEC, 0, 0, 0,),
        Ok(0)
    );
    assert_eq!(
        kernel.dispatch_syscall_without_signal_delivery(SYS_FCNTL, fd, F_GETFD, 0, 0, 0, 0),
        Ok(FD_CLOEXEC)
    );

    let dup_fd = kernel
        .dispatch_syscall_without_signal_delivery(SYS_FCNTL, fd, F_DUPFD, 40, 0, 0, 0)
        .expect("F_DUPFD should allocate from its lower bound");
    assert!(dup_fd >= 40);
    assert!(!task.get_fd_entry(dup_fd).unwrap().is_cloexec());
    let cloexec_dup_fd = kernel
        .dispatch_syscall_without_signal_delivery(
            SYS_FCNTL,
            fd,
            F_DUPFD_CLOEXEC,
            dup_fd + 1,
            0,
            0,
            0,
        )
        .expect("F_DUPFD_CLOEXEC should allocate from its lower bound");
    assert!(cloexec_dup_fd > dup_fd);
    assert!(task.get_fd_entry(cloexec_dup_fd).unwrap().is_cloexec());
    assert!(task
        .get_fd_entry(fd)
        .unwrap()
        .same_open_description(&task.get_fd_entry(dup_fd).unwrap()));

    assert_eq!(
        kernel.dispatch_syscall_without_signal_delivery(SYS_FCNTL, fd, F_GETFL, 0, 0, 0, 0),
        Ok(2)
    );
    assert_eq!(
        kernel.dispatch_syscall_without_signal_delivery(
            SYS_FCNTL,
            dup_fd,
            F_SETFL,
            O_APPEND | O_NONBLOCK,
            0,
            0,
            0,
        ),
        Ok(0)
    );
    assert_eq!(
        kernel.dispatch_syscall_without_signal_delivery(SYS_FCNTL, fd, F_GETFL, 0, 0, 0, 0),
        Ok(2 | O_APPEND | O_NONBLOCK)
    );

    let missing_fd = task
        .add_file(FLike::Tty(TtyDevice))
        .expect("fcntl missing-fd probe should allocate");
    kernel
        .close_task_fd(&task, missing_fd)
        .expect("fcntl missing-fd probe should close");
    assert_eq!(
        kernel.dispatch_syscall_without_signal_delivery(
            SYS_FCNTL, missing_fd, F_DUPFD, MAX_FD, 0, 0, 0,
        ),
        Err("ebadf")
    );
    assert_eq!(
        kernel.dispatch_syscall_without_signal_delivery(
            SYS_FCNTL,
            missing_fd,
            F_SETFL,
            usize::MAX,
            0,
            0,
            0,
        ),
        Err("ebadf")
    );

    let write_lock = flock_fixture(F_WRLCK, SEEK_SET, 10, 20);
    write_user_flock_fixture(kernel, &task, flock_addr, write_lock);
    assert_eq!(
        kernel
            .dispatch_syscall_without_signal_delivery(SYS_FCNTL, fd, F_SETLK, flock_addr, 0, 0, 0,),
        Ok(0)
    );
    assert!(kernel.record_locks.process_has_locks(task.process.pid()));

    write_user_flock_fixture(kernel, &task, flock_addr, write_lock);
    assert_eq!(
        kernel.dispatch_syscall_without_signal_delivery(
            SYS_FCNTL, dup_fd, F_GETLK, flock_addr, 0, 0, 0,
        ),
        Ok(0)
    );
    assert_eq!(
        read_user_flock_fixture(kernel, &task, flock_addr).lock_type,
        F_UNLCK
    );
    assert_eq!(
        kernel.dispatch_syscall_without_signal_delivery(
            SYS_FCNTL,
            fd,
            F_SETLKW,
            flock_addr,
            0,
            0,
            0,
        ),
        Ok(0)
    );

    let unlock = flock_fixture(F_UNLCK, SEEK_SET, 10, 20);
    write_user_flock_fixture(kernel, &task, flock_addr, unlock);
    assert_eq!(
        kernel
            .dispatch_syscall_without_signal_delivery(SYS_FCNTL, fd, F_SETLK, flock_addr, 0, 0, 0,),
        Ok(0)
    );
    assert!(!kernel.record_locks.process_has_locks(task.process.pid()));

    let entry = task
        .get_fd_entry(fd)
        .expect("fcntl primary fd should remain");
    let request = entry
        .record_lock_request(write_lock, true)
        .expect("fcntl direct conflict request should normalize");
    let other_pid = task.process.pid() + 100;
    kernel
        .record_locks
        .set_nonblocking(other_pid, request)
        .expect("other process lock should install");
    write_user_flock_fixture(kernel, &task, flock_addr, write_lock);
    assert_eq!(
        kernel
            .dispatch_syscall_without_signal_delivery(SYS_FCNTL, fd, F_GETLK, flock_addr, 0, 0, 0,),
        Ok(0)
    );
    let conflict = read_user_flock_fixture(kernel, &task, flock_addr);
    assert_eq!(conflict.lock_type, F_WRLCK);
    assert_eq!(conflict.whence, SEEK_SET);
    assert_eq!(conflict.start, 10);
    assert_eq!(conflict.len, 20);
    assert_eq!(conflict.pid, other_pid as i32);
    write_user_flock_fixture(kernel, &task, flock_addr, write_lock);
    assert_eq!(
        kernel
            .dispatch_syscall_without_signal_delivery(SYS_FCNTL, fd, F_SETLK, flock_addr, 0, 0, 0,),
        Err("eagain")
    );
    kernel.record_locks.release_process(other_pid);

    assert_eq!(
        kernel.dispatch_syscall_without_signal_delivery(SYS_FCNTL, fd, F_SETLK, 0, 0, 0, 0),
        Err("efault")
    );
    assert_eq!(
        kernel.dispatch_syscall_without_signal_delivery(
            SYS_FCNTL,
            fd,
            F_SETLK,
            FCNTL_USER_BASE + 4 * PAGE_SZ,
            0,
            0,
            0,
        ),
        Err("efault")
    );
    let partial_addr = FCNTL_USER_BASE + PAGE_SZ - 16;
    task.process
        .addr_space
        .lock()
        .unwrap()
        .write_user_bytes(partial_addr, &[0u8; 16], &kernel.pool)
        .expect("partial flock prefix should be writable");
    assert_eq!(
        kernel.dispatch_syscall_without_signal_delivery(
            SYS_FCNTL,
            fd,
            F_SETLK,
            partial_addr,
            0,
            0,
            0,
        ),
        Err("efault")
    );

    let readonly_addr = FCNTL_USER_BASE + 2 * PAGE_SZ;
    write_user_flock_fixture(kernel, &task, readonly_addr, write_lock);
    task.process
        .addr_space
        .lock()
        .unwrap()
        .protect(readonly_addr, PAGE_SZ, VM_READ)
        .expect("fcntl result page should become read-only");
    assert_eq!(
        kernel.dispatch_syscall_without_signal_delivery(
            SYS_FCNTL,
            fd,
            F_GETLK,
            readonly_addr,
            0,
            0,
            0,
        ),
        Err("efault")
    );

    write_user_flock_fixture(kernel, &task, flock_addr, write_lock);
    assert_eq!(
        kernel
            .dispatch_syscall_without_signal_delivery(SYS_FCNTL, fd, F_SETLK, flock_addr, 0, 0, 0,),
        Ok(0)
    );
    let close_alias = task
        .dup_fd(fd, false)
        .expect("close lifecycle alias should duplicate");
    assert_eq!(
        kernel.dispatch_syscall_without_signal_delivery(SYS_CLOSE, close_alias, 0, 0, 0, 0, 0,),
        Ok(0)
    );
    assert!(!kernel.record_locks.process_has_locks(task.process.pid()));

    write_user_flock_fixture(kernel, &task, flock_addr, write_lock);
    assert_eq!(
        kernel
            .dispatch_syscall_without_signal_delivery(SYS_FCNTL, fd, F_SETLK, flock_addr, 0, 0, 0,),
        Ok(0)
    );
    let replacement_source = do_open(kernel, &task, "/tmp/qemu-fcntl-b", 2, 0)
        .expect("fcntl replacement source should open");
    assert_eq!(
        kernel.dispatch_syscall_without_signal_delivery(
            SYS_DUP3,
            replacement_source,
            fd,
            0,
            0,
            0,
            0,
        ),
        Ok(fd)
    );
    assert!(!kernel.record_locks.process_has_locks(task.process.pid()));

    let replacement_entry = task
        .get_fd_entry(fd)
        .expect("dup3 replacement should remain installed");
    let replacement_request = replacement_entry
        .record_lock_request(write_lock, true)
        .expect("replacement record-lock request should normalize");
    kernel
        .record_locks
        .set_nonblocking(task.process.pid(), replacement_request)
        .expect("checkpoint rejection lock should install");
    let saved_frame = task
        .snapshot_user_trap_frame()
        .expect("fcntl checkpoint fixture should retain a frame")
        .to_saved_checkpoint_frame();
    assert_eq!(
        kernel.checkpoint_current_image(0, saved_frame),
        Err("enotsup")
    );
    kernel.record_locks.release_process(task.process.pid());

    for cleanup_fd in [dup_fd, cloexec_dup_fd, replacement_source, fd] {
        kernel
            .close_task_fd(&task, cleanup_fd)
            .expect("fcntl fixture descriptor should close");
    }

    // AGENT: a forked process shares fd/OFD state but starts with no parent-owned
    // POSIX record locks because the global table is keyed by process PID.
    let fork_kernel = Kernel::new(kernel.pool.clone());
    fork_kernel.proc_init();
    fork_kernel
        .install_directory("/tmp")
        .expect("fork lock fixture requires /tmp");
    fork_kernel
        .install_file("/tmp/fork-lock", vec![0u8; 8], false)
        .expect("fork lock fixture should install");
    let fork_parent = fork_kernel
        .cur_task(0)
        .expect("fork lock parent should exist");
    let fork_fd = do_open(&fork_kernel, &fork_parent, "/tmp/fork-lock", 2, 0)
        .expect("fork lock fixture should open");
    let fork_request = fork_parent
        .get_fd_entry(fork_fd)
        .unwrap()
        .record_lock_request(write_lock, true)
        .unwrap();
    fork_kernel
        .record_locks
        .set_nonblocking(fork_parent.process.pid(), fork_request)
        .unwrap();
    let fork_child = fork_kernel
        .tasks
        .fork_process(&fork_parent)
        .expect("record-lock fork child should be created");
    assert!(fork_kernel
        .record_locks
        .process_has_locks(fork_parent.process.pid()));
    assert!(!fork_kernel
        .record_locks
        .process_has_locks(fork_child.process.pid()));
    fork_kernel
        .record_locks
        .release_process(fork_parent.process.pid());

    // AGENT: exec preserves locks when no descriptor for the file closes, but
    // its captured FD_CLOEXEC close list releases all locks for that file.
    let exec_kernel = Kernel::new(kernel.pool.clone());
    exec_kernel.proc_init();
    exec_kernel
        .install_directory("/tmp")
        .expect("exec lock fixture requires /tmp");
    exec_kernel
        .install_file("/tmp/exec-lock", vec![0u8; 8], false)
        .expect("exec lock fixture should install");
    let exec_task = exec_kernel
        .cur_task(0)
        .expect("exec lock task should exist");
    let exec_fd = do_open(&exec_kernel, &exec_task, "/tmp/exec-lock", 2, 0)
        .expect("exec lock fixture should open");
    let exec_alias = exec_task
        .dup_fd(exec_fd, false)
        .expect("exec lock alias should duplicate");
    exec_task
        .set_cloexec(exec_fd, true)
        .expect("exec lock source should become close-on-exec");
    let exec_request = exec_task
        .get_fd_entry(exec_fd)
        .unwrap()
        .record_lock_request(write_lock, true)
        .unwrap();
    exec_kernel
        .record_locks
        .set_nonblocking(exec_task.process.pid(), exec_request)
        .unwrap();
    let close_fds = exec_task.cloexec_fds();
    exec_kernel.close_cloexec_task_fds(&exec_task, &close_fds);
    assert!(exec_task.get_fd_entry(exec_fd).is_none());
    assert!(exec_task.get_fd_entry(exec_alias).is_some());
    assert!(!exec_kernel
        .record_locks
        .process_has_locks(exec_task.process.pid()));

    let exec_alias_request = exec_task
        .get_fd_entry(exec_alias)
        .unwrap()
        .record_lock_request(write_lock, true)
        .unwrap();
    exec_kernel
        .record_locks
        .set_nonblocking(exec_task.process.pid(), exec_alias_request)
        .unwrap();
    exec_kernel.close_cloexec_task_fds(&exec_task, &[]);
    assert!(exec_kernel
        .record_locks
        .process_has_locks(exec_task.process.pid()));
    exec_kernel
        .record_locks
        .release_process(exec_task.process.pid());
    exec_kernel
        .close_task_fd(&exec_task, exec_alias)
        .expect("exec lock alias should close");
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
        .read_user_bytes(buf_addr, &mut bytes, &kernel.pool)
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
        .read_user_bytes(buf_addr, &mut bytes, &kernel.pool)
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
        .read_user_bytes(partial_addr, &mut partial, &kernel.pool)
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
        .read_user_bytes(buf_addr, &mut partial, &kernel.pool)
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
    let mut child_addr_space = {
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
        .read_user_bytes(buf_addr, &mut bytes, &kernel.pool)
        .expect("sys_read should resolve the parent COW destination");
    assert_eq!(&bytes, b"abc");
    let mut child_bytes = [1u8; 3];
    child_addr_space
        .read_user_bytes(buf_addr, &mut child_bytes, &kernel.pool)
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
        .read_user_bytes(buf_addr, &mut bytes, &kernel.pool)
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

// AGENT: exercise the real splice syscall across file-to-pipe, pipe-to-file,
// and pipe-to-pipe paths, including RV64 off_t usercopy and failure atomicity.
#[cfg_attr(test, test)]
fn splice_moves_between_files_and_pipes_with_linux_offsets(kernel: &Kernel) {
    assert_eq!(map_riscv_nr(RISCV_SYS_SPLICE), Some(INTERNAL_SYS_SPLICE));
    let task = kernel.cur_task(0).expect("init task should be current");
    {
        let mut addr_space = task.process.addr_space.lock().unwrap();
        addr_space
            .map_region(
                VmRegion::new(SPLICE_USER_BASE, PAGE_SZ, VM_READ | VM_WRITE),
                &kernel.pool,
            )
            .expect("splice read-write user page should map");
        addr_space
            .map_region(
                VmRegion::new(SPLICE_USER_BASE + PAGE_SZ, PAGE_SZ, VM_READ),
                &kernel.pool,
            )
            .expect("splice read-only user page should map");
    }

    let src_path_addr = SPLICE_USER_BASE;
    let dst_path_addr = SPLICE_USER_BASE + 64;
    let input_offset_addr = SPLICE_USER_BASE + 256;
    let output_offset_addr = SPLICE_USER_BASE + 264;
    let readonly_offset_addr = SPLICE_USER_BASE + PAGE_SZ;
    let unmapped_offset_addr = SPLICE_USER_BASE + 2 * PAGE_SZ;
    write_user_string(kernel, &task, src_path_addr, "/tmp/qemu-splice-src");
    write_user_string(kernel, &task, dst_path_addr, "/tmp/qemu-splice-dst");
    kernel
        .install_file("/tmp/qemu-splice-src", b"abcdef".to_vec(), false)
        .expect("splice source fixture should install");
    kernel
        .install_file("/tmp/qemu-splice-dst", b".....".to_vec(), false)
        .expect("splice destination fixture should install");
    let src_fd = kernel
        .dispatch_syscall_without_signal_delivery(SYS_OPENAT, 0, src_path_addr, 0, 0, 0, 0)
        .expect("splice source should open read-only");
    let dst_fd = kernel
        .dispatch_syscall_without_signal_delivery(SYS_OPENAT, 0, dst_path_addr, 1, 0, 0, 0)
        .expect("splice destination should open write-only");

    assert_eq!(
        kernel.dispatch_syscall_without_signal_delivery(
            SYS_SPLICE,
            MAX_FD,
            unmapped_offset_addr,
            MAX_FD,
            unmapped_offset_addr,
            0,
            usize::MAX,
        ),
        Ok(0)
    );
    assert_eq!(
        kernel.dispatch_syscall_without_signal_delivery(
            SYS_SPLICE,
            MAX_FD,
            0,
            MAX_FD,
            0,
            1,
            SPLICE_KNOWN_FLAGS | 0x10,
        ),
        Err("einval")
    );
    assert_eq!(
        kernel.dispatch_syscall_without_signal_delivery(SYS_SPLICE, MAX_FD, 0, dst_fd, 0, 1, 0,),
        Err("ebadf")
    );
    assert_eq!(
        kernel.dispatch_syscall_without_signal_delivery(SYS_SPLICE, src_fd, 0, dst_fd, 0, 1, 0,),
        Err("einval")
    );

    let (first_read, first_write) = PipeNode::pair();
    let (first_read_fd, first_write_fd) = task
        .add_file_pair_with_cloexec(FLike::Pipe(first_read), FLike::Pipe(first_write), false)
        .expect("file-to-pipe splice fixture should allocate descriptors");
    assert_eq!(
        kernel.dispatch_syscall_without_signal_delivery(
            SYS_SPLICE,
            src_fd,
            0,
            first_write_fd,
            output_offset_addr,
            1,
            0,
        ),
        Err("espipe")
    );
    assert_eq!(
        kernel.dispatch_syscall_without_signal_delivery(
            SYS_SPLICE,
            src_fd,
            unmapped_offset_addr,
            first_write_fd,
            0,
            1,
            0,
        ),
        Err("efault")
    );
    write_user_off(kernel, &task, input_offset_addr, -1);
    assert_eq!(
        kernel.dispatch_syscall_without_signal_delivery(
            SYS_SPLICE,
            src_fd,
            input_offset_addr,
            first_write_fd,
            0,
            1,
            0,
        ),
        Err("einval")
    );

    write_user_off(kernel, &task, input_offset_addr, 1);
    assert_eq!(
        kernel.dispatch_syscall_without_signal_delivery(
            SYS_SPLICE,
            src_fd,
            input_offset_addr,
            first_write_fd,
            0,
            3,
            SPLICE_F_MOVE | SPLICE_F_MORE | SPLICE_F_GIFT,
        ),
        Ok(3)
    );
    assert_eq!(read_user_off(kernel, &task, input_offset_addr), 4);
    assert_eq!(
        task.get_fd_entry(src_fd)
            .expect("splice source descriptor should exist")
            .offset(),
        0
    );
    let mut moved = [0u8; 3];
    assert_eq!(
        task.get_fd_entry(first_read_fd)
            .expect("file-to-pipe read endpoint should exist")
            .read(task.id(), &mut moved),
        Ok(3)
    );
    assert_eq!(&moved, b"bcd");

    assert_eq!(
        kernel.dispatch_syscall_without_signal_delivery(
            SYS_SPLICE,
            src_fd,
            0,
            first_write_fd,
            0,
            2,
            0,
        ),
        Ok(2)
    );
    assert_eq!(
        task.get_fd_entry(src_fd)
            .expect("splice source descriptor should remain installed")
            .offset(),
        2
    );
    let mut shared_bytes = [0u8; 2];
    assert_eq!(
        task.get_fd_entry(first_read_fd)
            .expect("shared-offset pipe bytes should remain readable")
            .read(task.id(), &mut shared_bytes),
        Ok(2)
    );
    assert_eq!(&shared_bytes, b"ab");

    assert_eq!(
        kernel.dispatch_syscall_without_signal_delivery(
            SYS_SPLICE,
            src_fd,
            readonly_offset_addr,
            first_write_fd,
            0,
            1,
            0,
        ),
        Err("efault")
    );
    let mut late_fault_byte = [0u8; 1];
    assert_eq!(
        task.get_fd_entry(first_read_fd)
            .expect("late-fault splice byte should remain committed")
            .read(task.id(), &mut late_fault_byte),
        Ok(1)
    );
    assert_eq!(&late_fault_byte, b"a");
    assert_eq!(
        task.get_fd_entry(src_fd)
            .expect("explicit late-fault offset should not move OFD state")
            .offset(),
        2
    );
    task.close_fd(first_read_fd)
        .expect("first splice pipe read fd should close");
    task.close_fd(first_write_fd)
        .expect("first splice pipe write fd should close");

    let (to_file_read, to_file_write) = PipeNode::pair();
    let (to_file_read_fd, to_file_write_fd) = task
        .add_file_pair_with_cloexec(FLike::Pipe(to_file_read), FLike::Pipe(to_file_write), false)
        .expect("pipe-to-file splice fixture should allocate descriptors");
    assert_eq!(
        task.get_fd_entry(to_file_write_fd)
            .expect("pipe-to-file writer should exist")
            .write(task.id(), b"XYZ"),
        Ok(FdWriteOutcome::Written(3))
    );
    write_user_off(kernel, &task, output_offset_addr, 1);
    assert_eq!(
        kernel.dispatch_syscall_without_signal_delivery(
            SYS_SPLICE,
            to_file_read_fd,
            0,
            dst_fd,
            output_offset_addr,
            3,
            0,
        ),
        Ok(3)
    );
    assert_eq!(read_user_off(kernel, &task, output_offset_addr), 4);
    assert_eq!(
        task.get_fd_entry(dst_fd)
            .expect("splice destination descriptor should exist")
            .offset(),
        0
    );
    let dst_instance = kernel
        .vfs
        .resolve("/tmp/qemu-splice-dst")
        .expect("splice destination should resolve")
        .path_ref;
    let mut dst_bytes = [0u8; 5];
    assert_eq!(dst_instance.read_at(0, &mut dst_bytes), Ok(5));
    assert_eq!(&dst_bytes, b".XYZ.");
    task.close_fd(to_file_read_fd)
        .expect("pipe-to-file read fd should close");
    task.close_fd(to_file_write_fd)
        .expect("pipe-to-file write fd should close");

    let append_fd = kernel
        .dispatch_syscall_without_signal_delivery(
            SYS_OPENAT,
            0,
            dst_path_addr,
            O_APPEND | 1,
            0,
            0,
            0,
        )
        .expect("append splice destination should open");
    let (append_read, append_write) = PipeNode::pair();
    let (append_read_fd, append_write_fd) = task
        .add_file_pair_with_cloexec(FLike::Pipe(append_read), FLike::Pipe(append_write), false)
        .expect("append rejection pipe should allocate descriptors");
    assert_eq!(
        task.get_fd_entry(append_write_fd)
            .expect("append rejection writer should exist")
            .write(task.id(), b"Q"),
        Ok(FdWriteOutcome::Written(1))
    );
    assert_eq!(
        kernel.dispatch_syscall_without_signal_delivery(
            SYS_SPLICE,
            append_read_fd,
            0,
            append_fd,
            0,
            1,
            0,
        ),
        Err("einval")
    );
    assert_eq!(
        task.get_fd_entry(append_read_fd)
            .expect("append rejection should preserve pipe bytes")
            .io_ctl(FIONREAD),
        Ok(1)
    );
    task.close_fd(append_read_fd)
        .expect("append pipe read fd should close");
    task.close_fd(append_write_fd)
        .expect("append pipe write fd should close");
    task.close_fd(append_fd)
        .expect("append destination fd should close");

    let (input_read, input_write) = PipeNode::pair();
    let (input_read_fd, input_write_fd) = task
        .add_file_pair_with_cloexec(FLike::Pipe(input_read), FLike::Pipe(input_write), false)
        .expect("pipe-to-pipe input should allocate descriptors");
    let (output_read, output_write) = PipeNode::pair();
    let (output_read_fd, output_write_fd) = task
        .add_file_pair_with_cloexec(FLike::Pipe(output_read), FLike::Pipe(output_write), false)
        .expect("pipe-to-pipe output should allocate descriptors");
    assert_eq!(
        task.get_fd_entry(input_write_fd)
            .expect("pipe-to-pipe writer should exist")
            .write(task.id(), b"pq"),
        Ok(FdWriteOutcome::Written(2))
    );
    assert_eq!(
        kernel.dispatch_syscall_without_signal_delivery(
            SYS_SPLICE,
            input_read_fd,
            0,
            output_write_fd,
            0,
            2,
            0,
        ),
        Ok(2)
    );
    let mut pipe_bytes = [0u8; 2];
    assert_eq!(
        task.get_fd_entry(output_read_fd)
            .expect("pipe-to-pipe output should be readable")
            .read(task.id(), &mut pipe_bytes),
        Ok(2)
    );
    assert_eq!(&pipe_bytes, b"pq");
    assert_eq!(
        kernel.dispatch_syscall_without_signal_delivery(
            SYS_SPLICE,
            input_read_fd,
            0,
            input_write_fd,
            0,
            1,
            SPLICE_F_NONBLOCK,
        ),
        Err("einval")
    );
    assert_eq!(
        kernel.dispatch_syscall_without_signal_delivery(
            SYS_SPLICE,
            input_read_fd,
            0,
            dst_fd,
            0,
            1,
            SPLICE_F_NONBLOCK,
        ),
        Err("eagain")
    );
    assert_eq!(
        kernel.dispatch_syscall_without_signal_delivery(
            SYS_SPLICE,
            input_read_fd,
            input_offset_addr,
            dst_fd,
            0,
            1,
            0,
        ),
        Err("espipe")
    );
    assert_eq!(
        kernel.dispatch_syscall_without_signal_delivery(
            SYS_SPLICE,
            input_write_fd,
            0,
            dst_fd,
            0,
            1,
            SPLICE_F_NONBLOCK,
        ),
        Err("ebadf")
    );
    assert_eq!(
        kernel.dispatch_syscall_without_signal_delivery(
            SYS_SPLICE,
            src_fd,
            0,
            input_read_fd,
            0,
            1,
            SPLICE_F_NONBLOCK,
        ),
        Err("ebadf")
    );
    task.close_fd(input_read_fd)
        .expect("pipe-to-pipe input read fd should close");
    task.close_fd(input_write_fd)
        .expect("pipe-to-pipe input write fd should close");
    task.close_fd(output_read_fd)
        .expect("pipe-to-pipe output read fd should close");
    task.close_fd(output_write_fd)
        .expect("pipe-to-pipe output write fd should close");

    task.process
        .sig_queue
        .lock()
        .unwrap()
        .retain(|(signo, _)| *signo != SIGPIPE as i32);
    let source_offset_before_broken = task
        .get_fd_entry(src_fd)
        .expect("broken splice source should exist")
        .offset();
    let (broken_read, broken_write) = PipeNode::pair();
    let (broken_read_fd, broken_write_fd) = task
        .add_file_pair_with_cloexec(FLike::Pipe(broken_read), FLike::Pipe(broken_write), false)
        .expect("broken splice pipe should allocate descriptors");
    task.close_fd(broken_read_fd)
        .expect("broken splice should close its last reader");
    assert_eq!(
        kernel.dispatch_syscall_without_signal_delivery(
            SYS_SPLICE,
            src_fd,
            0,
            broken_write_fd,
            0,
            1,
            0,
        ),
        Err("epipe")
    );
    assert_eq!(
        task.get_fd_entry(src_fd)
            .expect("broken splice source should remain installed")
            .offset(),
        source_offset_before_broken
    );
    assert!(
        task.process
            .sig_queue
            .lock()
            .unwrap()
            .iter()
            .any(|(signo, _)| *signo == SIGPIPE as i32),
        "broken file-to-pipe splice should queue SIGPIPE"
    );
    task.process
        .sig_queue
        .lock()
        .unwrap()
        .retain(|(signo, _)| *signo != SIGPIPE as i32);
    task.close_fd(broken_write_fd)
        .expect("broken splice write fd should close");

    let (eof_read, eof_write) = PipeNode::pair();
    let (eof_read_fd, eof_write_fd) = task
        .add_file_pair_with_cloexec(FLike::Pipe(eof_read), FLike::Pipe(eof_write), false)
        .expect("EOF splice pipe should allocate descriptors");
    task.close_fd(eof_write_fd)
        .expect("EOF splice should close its last writer");
    assert_eq!(
        kernel.dispatch_syscall_without_signal_delivery(
            SYS_SPLICE,
            eof_read_fd,
            0,
            dst_fd,
            0,
            1,
            0,
        ),
        Ok(0)
    );
    task.close_fd(eof_read_fd)
        .expect("EOF splice read fd should close");

    task.close_fd(src_fd)
        .expect("splice source fd should close");
    task.close_fd(dst_fd)
        .expect("splice destination fd should close");
}
