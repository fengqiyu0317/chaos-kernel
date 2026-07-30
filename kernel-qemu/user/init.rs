#![no_main]
#![no_std]

use core::arch::asm;
use core::panic::PanicInfo;

const SYS_WRITE: usize = 64;
const SYS_DUP: usize = 23;
const SYS_DUP3: usize = 24;
const SYS_FCNTL: usize = 25;
const SYS_MKDIRAT: usize = 34;
const SYS_OPENAT: usize = 56;
const SYS_CLOSE: usize = 57;
const SYS_PIPE2: usize = 59;
const SYS_READ: usize = 63;
const SYS_SPLICE: usize = 76;
const SYS_NEWFSTATAT: usize = 79;
const SYS_FSTAT: usize = 80;
const SYS_EXIT: usize = 93;
const SYS_EXECVE: usize = 221;
const AT_FDCWD: usize = (-100isize) as usize;
const O_WRONLY: usize = 1;
const O_CREAT: usize = 0o100;
const O_CLOEXEC: usize = 0o2000000;
const O_APPEND: usize = 0o2000;
const F_DUPFD: usize = 0;
const F_GETFD: usize = 1;
const F_SETFD: usize = 2;
const F_GETFL: usize = 3;
const F_SETFL: usize = 4;
const F_GETLK: usize = 5;
const F_SETLK: usize = 6;
const F_SETLKW: usize = 7;
const F_DUPFD_CLOEXEC: usize = 1030;
const F_WRLCK: i16 = 1;
const F_UNLCK: i16 = 2;
const SEEK_SET: i16 = 0;
const FD_CLOEXEC: usize = 1;
const STDOUT_FILENO: usize = 1;
const DUP3_TARGET_FD: usize = 100;
const EXEC_CLOEXEC_FD: usize = 101;
const INIT_MESSAGE: &[u8] = b"[init] userspace /bin/init reached\n";
const MKDIR_MESSAGE: &[u8] = b"[init] mkdirat round-trip passed\n";
const OPEN_MESSAGE: &[u8] = b"[init] openat round-trip passed\n";
const DUP_MESSAGE: &[u8] = b"[init] dup round-trip passed\n";
const DUP3_MESSAGE: &[u8] = b"[init] dup3 round-trip passed\n";
const PIPE_MESSAGE: &[u8] = b"[init] pipe2 round-trip passed\n";
const SPLICE_MESSAGE: &[u8] = b"[init] splice round-trip passed\n";
const STAT_MESSAGE: &[u8] = b"[init] stat round-trip passed\n";
const FCNTL_MESSAGE: &[u8] = b"[init] fcntl nine-command round-trip passed\n";
const CLOSE_MESSAGE: &[u8] = b"[init] close round-trip passed\n";
const EXEC_FAILURE_MESSAGE: &[u8] = b"[init] execve unexpectedly returned\n";
const MKDIR_PATH: &[u8] = b"/tmp/init-mkdirat\0";
const OPEN_PATH: &[u8] = b"/tmp/init-mkdirat/file\0";
const OPEN_PAYLOAD: &[u8] = b"openat-ok";
const EXEC_PATH: &[u8] = b"/bin/exec-smoke\0";
const EXEC_ARG0: &[u8] = b"exec-smoke\0";
const EXEC_ENV0: &[u8] = b"EXEC_TEST=1\0";
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

