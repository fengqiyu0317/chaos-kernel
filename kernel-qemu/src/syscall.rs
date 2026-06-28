#![allow(dead_code)]

use crate::trap::TrapFrame;

pub const ENOSYS_RET: usize = (-38isize) as usize;

pub const RISCV_SYS_READ: usize = 63;
pub const RISCV_SYS_WRITE: usize = 64;
pub const RISCV_SYS_EXIT: usize = 93;
pub const RISCV_SYS_GETPID: usize = 172;

pub const INTERNAL_SYS_READ: usize = 0;
pub const INTERNAL_SYS_WRITE: usize = 1;
pub const INTERNAL_SYS_EXIT: usize = 60;
pub const INTERNAL_SYS_GETPID: usize = 39;

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
        RISCV_SYS_READ => Some(INTERNAL_SYS_READ),
        RISCV_SYS_WRITE => Some(INTERNAL_SYS_WRITE),
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
    let ret = dispatch_migrated_semantics(request);
    write_return(frame, ret);
}

// AGENT: Keep syscall behavior behind a semantic entry rather than in the trap layer.
fn dispatch_migrated_semantics(request: SyscallRequest) -> usize {
    crate::semantics::dispatch_syscall(request)
}

// AGENT: Store the architecture-level syscall return value in a0.
pub fn write_return(frame: &mut TrapFrame, value: usize) {
    frame.set_return_value(value);
}
