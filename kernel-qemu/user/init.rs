#![no_main]
#![no_std]

use core::arch::asm;
use core::panic::PanicInfo;

const SYS_WRITE: usize = 64;
const SYS_OPENAT: usize = 56;
const SYS_EXIT: usize = 93;
const AT_FDCWD: usize = (-100isize) as usize;
const O_WRONLY: usize = 1;
const O_CREAT: usize = 0o100;
const STDOUT_FILENO: usize = 1;
const INIT_MESSAGE: &[u8] = b"[init] userspace /bin/init reached\n";
const OPEN_MESSAGE: &[u8] = b"[init] openat round-trip passed\n";
const OPEN_PATH: &[u8] = b"/tmp/init-openat\0";
const OPEN_PAYLOAD: &[u8] = b"openat-ok";

// AGENT: issue one Linux RISC-V openat-style four-argument syscall so the
// embedded init validates the live dirfd/path/flags/mode trap ABI.
#[inline(always)]
unsafe fn syscall4(number: usize, arg0: usize, arg1: usize, arg2: usize, arg3: usize) -> isize {
    let mut result = arg0;
    unsafe {
        asm!(
            "ecall",
            inlateout("a0") result,
            in("a1") arg1,
            in("a2") arg2,
            in("a3") arg3,
            in("a7") number,
            options(nostack)
        );
    }
    result as isize
}

// AGENT: issue one Linux RISC-V syscall with the write-style three-argument ABI.
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

// AGENT: issue the non-returning exit syscall while retaining a return value
// only for the defensive fallback when a broken kernel returns unexpectedly.
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

// AGENT: prove the user-mode write and openat paths, including regular-file OFD
// dispatch, and report any short or failed syscall through init's exit status.
#[no_mangle]
#[link_section = ".text.entry"]
pub extern "C" fn _start() -> ! {
    let console_written = unsafe {
        syscall3(
            SYS_WRITE,
            STDOUT_FILENO,
            INIT_MESSAGE.as_ptr() as usize,
            INIT_MESSAGE.len(),
        )
    };
    let opened = unsafe {
        syscall4(
            SYS_OPENAT,
            AT_FDCWD,
            OPEN_PATH.as_ptr() as usize,
            O_CREAT | O_WRONLY,
            0o600,
        )
    };
    let file_written = if opened >= 0 {
        unsafe {
            syscall3(
                SYS_WRITE,
                opened as usize,
                OPEN_PAYLOAD.as_ptr() as usize,
                OPEN_PAYLOAD.len(),
            )
        }
    } else {
        opened
    };
    let open_message_written = if file_written == OPEN_PAYLOAD.len() as isize {
        unsafe {
            syscall3(
                SYS_WRITE,
                STDOUT_FILENO,
                OPEN_MESSAGE.as_ptr() as usize,
                OPEN_MESSAGE.len(),
            )
        }
    } else {
        file_written
    };
    let status = usize::from(
        console_written != INIT_MESSAGE.len() as isize
            || file_written != OPEN_PAYLOAD.len() as isize
            || open_message_written != OPEN_MESSAGE.len() as isize,
    );
    let _ = unsafe { syscall1(SYS_EXIT, status) };

    loop {
        core::hint::spin_loop();
    }
}

// AGENT: keep the standalone no_std user binary linkable without importing a runtime.
#[panic_handler]
fn panic(_info: &PanicInfo<'_>) -> ! {
    loop {
        core::hint::spin_loop();
    }
}
