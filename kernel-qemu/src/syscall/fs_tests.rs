// AGENT: QEMU filesystem syscall regressions that require the installed frame
// pool, current init task, Sv39 mappings, and authoritative AddrSpace usercopy.
use super::*;
use crate::syscall_abi::{
    map_riscv_nr, INTERNAL_SYS_MOUNT, INTERNAL_SYS_UMOUNT2, RISCV_SYS_MOUNT, RISCV_SYS_UMOUNT2,
};

const USER_STRINGS_BASE: usize = 0x4000_0000;

// AGENT: Run the mount/umount2 ABI and semantic regression after QEMU installs
// the real kernel frame pool and current init task.
pub fn run_all(kernel: &Kernel) {
    mount_and_umount2_use_usercopy_and_mutate_mount_table(kernel);
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
