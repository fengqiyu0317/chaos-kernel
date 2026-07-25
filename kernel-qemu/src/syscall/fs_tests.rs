// AGENT: QEMU filesystem syscall regressions that require the installed frame
// pool, current init task, Sv39 mappings, and authoritative AddrSpace usercopy.
use super::*;
use crate::syscall_abi::{
    map_riscv_nr, INTERNAL_SYS_MOUNT, INTERNAL_SYS_OPENAT, INTERNAL_SYS_UMOUNT2, RISCV_SYS_MOUNT,
    RISCV_SYS_OPENAT, RISCV_SYS_UMOUNT2,
};

const USER_STRINGS_BASE: usize = 0x4000_0000;
const OPEN_USER_BASE: usize = 0x4001_0000;

// AGENT: Run filesystem ABI and semantic regressions after QEMU installs the
// real kernel frame pool, current init task, Sv39 mappings, and fd table.
pub fn run_all(kernel: &Kernel) {
    mount_and_umount2_use_usercopy_and_mutate_mount_table(kernel);
    pathname_lookup_returns_shared_file_node(kernel);
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

// AGENT: Verify RISC-V number mapping, real AddrSpace string copy, mount
// replacement, unsupported flags, exact unmount, and missing-mount errors.
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
    write_user_string(kernel, &task, source_addr, "dev0");
    write_user_string(kernel, &task, target_addr, "/mnt");
    write_user_string(kernel, &task, filesystem_type_addr, "chaosfs");

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
    assert_eq!(
        kernel.mnt.resolve("/mnt/file"),
        Ok("dev0:/file".to_string())
    );

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
    assert_eq!(
        kernel.mnt.resolve("/mnt/file"),
        Ok("dev1:/file".to_string())
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
            USER_STRINGS_BASE + PAGE_SZ * 2,
            target_addr,
            filesystem_type_addr,
            0,
            0,
            0,
        ),
        Err("efault")
    );

    assert_eq!(
        kernel.dispatch_syscall_without_signal_delivery(SYS_UMOUNT2, target_addr, 0, 0, 0, 0, 0,),
        Ok(0)
    );
    assert_eq!(kernel.mnt.resolve("/mnt/file"), Ok("/mnt/file".to_string()));
    assert_eq!(
        kernel.dispatch_syscall_without_signal_delivery(SYS_UMOUNT2, target_addr, 0, 0, 0, 0, 0,),
        Err("einval")
    );
}

// AGENT: verify that pathname lookup normalizes aliases, applies mount
// translation, returns one shared inode-like FileNode, and reports absence.
#[cfg_attr(test, test)]
fn pathname_lookup_returns_shared_file_node(kernel: &Kernel) {
    kernel
        .install_file("/tmp/qemu-lookup", b"node".to_vec(), false)
        .expect("lookup test file should install");

    let direct = kernel
        .lookup_file_node("/tmp/qemu-lookup")
        .expect("canonical lookup path should resolve");
    let alias = kernel
        .lookup_file_node("/tmp//unused/.././qemu-lookup")
        .expect("normalized lookup alias should resolve");
    assert_eq!(direct.path, "/tmp/qemu-lookup");
    assert_eq!(alias.path, direct.path);
    assert!(Arc::ptr_eq(&direct.node, &alias.node));
    assert!(matches!(
        kernel.lookup_file_node("/tmp/qemu-lookup-missing"),
        Err("enoent")
    ));

    kernel
        .mnt
        .mount("/lookup-mnt", "lookupdev")
        .expect("lookup test mount should install");
    kernel
        .install_file("/lookup-mnt/file", b"mounted".to_vec(), false)
        .expect("mounted lookup test file should install");
    let mounted = kernel
        .lookup_file_node("/lookup-mnt/./file")
        .expect("mounted lookup path should resolve");
    assert_eq!(mounted.path, "lookupdev:/file");
    kernel
        .mnt
        .umount("/lookup-mnt")
        .expect("lookup test mount should uninstall");
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
            .node
            .len(),
        4
    );
    for fd in fillers {
        task.close_fd(fd).expect("filler fd should close");
    }
}
