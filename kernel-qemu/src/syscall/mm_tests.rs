// AGENT: QEMU anonymous mmap regressions that require an installed Kernel,
// current task, real frame pool, Sv39 address space, and RV64 syscall adapter.
use super::*;
use crate::syscall_abi::{
    decode_from_trap_frame, dispatch_from_trap_frame, map_riscv_nr, INTERNAL_SYS_MMAP,
    INTERNAL_SYS_MUNMAP, RISCV_SYS_MMAP, RISCV_SYS_MUNMAP,
};

const HINT_BASE: usize = 0x7100_0001;
const HINT_ALIGNED: usize = 0x7100_1000;
const FIXED_BASE: usize = 0x7200_0000;

// AGENT: run every first-stage anonymous mmap syscall contract and clean each
// temporary VMA so later filesystem/checkpoint selftests inherit a stable task.
pub fn run_all(kernel: &Kernel) {
    rv64_mmap_and_munmap_round_trip(kernel);
    mmap_rejects_invalid_types_and_reserved_signal_page(kernel);
    mmap_honors_hint_conflicts_and_default_fallback(kernel);
    fixed_mmap_replaces_contents_and_permissions(kernel);
}

// AGENT: prove that RV64 syscall numbers and all six argument slots reach the
// installed semantic entry, then observe zero-fill, usercopy, and frame release.
fn rv64_mmap_and_munmap_round_trip(kernel: &Kernel) {
    assert_eq!(map_riscv_nr(RISCV_SYS_MMAP), Some(INTERNAL_SYS_MMAP));
    assert_eq!(map_riscv_nr(RISCV_SYS_MUNMAP), Some(INTERNAL_SYS_MUNMAP));

    let args = [
        0,
        PAGE_SZ,
        PROT_READ | PROT_WRITE,
        MAP_PRIVATE | MAP_ANONYMOUS,
        usize::MAX,
        0,
    ];
    let mut frame = TrapFrame::new();
    frame.regs[10..16].copy_from_slice(&args);
    frame.regs[17] = RISCV_SYS_MMAP;
    let request = decode_from_trap_frame(&frame);
    assert_eq!(request.internal_nr, Some(INTERNAL_SYS_MMAP));
    assert_eq!(request.args, args);
    dispatch_from_trap_frame(&mut frame);
    let mapped = frame.regs[10];
    assert_eq!(mapped, 0x7000_0000);

    let task = kernel
        .cur_task(0)
        .expect("mmap selftest needs current init");
    let mut zeros = [0xffu8; 16];
    task.process
        .addr_space
        .lock()
        .unwrap()
        .read_user_bytes(mapped, &mut zeros)
        .unwrap();
    assert_eq!(zeros, [0u8; 16]);
    task.process
        .addr_space
        .lock()
        .unwrap()
        .write_user_bytes(mapped, b"mmap", &kernel.pool)
        .unwrap();

    let free_while_mapped = kernel.pool.free_count();
    let mut unmap = TrapFrame::new();
    unmap.regs[10] = mapped;
    unmap.regs[11] = PAGE_SZ;
    unmap.regs[17] = RISCV_SYS_MUNMAP;
    dispatch_from_trap_frame(&mut unmap);
    assert_eq!(unmap.regs[10], 0);
    assert!(task
        .process
        .addr_space
        .lock()
        .unwrap()
        .mapped_region(mapped)
        .is_none());
    assert!(kernel.pool.free_count() >= free_while_mapped + 1);
}

// AGENT: enforce exactly one mapping type and keep the kernel-owned rt_sigreturn
// page outside both fixed mmap replacement and user munmap ranges.
fn mmap_rejects_invalid_types_and_reserved_signal_page(kernel: &Kernel) {
    let prot = PROT_READ | PROT_WRITE;
    let anon = MAP_ANONYMOUS;
    assert_eq!(
        sys_mmap(kernel, 0, PAGE_SZ, prot, anon, usize::MAX, 0),
        Err("einval")
    );
    assert_eq!(
        sys_mmap(
            kernel,
            0,
            PAGE_SZ,
            prot,
            anon | MAP_PRIVATE | MAP_SHARED,
            usize::MAX,
            0,
        ),
        Err("einval")
    );
    assert_eq!(
        sys_mmap(kernel, 0, PAGE_SZ, prot, MAP_PRIVATE, 0, 0),
        Err("enosys")
    );
    assert_eq!(
        sys_mmap(
            kernel,
            USER_SIGTRAMP,
            PAGE_SZ,
            prot,
            MAP_FIXED | MAP_PRIVATE | anon,
            usize::MAX,
            0,
        ),
        Err("enomem")
    );
    assert_eq!(sys_munmap(kernel, USER_SIGTRAMP, PAGE_SZ), Err("enomem"));
}

// AGENT: use an unaligned hint when free, advance past its conflict, and fall
// back to the default base when a top-of-user-space hint cannot fit one page.
fn mmap_honors_hint_conflicts_and_default_fallback(kernel: &Kernel) {
    let flags = MAP_PRIVATE | MAP_ANONYMOUS;
    let first = sys_mmap(kernel, HINT_BASE, PAGE_SZ, PROT_READ, flags, usize::MAX, 0).unwrap();
    assert_eq!(first, HINT_ALIGNED);
    let second = sys_mmap(kernel, HINT_BASE, PAGE_SZ, PROT_READ, flags, usize::MAX, 0).unwrap();
    assert_eq!(second, HINT_ALIGNED + PAGE_SZ);
    assert_eq!(sys_munmap(kernel, first, 2 * PAGE_SZ), Ok(0));

    let fallback = sys_mmap(
        kernel,
        USER_SIGTRAMP - PAGE_SZ + 1,
        PAGE_SZ,
        PROT_READ,
        flags,
        usize::MAX,
        0,
    )
    .unwrap();
    assert_eq!(fallback, 0x7000_0000);
    assert_eq!(sys_munmap(kernel, fallback, PAGE_SZ), Ok(0));
}

// AGENT: successful MAP_FIXED replacement must discard old bytes, install the
// new VMA permissions, and remain removable through the ordinary syscall path.
fn fixed_mmap_replaces_contents_and_permissions(kernel: &Kernel) {
    let rw_flags = MAP_FIXED | MAP_PRIVATE | MAP_ANONYMOUS;
    assert_eq!(
        sys_mmap(
            kernel,
            FIXED_BASE,
            PAGE_SZ,
            PROT_READ | PROT_WRITE,
            rw_flags,
            usize::MAX,
            0,
        ),
        Ok(FIXED_BASE)
    );
    let task = kernel.cur_task(0).expect("fixed mmap needs current init");
    task.process
        .addr_space
        .lock()
        .unwrap()
        .write_user_bytes(FIXED_BASE, &[0xa5], &kernel.pool)
        .unwrap();

    assert_eq!(
        sys_mmap(
            kernel,
            FIXED_BASE,
            PAGE_SZ,
            PROT_READ,
            rw_flags,
            usize::MAX,
            0,
        ),
        Ok(FIXED_BASE)
    );
    let mut value = [0xffu8; 1];
    let mut addr_space = task.process.addr_space.lock().unwrap();
    addr_space.read_user_bytes(FIXED_BASE, &mut value).unwrap();
    assert_eq!(value, [0]);
    assert_eq!(
        addr_space.write_user_bytes(FIXED_BASE, &[1], &kernel.pool),
        Err("efault")
    );
    drop(addr_space);
    assert_eq!(sys_munmap(kernel, FIXED_BASE, PAGE_SZ), Ok(0));
}
