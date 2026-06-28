#![allow(dead_code)]

use core::cmp::min;

use crate::syscall::{
    SyscallRequest, INTERNAL_SYS_EXIT, INTERNAL_SYS_GETPID, INTERNAL_SYS_READ, INTERNAL_SYS_WRITE,
};
use crate::{println, sbi, syscall};

const STDIN_FD: usize = 0;
const STDOUT_FD: usize = 1;
const STDERR_FD: usize = 2;
const INIT_PID: usize = 1;
const MAX_RW_COUNT: usize = 4096 * 16;

const EBADF_RET: usize = (-9isize) as usize;
const EFAULT_RET: usize = (-14isize) as usize;

// AGENT: Dispatch the first migrated syscall subset behind the RISC-V ABI adapter.
pub fn dispatch_syscall(request: SyscallRequest) -> usize {
    match request.internal_nr {
        Some(INTERNAL_SYS_READ) => sys_read(&request.args),
        Some(INTERNAL_SYS_WRITE) => sys_write(&request.args),
        Some(INTERNAL_SYS_EXIT) => sys_exit(&request.args),
        Some(INTERNAL_SYS_GETPID) => sys_getpid(&request.args),
        Some(_) | None => syscall::ENOSYS_RET,
    }
}

// AGENT: Minimal read semantics for the early QEMU carrier; stdin has no backend yet.
fn sys_read(args: &[usize; 6]) -> usize {
    let fd = args[0];
    let buf_addr = args[1];
    let count = args[2];
    if count == 0 {
        return 0;
    }
    if buf_addr == 0 {
        return EFAULT_RET;
    }
    match fd {
        STDIN_FD => 0,
        _ => EBADF_RET,
    }
}

// AGENT: Minimal write semantics route stdout/stderr bytes to the SBI console backend.
fn sys_write(args: &[usize; 6]) -> usize {
    let fd = args[0];
    let buf_addr = args[1];
    let count = args[2];
    if count == 0 {
        return 0;
    }
    if buf_addr == 0 {
        return EFAULT_RET;
    }
    match fd {
        STDOUT_FD | STDERR_FD => write_console(buf_addr, count),
        _ => EBADF_RET,
    }
}

// AGENT: Copy early user bytes by direct S-mode loads until Sv39 copy_from_user exists.
fn write_console(buf_addr: usize, count: usize) -> usize {
    let len = min(count, MAX_RW_COUNT);
    let ptr = buf_addr as *const u8;
    for offset in 0..len {
        let byte = unsafe { ptr.add(offset).read_volatile() };
        sbi::console_putchar(byte);
    }
    len
}

// AGENT: Exit currently terminates the single early init path through SBI shutdown.
fn sys_exit(args: &[usize; 6]) -> usize {
    let code = args[0];
    println!("[kernel-qemu] init exit status={}", code);
    sbi::shutdown();
}

// AGENT: Report the first process id until real kernel-qemu task state is migrated.
fn sys_getpid(_args: &[usize; 6]) -> usize {
    INIT_PID
}
