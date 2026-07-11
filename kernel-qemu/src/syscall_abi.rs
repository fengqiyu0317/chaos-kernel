#![allow(dead_code)]

use crate::trap::TrapFrame;

pub const ENOSYS_RET: usize = (-38isize) as usize;

// AGENT: Linux asm-generic syscall numbers used by the RISC-V ABI.
pub const RISCV_SYS_UMOUNT2: usize = 39;
pub const RISCV_SYS_MOUNT: usize = 40;
pub const RISCV_SYS_READ: usize = 63;
pub const RISCV_SYS_WRITE: usize = 64;
pub const RISCV_SYS_BRK: usize = 214;
pub const RISCV_SYS_EXIT: usize = 93;
pub const RISCV_SYS_GETPID: usize = 172;

pub const INTERNAL_SYS_READ: usize = 0;
pub const INTERNAL_SYS_WRITE: usize = 1;
pub const INTERNAL_SYS_BRK: usize = 12;
pub const INTERNAL_SYS_EXIT: usize = 60;
pub const INTERNAL_SYS_GETPID: usize = 39;
pub const INTERNAL_SYS_MOUNT: usize = 165;
pub const INTERNAL_SYS_UMOUNT2: usize = 166;

// AGENT: Decoded RISC-V syscall request before it reaches migrated kernel-sim semantics.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SyscallRequest {
    pub riscv_nr: usize,
    pub internal_nr: Option<usize>,
    pub args: [usize; 6],
}

// AGENT: Convert the small first-stage RISC-V syscall set to kernel-sim-style ids.
pub fn map_riscv_nr(nr: usize) -> Option<usize> {
    match nr {
        RISCV_SYS_UMOUNT2 => Some(INTERNAL_SYS_UMOUNT2),
        RISCV_SYS_MOUNT => Some(INTERNAL_SYS_MOUNT),
        RISCV_SYS_READ => Some(INTERNAL_SYS_READ),
        RISCV_SYS_WRITE => Some(INTERNAL_SYS_WRITE),
        RISCV_SYS_BRK => Some(INTERNAL_SYS_BRK),
        RISCV_SYS_EXIT => Some(INTERNAL_SYS_EXIT),
        RISCV_SYS_GETPID => Some(INTERNAL_SYS_GETPID),
        _ => None,
    }
}

// AGENT: Decode a request from the trap frame without implementing syscall behavior here.
pub fn decode_from_trap_frame(frame: &TrapFrame) -> SyscallRequest {
    let riscv_nr = frame.syscall_nr();
    SyscallRequest {
        riscv_nr,
        internal_nr: map_riscv_nr(riscv_nr),
        args: frame.syscall_args(),
    }
}

// AGENT: Complete the RISC-V ABI adapter step and forward to migrated semantics.
pub fn dispatch_from_trap_frame(frame: &mut TrapFrame) {
    let request = decode_from_trap_frame(frame);
    dispatch_migrated_semantics(request, frame);
}

// AGENT: Keep syscall behavior behind a semantic entry rather than in the trap layer.
fn dispatch_migrated_semantics(request: SyscallRequest, frame: &mut TrapFrame) {
    match crate::kernel::qemu_wait_kernel() {
        Some(kernel) => dispatch_installed_kernel(kernel, request, frame),
        None => write_return(frame, crate::semantics::dispatch_syscall(request)),
    }
}

// AGENT: route RISC-V syscalls into the installed migrated Kernel instead of
// keeping behavior in the early carrier-only semantics shim.
fn dispatch_installed_kernel(
    kernel: &crate::kernel::Kernel,
    request: SyscallRequest,
    frame: &mut TrapFrame,
) {
    let Some(nr) = request.internal_nr else {
        write_return(frame, ENOSYS_RET);
        return;
    };
    let [a0, a1, a2, a3, a4, a5] = request.args;
    let ret = match kernel.dispatch_syscall_without_signal_delivery(nr, a0, a1, a2, a3, a4, a5) {
        Ok(value) => value,
        Err(err) => errno_ret(err),
    };
    write_return(frame, ret);
    if nr == INTERNAL_SYS_EXIT {
        crate::sbi::shutdown();
    }
    if nr == crate::kernel::SYS_SIGRETURN {
        if let Some(ctx) = kernel.current_user_context(0) {
            frame.apply_user_context(&ctx);
        }
        return;
    }
    let interrupted = frame.capture_user_context();
    if let Some(next) = kernel.deliver_pending_signals_from_context(0, interrupted) {
        frame.apply_user_context(&next);
    }
}

// AGENT: translate the migrated kernel-sim string errors into Linux-style
// negative syscall return values for the RISC-V ABI boundary.
fn errno_ret(err: &'static str) -> usize {
    let errno = match err {
        "eperm" => 1,
        "enoent" => 2,
        "esrch" => 3,
        "eintr" => 4,
        "eio" => 5,
        "e2big" => 7,
        "echild" => 10,
        "eagain" | "changed" => 11,
        "enomem" | "oom" => 12,
        "eacces" => 13,
        "efault" => 14,
        "ebusy" => 16,
        "eexist" => 17,
        "enodev" => 19,
        "enotdir" => 20,
        "eisdir" => 21,
        "einval" | "ph_overflow" => 22,
        "emfile" => 24,
        "enospc" => 28,
        "enametoolong" => 36,
        "enosys" => 38,
        "removed" => 43,
        "enotsup" => 95,
        "timeout" => 110,
        _ => 22,
    };
    (-(errno as isize)) as usize
}

// AGENT: Store the architecture-level syscall return value in a0.
pub fn write_return(frame: &mut TrapFrame, value: usize) {
    frame.set_return_value(value);
}
