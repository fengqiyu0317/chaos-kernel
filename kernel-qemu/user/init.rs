#![no_main]
#![no_std]

use core::arch::asm;
use core::panic::PanicInfo;

const SYS_WRITE: usize = 64;
const SYS_EXIT: usize = 93;
const STDOUT_FILENO: usize = 1;
const INIT_MESSAGE: &[u8] = b"[init] userspace /bin/init reached\n";

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

// AGENT: prove the first user-mode round trip by writing through fd 1 and then
// exiting with success only when the kernel reports the complete write.
#[no_mangle]
#[link_section = ".text.entry"]
pub extern "C" fn _start() -> ! {
    let written = unsafe {
        syscall3(
            SYS_WRITE,
            STDOUT_FILENO,
            INIT_MESSAGE.as_ptr() as usize,
            INIT_MESSAGE.len(),
        )
    };
    let status = usize::from(written != INIT_MESSAGE.len() as isize);
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
