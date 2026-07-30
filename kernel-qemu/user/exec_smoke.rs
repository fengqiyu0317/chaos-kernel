#![no_main]
#![no_std]

use core::arch::{asm, global_asm};
use core::panic::PanicInfo;

const SYS_WRITE: usize = 64;
const SYS_FSTAT: usize = 80;
const SYS_EXIT: usize = 93;
const STDOUT_FILENO: usize = 1;
const EXEC_CLOEXEC_FD: usize = 101;
const EBADF_RET: isize = -9;
const RISCV64_STAT_SIZE: usize = 128;
const EXPECTED_ARG0: &[u8] = b"exec-smoke";
const EXPECTED_ENV0: &[u8] = b"EXEC_TEST=1";
const SUCCESS_MESSAGE: &[u8] = b"[init] execve round-trip passed\n";

// AGENT: capture the kernel-provided initial stack pointer before any Rust
// function prologue can move sp, then pass it as the first Rust argument.
global_asm!(
    r#"
    .section .text.entry
    .globl _start
_start:
    mv a0, sp
    call exec_smoke_main
1:
    j 1b
"#
);

// AGENT: issue one Linux RISC-V syscall with three arguments.
#[inline(always)]
unsafe fn syscall3(number: usize, arg0: usize, arg1: usize, arg2: usize) -> isize {
    let mut result = arg0;
    unsafe {
        asm!(
            "ecall",
            inlateout("a0") result,
            in("a1") arg1,
            in("a2") arg2,
            in("a7") number,
            options(nostack)
        );
    }
    result as isize
}

// AGENT: issue one Linux RISC-V syscall with one argument.
#[inline(always)]
unsafe fn syscall1(number: usize, arg0: usize) -> isize {
    let mut result = arg0;
    unsafe {
        asm!(
            "ecall",
            inlateout("a0") result,
            in("a7") number,
            options(nostack)
        );
    }
    result as isize
}

// AGENT: compare one NUL-terminated initial-stack string without depending on
// a userspace runtime or allocating in the freshly installed image.
unsafe fn user_c_string_eq(ptr: usize, expected: &[u8]) -> bool {
    if ptr == 0 {
        return false;
    }
    for (offset, expected_byte) in expected.iter().enumerate() {
        if unsafe { (ptr as *const u8).add(offset).read() } != *expected_byte {
            return false;
        }
    }
    unsafe { (ptr as *const u8).add(expected.len()).read() == 0 }
}

// AGENT: validate argc/argv/envp from the pristine exec stack, prove the fixed
// CLOEXEC fd is gone, and use inherited stdout to report the completed round trip.
#[no_mangle]
pub extern "C" fn exec_smoke_main(initial_sp: usize) -> ! {
    let words = initial_sp as *const usize;
    let argc = unsafe { words.read() };
    let argv0 = unsafe { words.add(1).read() };
    let argv_end = unsafe { words.add(2).read() };
    let env0 = unsafe { words.add(3).read() };
    let env_end = unsafe { words.add(4).read() };
    let stack_ok = argc == 1
        && argv_end == 0
        && env_end == 0
        && unsafe { user_c_string_eq(argv0, EXPECTED_ARG0) }
        && unsafe { user_c_string_eq(env0, EXPECTED_ENV0) };

    let mut stat = [0u8; RISCV64_STAT_SIZE];
    let cloexec_closed =
        unsafe { syscall3(SYS_FSTAT, EXEC_CLOEXEC_FD, stat.as_mut_ptr() as usize, 0) } == EBADF_RET;

    let message_written = if stack_ok && cloexec_closed {
        unsafe {
            syscall3(
                SYS_WRITE,
                STDOUT_FILENO,
                SUCCESS_MESSAGE.as_ptr() as usize,
                SUCCESS_MESSAGE.len(),
            )
        }
    } else {
        -1
    };
    let status = usize::from(message_written != SUCCESS_MESSAGE.len() as isize);
    let _ = unsafe { syscall1(SYS_EXIT, status) };
    loop {
        core::hint::spin_loop();
    }
}

// AGENT: keep the standalone no_std exec target linkable without a runtime.
#[panic_handler]
fn panic(_info: &PanicInfo<'_>) -> ! {
    loop {
        core::hint::spin_loop();
    }
}