// AGENT: issue the complete Linux RV64 splice ABI with both off_t pointers,
// length, and flags in a0 through a5.
#[inline(always)]
unsafe fn syscall6(
    number: usize,
    arg0: usize,
    arg1: usize,
    arg2: usize,
    arg3: usize,
    arg4: usize,
    arg5: usize,
) -> isize {
    let mut result = arg0;
    unsafe {
        asm!(
            "ecall",
            inlateout("a0") result,
            in("a1") arg1,
            in("a2") arg2,
            in("a3") arg3,
            in("a4") arg4,
            in("a5") arg5,
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

// AGENT: issue one-argument dup/close/exit syscalls while retaining a return
// value for ordinary fd operations and a defensive broken-exit fallback.
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

// AGENT: build the exact 32-byte asm-generic RV64 flock image without relying
// on a Rust repr(C) layout or leaving user-stack padding uninitialized.
fn flock_image(lock_type: i16, start: i64, len: i64) -> [u8; 32] {
    let mut bytes = [0u8; 32];
    bytes[0..2].copy_from_slice(&lock_type.to_le_bytes());
    bytes[2..4].copy_from_slice(&SEEK_SET.to_le_bytes());
    bytes[8..16].copy_from_slice(&start.to_le_bytes());
    bytes[16..24].copy_from_slice(&len.to_le_bytes());
    bytes
}

// AGENT: inspect only the returned l_type field needed by the same-process
// F_GETLK smoke assertion.
fn flock_type(bytes: &[u8; 32]) -> i16 {
    i16::from_le_bytes([bytes[0], bytes[1]])
}

// AGENT: prove user-mode mkdirat/openat/dup/dup3/pipe2/splice/stat/close,
// including six-argument offset copyout, exact-target survival, and OFD teardown.
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
    let duplicated = if file_written == OPEN_PAYLOAD.len() as isize {
        unsafe { syscall1(SYS_DUP, opened as usize) }
    } else {
        file_written
    };
    let source_close_result = if duplicated >= 0 {
        unsafe { syscall1(SYS_CLOSE, opened as usize) }
    } else {
        duplicated
    };
    let dup3_result = if source_close_result == 0 {
        unsafe { syscall3(SYS_DUP3, duplicated as usize, DUP3_TARGET_FD, O_CLOEXEC) }
    } else {
        source_close_result
    };
    let duplicate_close_result = if dup3_result == DUP3_TARGET_FD as isize {
        unsafe { syscall1(SYS_CLOSE, duplicated as usize) }
    } else {
        dup3_result
    };
    let mut fd_stat = [0u8; RISCV64_STAT_SIZE];
    let fstat_result = if duplicate_close_result == 0 {
        unsafe { syscall2(SYS_FSTAT, DUP3_TARGET_FD, fd_stat.as_mut_ptr() as usize) }
    } else {
        duplicate_close_result
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
    let dup_round_trip_ok = duplicated >= 0 && duplicated != opened && source_close_result == 0;
    let dup3_round_trip_ok = dup_round_trip_ok
        && dup3_result == DUP3_TARGET_FD as isize
        && duplicate_close_result == 0
        && stat_round_trip_ok;
    let dup_message_written = if dup_round_trip_ok {
        unsafe {
            syscall3(
                SYS_WRITE,
                STDOUT_FILENO,
                DUP_MESSAGE.as_ptr() as usize,
                DUP_MESSAGE.len(),
            )
        }
    } else {
        -1
    };
    let dup3_message_written = if dup3_round_trip_ok {
        unsafe {
            syscall3(
                SYS_WRITE,
                STDOUT_FILENO,
                DUP3_MESSAGE.as_ptr() as usize,
                DUP3_MESSAGE.len(),
            )
        }
    } else {
        -1
    };
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
    let fcntl_getfd_before = if stat_round_trip_ok {
        unsafe { syscall3(SYS_FCNTL, DUP3_TARGET_FD, F_GETFD, 0) }
    } else {
        -1
    };
    let fcntl_setfd = if fcntl_getfd_before == FD_CLOEXEC as isize {
        unsafe { syscall3(SYS_FCNTL, DUP3_TARGET_FD, F_SETFD, 0) }
    } else {
        fcntl_getfd_before
    };
    let fcntl_getfd_after = if fcntl_setfd == 0 {
        unsafe { syscall3(SYS_FCNTL, DUP3_TARGET_FD, F_GETFD, 0) }
    } else {
        fcntl_setfd
    };
    let fcntl_getfl_before = if fcntl_getfd_after == 0 {
        unsafe { syscall3(SYS_FCNTL, DUP3_TARGET_FD, F_GETFL, 0) }
    } else {
        fcntl_getfd_after
    };
    let fcntl_setfl = if fcntl_getfl_before == O_WRONLY as isize {
        unsafe { syscall3(SYS_FCNTL, DUP3_TARGET_FD, F_SETFL, O_APPEND) }
    } else {
        fcntl_getfl_before
    };
    let fcntl_getfl_after = if fcntl_setfl == 0 {
        unsafe { syscall3(SYS_FCNTL, DUP3_TARGET_FD, F_GETFL, 0) }
    } else {
        fcntl_setfl
    };
    let fcntl_dup = if fcntl_getfl_after == (O_WRONLY | O_APPEND) as isize {
        unsafe { syscall3(SYS_FCNTL, DUP3_TARGET_FD, F_DUPFD, 10) }
    } else {
        fcntl_getfl_after
    };
    let fcntl_cloexec_dup = if fcntl_dup >= 10 {
        unsafe { syscall3(SYS_FCNTL, DUP3_TARGET_FD, F_DUPFD_CLOEXEC, 11) }
    } else {
        fcntl_dup
    };
    let fcntl_cloexec_dup_flags = if fcntl_cloexec_dup >= 11 {
        unsafe { syscall3(SYS_FCNTL, fcntl_cloexec_dup as usize, F_GETFD, 0) }
    } else {
        fcntl_cloexec_dup
    };
    let mut record_lock = flock_image(F_WRLCK, 0, 0);
    let fcntl_setlk = if fcntl_cloexec_dup_flags == FD_CLOEXEC as isize {
        unsafe {
            syscall3(
                SYS_FCNTL,
                DUP3_TARGET_FD,
                F_SETLK,
                record_lock.as_mut_ptr() as usize,
            )
        }
    } else {
        fcntl_cloexec_dup_flags
    };
    let fcntl_getlk = if fcntl_setlk == 0 {
        unsafe {
            syscall3(
                SYS_FCNTL,
                fcntl_dup as usize,
                F_GETLK,
                record_lock.as_mut_ptr() as usize,
            )
        }
    } else {
        fcntl_setlk
    };
    let fcntl_getlk_type = flock_type(&record_lock);
    record_lock = flock_image(F_WRLCK, 0, 0);
    let fcntl_setlkw = if fcntl_getlk == 0 && fcntl_getlk_type == F_UNLCK {
        // F_GETLK wrote F_UNLCK into the previous image; rebuild the write-lock
        // request before exercising the blocking command on our own lock.
        unsafe {
            syscall3(
                SYS_FCNTL,
                DUP3_TARGET_FD,
                F_SETLKW,
                record_lock.as_mut_ptr() as usize,
            )
        }
    } else {
        -1
    };
    let mut record_unlock = flock_image(F_UNLCK, 0, 0);
    let fcntl_unlock = if fcntl_setlkw == 0 {
        unsafe {
            syscall3(
                SYS_FCNTL,
                DUP3_TARGET_FD,
                F_SETLK,
                record_unlock.as_mut_ptr() as usize,
            )
        }
    } else {
        fcntl_setlkw
    };
    let fcntl_dup_close = if fcntl_dup >= 0 {
        unsafe { syscall1(SYS_CLOSE, fcntl_dup as usize) }
    } else {
        fcntl_dup
    };
    let fcntl_cloexec_dup_close = if fcntl_cloexec_dup >= 0 {
        unsafe { syscall1(SYS_CLOSE, fcntl_cloexec_dup as usize) }
    } else {
        fcntl_cloexec_dup
    };
    let fcntl_round_trip_ok = fcntl_getfd_before == FD_CLOEXEC as isize
        && fcntl_getfd_after == 0
        && fcntl_getfl_before == O_WRONLY as isize
        && fcntl_getfl_after == (O_WRONLY | O_APPEND) as isize
        && fcntl_dup >= 10
        && fcntl_cloexec_dup > fcntl_dup
        && fcntl_cloexec_dup_flags == FD_CLOEXEC as isize
        && fcntl_setlk == 0
        && fcntl_getlk == 0
        && fcntl_setlkw == 0
        && fcntl_unlock == 0
        && fcntl_dup_close == 0
        && fcntl_cloexec_dup_close == 0;
    let fcntl_message_written = if fcntl_round_trip_ok {
        unsafe {
            syscall3(
                SYS_WRITE,
                STDOUT_FILENO,
                FCNTL_MESSAGE.as_ptr() as usize,
                FCNTL_MESSAGE.len(),
            )
        }
    } else {
        -1
    };
    let mut pipe_fds = [-1i32; 2];
    let pipe_result = if fcntl_round_trip_ok {
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
    let splice_source_fd = if pipe_round_trip_ok {
        unsafe { syscall4(SYS_OPENAT, AT_FDCWD, OPEN_PATH.as_ptr() as usize, 0, 0) }
    } else {
        -1
    };
    let mut splice_fds = [-1i32; 2];
    let splice_pipe_result = if splice_source_fd >= 0 {
        unsafe { syscall2(SYS_PIPE2, splice_fds.as_mut_ptr() as usize, 0) }
    } else {
        splice_source_fd
    };
    let mut splice_offset = 0i64;
    let splice_moved = if splice_pipe_result == 0 && splice_fds[1] >= 0 {
        unsafe {
            syscall6(
                SYS_SPLICE,
                splice_source_fd as usize,
                (&mut splice_offset as *mut i64) as usize,
                splice_fds[1] as usize,
                0,
                OPEN_PAYLOAD.len(),
                0,
            )
        }
    } else {
        splice_pipe_result
    };
    let mut splice_output = [0u8; OPEN_PAYLOAD.len()];
    let splice_read = if splice_moved == OPEN_PAYLOAD.len() as isize && splice_fds[0] >= 0 {
        unsafe {
            syscall3(
                SYS_READ,
                splice_fds[0] as usize,
                splice_output.as_mut_ptr() as usize,
                splice_output.len(),
            )
        }
    } else {
        splice_moved
    };
    let splice_read_close = if splice_pipe_result == 0 && splice_fds[0] >= 0 {
        unsafe { syscall1(SYS_CLOSE, splice_fds[0] as usize) }
    } else {
        splice_pipe_result
    };
    let splice_write_close = if splice_pipe_result == 0 && splice_fds[1] >= 0 {
        unsafe { syscall1(SYS_CLOSE, splice_fds[1] as usize) }
    } else {
        splice_pipe_result
    };
    let splice_source_close = if splice_source_fd >= 0 {
        unsafe { syscall1(SYS_CLOSE, splice_source_fd as usize) }
    } else {
        splice_source_fd
    };
    let splice_round_trip_ok = splice_source_fd >= 0
        && splice_pipe_result == 0
        && splice_moved == OPEN_PAYLOAD.len() as isize
        && splice_offset == OPEN_PAYLOAD.len() as i64
        && splice_read == OPEN_PAYLOAD.len() as isize
        && splice_output == OPEN_PAYLOAD
        && splice_read_close == 0
        && splice_write_close == 0
        && splice_source_close == 0;
    let splice_message_written = if splice_round_trip_ok {
        unsafe {
            syscall3(
                SYS_WRITE,
                STDOUT_FILENO,
                SPLICE_MESSAGE.as_ptr() as usize,
                SPLICE_MESSAGE.len(),
            )
        }
    } else {
        -1
    };
    let close_fd = if dup3_result == DUP3_TARGET_FD as isize {
        dup3_result
    } else if duplicated >= 0 {
        duplicated
    } else {
        opened
    };
    let close_result = if close_fd >= 0 {
        unsafe { syscall1(SYS_CLOSE, close_fd as usize) }
    } else {
        close_fd
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
            || !dup_round_trip_ok
            || dup_message_written != DUP_MESSAGE.len() as isize
            || !dup3_round_trip_ok
            || dup3_message_written != DUP3_MESSAGE.len() as isize
            || !stat_round_trip_ok
            || stat_message_written != STAT_MESSAGE.len() as isize
            || !fcntl_round_trip_ok
            || fcntl_message_written != FCNTL_MESSAGE.len() as isize
            || !pipe_round_trip_ok
            || pipe_message_written != PIPE_MESSAGE.len() as isize
            || !splice_round_trip_ok
            || splice_message_written != SPLICE_MESSAGE.len() as isize
            || close_result != 0
            || close_message_written != CLOSE_MESSAGE.len() as isize,
    );
    if status != 0 {
        let _ = unsafe { syscall1(SYS_EXIT, status) };
        loop {
            core::hint::spin_loop();
        }
    }

    // AGENT: leave one fixed close-on-exec alias live across execve so the new
    // image can distinguish descriptor cleanup from inherited stdout.
    let exec_cloexec = unsafe { syscall3(SYS_DUP3, STDOUT_FILENO, EXEC_CLOEXEC_FD, O_CLOEXEC) };
    if exec_cloexec != EXEC_CLOEXEC_FD as isize {
        let _ = unsafe { syscall1(SYS_EXIT, 1) };
        loop {
            core::hint::spin_loop();
        }
    }

    let argv = [EXEC_ARG0.as_ptr() as usize, 0];
    let envp = [EXEC_ENV0.as_ptr() as usize, 0];
    let result = unsafe {
        syscall3(
            SYS_EXECVE,
            EXEC_PATH.as_ptr() as usize,
            argv.as_ptr() as usize,
            envp.as_ptr() as usize,
        )
    };

    // AGENT: reaching this point means execve failed and returned to the old image.
    let _ = result;
    let _ = unsafe {
        syscall3(
            SYS_WRITE,
            STDOUT_FILENO,
            EXEC_FAILURE_MESSAGE.as_ptr() as usize,
            EXEC_FAILURE_MESSAGE.len(),
        )
    };
    let _ = unsafe { syscall1(SYS_EXIT, 1) };

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
