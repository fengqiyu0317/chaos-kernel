#![no_main]
#![no_std]

use core::arch::asm;
use core::panic::PanicInfo;

const SYS_WRITE: usize = 64;
const SYS_MKDIRAT: usize = 34;
const SYS_OPENAT: usize = 56;
const SYS_CLOSE: usize = 57;
const SYS_PIPE2: usize = 59;
const SYS_READ: usize = 63;
const SYS_NEWFSTATAT: usize = 79;
const SYS_FSTAT: usize = 80;
const SYS_EXIT: usize = 93;
const AT_FDCWD: usize = (-100isize) as usize;
const O_WRONLY: usize = 1;
const O_CREAT: usize = 0o100;
const STDOUT_FILENO: usize = 1;
const INIT_MESSAGE: &[u8] = b"[init] userspace /bin/init reached\n";
const MKDIR_MESSAGE: &[u8] = b"[init] mkdirat round-trip passed\n";
const OPEN_MESSAGE: &[u8] = b"[init] openat round-trip passed\n";
const PIPE_MESSAGE: &[u8] = b"[init] pipe2 round-trip passed\n";
const STAT_MESSAGE: &[u8] = b"[init] stat round-trip passed\n";
const CLOSE_MESSAGE: &[u8] = b"[init] close round-trip passed\n";
const MKDIR_PATH: &[u8] = b"/tmp/init-mkdirat\0";
const OPEN_PATH: &[u8] = b"/tmp/init-mkdirat/file\0";
const OPEN_PAYLOAD: &[u8] = b"openat-ok";
const PIPE_PAYLOAD: &[u8] = b"pipe2-ok";
const RISCV64_STAT_SIZE: usize = 128;
const S_IFMT: u32 = 0o170000;
const S_IFREG: u32 = 0o100000;

// AGENT: issue one Linux RISC-V four-argument syscall so embedded init can
// validate both openat and newfstatat through the live trap ABI.
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

// AGENT: issue one Linux RISC-V two-argument syscall for the real fstat ABI.
#[inline(always)]
unsafe fn syscall2(number: usize, arg0: usize, arg1: usize) -> isize {
    let mut result = arg0;
    unsafe {
        asm!(
            "ecall",
            inlateout("a0") result,
            in("a1") arg1,
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

// AGENT: decode one native RV64 stat u32 without depending on a userspace C ABI
// structure or introducing potentially uninitialized Rust padding.
fn stat_u32(bytes: &[u8; RISCV64_STAT_SIZE], offset: usize) -> u32 {
    u32::from_le_bytes([
        bytes[offset],
        bytes[offset + 1],
        bytes[offset + 2],
        bytes[offset + 3],
    ])
}

// AGENT: decode one native RV64 stat u64 from the kernel's fixed ABI offsets.
fn stat_u64(bytes: &[u8; RISCV64_STAT_SIZE], offset: usize) -> u64 {
    u64::from_le_bytes([
        bytes[offset],
        bytes[offset + 1],
        bytes[offset + 2],
        bytes[offset + 3],
        bytes[offset + 4],
        bytes[offset + 5],
        bytes[offset + 6],
        bytes[offset + 7],
    ])
}

// AGENT: prove user-mode mkdirat/openat/pipe2/fstat/newfstatat/close, including
// real descriptor/stat copyout and OFD teardown, then report failures via status.
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
    let mkdir_result =
        unsafe { syscall3(SYS_MKDIRAT, AT_FDCWD, MKDIR_PATH.as_ptr() as usize, 0o700) };
    let mkdir_message_written = if mkdir_result == 0 {
        unsafe {
            syscall3(
                SYS_WRITE,
                STDOUT_FILENO,
                MKDIR_MESSAGE.as_ptr() as usize,
                MKDIR_MESSAGE.len(),
            )
        }
    } else {
        mkdir_result
    };
    let opened = if mkdir_result == 0 {
        unsafe {
            syscall4(
                SYS_OPENAT,
                AT_FDCWD,
                OPEN_PATH.as_ptr() as usize,
                O_CREAT | O_WRONLY,
                0o600,
            )
        }
    } else {
        mkdir_result
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
    let mut fd_stat = [0u8; RISCV64_STAT_SIZE];
    let fstat_result = if file_written == OPEN_PAYLOAD.len() as isize {
        unsafe { syscall2(SYS_FSTAT, opened as usize, fd_stat.as_mut_ptr() as usize) }
    } else {
        file_written
    };
    let mut path_stat = [0u8; RISCV64_STAT_SIZE];
    let newfstatat_result = if fstat_result == 0 {
        unsafe {
            syscall4(
                SYS_NEWFSTATAT,
                AT_FDCWD,
                OPEN_PATH.as_ptr() as usize,
                path_stat.as_mut_ptr() as usize,
                0,
            )
        }
    } else {
        fstat_result
    };
    let stat_round_trip_ok = fstat_result == 0
        && newfstatat_result == 0
        && stat_u64(&fd_stat, 8) != 0
        && stat_u32(&fd_stat, 16) & S_IFMT == S_IFREG
        && stat_u64(&fd_stat, 48) == OPEN_PAYLOAD.len() as u64
        && stat_u32(&fd_stat, 56) == 512
        && stat_u64(&fd_stat, 64) == 1
        && fd_stat == path_stat;
    let stat_message_written = if stat_round_trip_ok {
        unsafe {
            syscall3(
                SYS_WRITE,
                STDOUT_FILENO,
                STAT_MESSAGE.as_ptr() as usize,
                STAT_MESSAGE.len(),
            )
        }
    } else {
        -1
    };
    let mut pipe_fds = [-1i32; 2];
    let pipe_result = if stat_round_trip_ok {
        unsafe { syscall2(SYS_PIPE2, pipe_fds.as_mut_ptr() as usize, 0) }
    } else {
        -1
    };
    let pipe_written = if pipe_result == 0 && pipe_fds[1] >= 0 {
        unsafe {
            syscall3(
                SYS_WRITE,
                pipe_fds[1] as usize,
                PIPE_PAYLOAD.as_ptr() as usize,
                PIPE_PAYLOAD.len(),
            )
        }
    } else {
        pipe_result
    };
    let mut pipe_output = [0u8; PIPE_PAYLOAD.len()];
    let pipe_read = if pipe_written == PIPE_PAYLOAD.len() as isize && pipe_fds[0] >= 0 {
        unsafe {
            syscall3(
                SYS_READ,
                pipe_fds[0] as usize,
                pipe_output.as_mut_ptr() as usize,
                pipe_output.len(),
            )
        }
    } else {
        pipe_written
    };
    let pipe_read_close = if pipe_result == 0 && pipe_fds[0] >= 0 {
        unsafe { syscall1(SYS_CLOSE, pipe_fds[0] as usize) }
    } else {
        pipe_result
    };
    let pipe_write_close = if pipe_result == 0 && pipe_fds[1] >= 0 {
        unsafe { syscall1(SYS_CLOSE, pipe_fds[1] as usize) }
    } else {
        pipe_result
    };
    let pipe_round_trip_ok = pipe_result == 0
        && pipe_written == PIPE_PAYLOAD.len() as isize
        && pipe_read == PIPE_PAYLOAD.len() as isize
        && pipe_output == PIPE_PAYLOAD
        && pipe_read_close == 0
        && pipe_write_close == 0;
    let pipe_message_written = if pipe_round_trip_ok {
        unsafe {
            syscall3(
                SYS_WRITE,
                STDOUT_FILENO,
                PIPE_MESSAGE.as_ptr() as usize,
                PIPE_MESSAGE.len(),
            )
        }
    } else {
        -1
    };
    let close_result = if file_written == OPEN_PAYLOAD.len() as isize {
        unsafe { syscall1(SYS_CLOSE, opened as usize) }
    } else {
        file_written
    };
    let close_message_written = if close_result == 0 {
        unsafe {
            syscall3(
                SYS_WRITE,
                STDOUT_FILENO,
                CLOSE_MESSAGE.as_ptr() as usize,
                CLOSE_MESSAGE.len(),
            )
        }
    } else {
        close_result
    };
    let status = usize::from(
        console_written != INIT_MESSAGE.len() as isize
            || mkdir_result != 0
            || mkdir_message_written != MKDIR_MESSAGE.len() as isize
            || file_written != OPEN_PAYLOAD.len() as isize
            || open_message_written != OPEN_MESSAGE.len() as isize
            || !stat_round_trip_ok
            || stat_message_written != STAT_MESSAGE.len() as isize
            || !pipe_round_trip_ok
            || pipe_message_written != PIPE_MESSAGE.len() as isize
            || close_result != 0
            || close_message_written != CLOSE_MESSAGE.len() as isize,
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
