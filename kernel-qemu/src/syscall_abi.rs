#![allow(dead_code)]

use crate::trap::TrapFrame;

pub const ENOSYS_RET: usize = (-38isize) as usize;

// AGENT: Linux asm-generic syscall numbers used by the RISC-V ABI.
pub const RISCV_SYS_DUP: usize = 23;
pub const RISCV_SYS_DUP3: usize = 24;
pub const RISCV_SYS_MKDIRAT: usize = 34;
pub const RISCV_SYS_UMOUNT2: usize = 39;
pub const RISCV_SYS_MOUNT: usize = 40;
pub const RISCV_SYS_OPENAT: usize = 56;
pub const RISCV_SYS_CLOSE: usize = 57;
pub const RISCV_SYS_PIPE2: usize = 59;
pub const RISCV_SYS_NEWFSTATAT: usize = 79;
pub const RISCV_SYS_FSTAT: usize = 80;
pub const RISCV_SYS_READ: usize = 63;
pub const RISCV_SYS_WRITE: usize = 64;
pub const RISCV_SYS_KILL: usize = 129;
pub const RISCV_SYS_RT_SIGACTION: usize = 134;
pub const RISCV_SYS_RT_SIGPROCMASK: usize = 135;
pub const RISCV_SYS_RT_SIGRETURN: usize = 139;
pub const RISCV_SYS_BRK: usize = 214;
pub const RISCV_SYS_EXIT: usize = 93;
pub const RISCV_SYS_EXIT_GROUP: usize = 94;
pub const RISCV_SYS_GETPID: usize = 172;

pub const INTERNAL_SYS_READ: usize = 0;
pub const INTERNAL_SYS_WRITE: usize = 1;
pub const INTERNAL_SYS_CLOSE: usize = 3;
pub const INTERNAL_SYS_FSTAT: usize = 5;
pub const INTERNAL_SYS_BRK: usize = 12;
pub const INTERNAL_SYS_PIPE: usize = 22;
pub const INTERNAL_SYS_DUP: usize = 32;
pub const INTERNAL_SYS_DUP3: usize = 292;
pub const INTERNAL_SYS_EXIT: usize = 60;
pub const INTERNAL_SYS_EXIT_GROUP: usize = 231;
pub const INTERNAL_SYS_GETPID: usize = 39;
pub const INTERNAL_SYS_KILL: usize = 62;
pub const INTERNAL_SYS_MKDIRAT: usize = 258;
pub const INTERNAL_SYS_MOUNT: usize = 165;
pub const INTERNAL_SYS_RT_SIGACTION: usize = 13;
pub const INTERNAL_SYS_RT_SIGPROCMASK: usize = 14;
pub const INTERNAL_SYS_RT_SIGRETURN: usize = 15;
pub const INTERNAL_SYS_UMOUNT2: usize = 166;
pub const INTERNAL_SYS_OPENAT: usize = 257;
pub const INTERNAL_SYS_NEWFSTATAT: usize = 262;

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
        RISCV_SYS_DUP => Some(INTERNAL_SYS_DUP),
        RISCV_SYS_DUP3 => Some(INTERNAL_SYS_DUP3),
        RISCV_SYS_MKDIRAT => Some(INTERNAL_SYS_MKDIRAT),
        RISCV_SYS_UMOUNT2 => Some(INTERNAL_SYS_UMOUNT2),
        RISCV_SYS_MOUNT => Some(INTERNAL_SYS_MOUNT),
        RISCV_SYS_OPENAT => Some(INTERNAL_SYS_OPENAT),
        RISCV_SYS_CLOSE => Some(INTERNAL_SYS_CLOSE),
        RISCV_SYS_PIPE2 => Some(INTERNAL_SYS_PIPE),
        RISCV_SYS_NEWFSTATAT => Some(INTERNAL_SYS_NEWFSTATAT),
        RISCV_SYS_FSTAT => Some(INTERNAL_SYS_FSTAT),
        RISCV_SYS_READ => Some(INTERNAL_SYS_READ),
        RISCV_SYS_WRITE => Some(INTERNAL_SYS_WRITE),
        RISCV_SYS_KILL => Some(INTERNAL_SYS_KILL),
        RISCV_SYS_RT_SIGACTION => Some(INTERNAL_SYS_RT_SIGACTION),
        RISCV_SYS_RT_SIGPROCMASK => Some(INTERNAL_SYS_RT_SIGPROCMASK),
        RISCV_SYS_RT_SIGRETURN => Some(INTERNAL_SYS_RT_SIGRETURN),
        RISCV_SYS_BRK => Some(INTERNAL_SYS_BRK),
        RISCV_SYS_EXIT => Some(INTERNAL_SYS_EXIT),
        RISCV_SYS_EXIT_GROUP => Some(INTERNAL_SYS_EXIT_GROUP),
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
    match crate::kernel::global_kernel() {
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
    let outcome = match kernel.dispatch_syscall_from_trap(nr, a0, a1, a2, a3, a4, a5, frame) {
        Ok(outcome) => outcome,
        Err(err) => {
            write_return(frame, errno_ret(err));
            deliver_pending_signal(kernel, frame);
            return;
        }
    };
    match outcome {
        crate::kernel::SyscallOutcome::Return(value) => {
            write_return(frame, value);
            deliver_pending_signal(kernel, frame);
        }
        crate::kernel::SyscallOutcome::ReplaceUserContext {
            entry,
            stack_pointer,
        } => {
            frame.prepare_user_entry(entry, stack_pointer);
            deliver_pending_signal(kernel, frame);
        }
        crate::kernel::SyscallOutcome::RestoreUserContext(restored) => {
            *frame = restored;
        }
        crate::kernel::SyscallOutcome::NoReturn => {
            // AGENT: every non-returning syscall abandons the exited task's trap
            // stack through idle; the outcome, not one hard-coded nr, owns flow.
            kernel.switch_current_to_idle(0);
            unreachable!("a no-return syscall task was scheduled again");
        }
    }
}

// AGENT: deliver one pending handler from the complete post-syscall frame and
// replace the live stack slot only when signal state changes its continuation.
fn deliver_pending_signal(kernel: &crate::kernel::Kernel, frame: &mut TrapFrame) {
    if let Some(next) = kernel.deliver_pending_signals_from_frame(0, frame.clone()) {
        *frame = next;
    }
    // AGENT: a default-terminating signal uses the same task -> idle handoff as
    // SYS_EXIT instead of returning a zombie frame to user mode.
    let current_done = kernel.cur_task(0).is_some_and(|task| task.done());
    if current_done {
        kernel.switch_current_to_idle(0);
        unreachable!("a signal-terminated task was scheduled again");
    }
}

// AGENT: translate migrated kernel-sim errno names and intentional internal
// aliases into exact Linux asm-generic negative RISC-V syscall return values.
fn errno_ret(err: &'static str) -> usize {
    let errno = match err {
        "eperm" => 1,
        "enoent" => 2,
        "esrch" => 3,
        "eintr" => 4,
        "eio" => 5,
        "e2big" => 7,
        "enoexec" => 8,
        "ebadf" => 9,
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
        "enotty" => 25,
        "efbig" => 27,
        "enospc" => 28,
        "espipe" => 29,
        "epipe" => 32,
        "enametoolong" => 36,
        "enosys" => 38,
        "removed" => 43,
        "eoverflow" => 75,
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

// AGENT: expose focused errno-encoding regressions to Rust tests and the QEMU
// filesystem selftest without widening the private ABI conversion interface.
#[cfg(any(test, feature = "qemu-fs-selftest"))]
pub mod tests {
    use super::*;

    // AGENT: run every focused RISC-V ABI mapping regression, including dup and dup3.
    pub fn run_all() {
        standard_errno_names_use_linux_riscv_numbers();
        mkdirat_maps_to_the_three_argument_internal_entry();
        openat_maps_to_the_four_argument_internal_entry();
        dup_maps_to_the_one_argument_internal_entry();
        dup3_maps_to_the_three_argument_internal_entry();
        close_maps_to_the_one_argument_internal_entry();
        pipe2_maps_to_the_two_argument_internal_entry();
        stat_syscalls_map_to_their_distinct_internal_entries();
        signal_syscalls_map_to_migrated_semantics();
    }

    // AGENT: keep every audited file/exec errno distinguishable from EINVAL at
    // the final userspace-visible syscall ABI boundary.
    #[cfg_attr(test, test)]
    fn standard_errno_names_use_linux_riscv_numbers() {
        for (name, errno) in [
            ("enoexec", 8),
            ("ebadf", 9),
            ("enotty", 25),
            ("efbig", 27),
            ("espipe", 29),
            ("epipe", 32),
            ("eoverflow", 75),
        ] {
            assert_eq!(errno_ret(name), (-(errno as isize)) as usize);
        }
    }

    // AGENT: preserve mkdirat's dirfd/path/mode layout while translating the
    // Linux asm-generic RISC-V number into the internal syscall namespace.
    #[cfg_attr(test, test)]
    fn mkdirat_maps_to_the_three_argument_internal_entry() {
        let mut frame = TrapFrame::new();
        frame.regs[10..16].copy_from_slice(&[usize::MAX - 99, 0x3000, 0o750, 4, 5, 6]);
        frame.regs[17] = RISCV_SYS_MKDIRAT;

        let request = decode_from_trap_frame(&frame);
        assert_eq!(request.internal_nr, Some(INTERNAL_SYS_MKDIRAT));
        assert_eq!(request.args, [usize::MAX - 99, 0x3000, 0o750, 4, 5, 6]);
    }

    // AGENT: preserve openat's dirfd/path/flags/mode argument layout while the
    // RISC-V ABI adapter changes only the syscall number namespace.
    #[cfg_attr(test, test)]
    fn openat_maps_to_the_four_argument_internal_entry() {
        let mut frame = TrapFrame::new();
        frame.regs[10..16].copy_from_slice(&[usize::MAX - 99, 0x4000, 0x81, 0o640, 5, 6]);
        frame.regs[17] = RISCV_SYS_OPENAT;

        let request = decode_from_trap_frame(&frame);
        assert_eq!(request.internal_nr, Some(INTERNAL_SYS_OPENAT));
        assert_eq!(request.args, [usize::MAX - 99, 0x4000, 0x81, 0o640, 5, 6]);
    }

    // AGENT: preserve dup's oldfd argument while translating Linux RV64
    // syscall 23 into the migrated internal dup semantic id.
    #[cfg_attr(test, test)]
    fn dup_maps_to_the_one_argument_internal_entry() {
        let mut frame = TrapFrame::new();
        frame.regs[10..16].copy_from_slice(&[7, 1, 2, 3, 4, 5]);
        frame.regs[17] = RISCV_SYS_DUP;

        let request = decode_from_trap_frame(&frame);
        assert_eq!(request.internal_nr, Some(INTERNAL_SYS_DUP));
        assert_eq!(request.args, [7, 1, 2, 3, 4, 5]);
    }

    // AGENT: preserve dup3's oldfd/newfd/flags layout while translating Linux
    // RV64 syscall 24 into the distinct migrated dup3 semantic id.
    #[cfg_attr(test, test)]
    fn dup3_maps_to_the_three_argument_internal_entry() {
        let mut frame = TrapFrame::new();
        frame.regs[10..16].copy_from_slice(&[7, 100, 0o2000000, 3, 4, 5]);
        frame.regs[17] = RISCV_SYS_DUP3;

        let request = decode_from_trap_frame(&frame);
        assert_eq!(request.internal_nr, Some(INTERNAL_SYS_DUP3));
        assert_eq!(request.args, [7, 100, 0o2000000, 3, 4, 5]);
    }

    // AGENT: preserve close's single fd argument while translating Linux RISC-V
    // syscall 57 into the internal close syscall id.
    #[cfg_attr(test, test)]
    fn close_maps_to_the_one_argument_internal_entry() {
        let mut frame = TrapFrame::new();
        frame.regs[10..16].copy_from_slice(&[7, 1, 2, 3, 4, 5]);
        frame.regs[17] = RISCV_SYS_CLOSE;

        let request = decode_from_trap_frame(&frame);
        assert_eq!(request.internal_nr, Some(INTERNAL_SYS_CLOSE));
        assert_eq!(request.args, [7, 1, 2, 3, 4, 5]);
    }

    // AGENT: map Linux RV64 pipe2 number 59 onto the migrated pipe semantic id
    // while preserving the user int[2] pointer and creation flags.
    #[cfg_attr(test, test)]
    fn pipe2_maps_to_the_two_argument_internal_entry() {
        let mut frame = TrapFrame::new();
        frame.regs[10..16].copy_from_slice(&[0x5000, 0o2004000, 2, 3, 4, 5]);
        frame.regs[17] = RISCV_SYS_PIPE2;

        let request = decode_from_trap_frame(&frame);
        assert_eq!(request.internal_nr, Some(INTERNAL_SYS_PIPE));
        assert_eq!(request.args, [0x5000, 0o2004000, 2, 3, 4, 5]);
    }

    // AGENT: preserve newfstatat's four arguments and fstat's two arguments while
    // translating both Linux RV64 numbers into distinct semantic entries.
    #[cfg_attr(test, test)]
    fn stat_syscalls_map_to_their_distinct_internal_entries() {
        let mut frame = TrapFrame::new();
        frame.regs[10..16].copy_from_slice(&[usize::MAX - 99, 0x5000, 0x6000, 0, 5, 6]);
        frame.regs[17] = RISCV_SYS_NEWFSTATAT;
        let request = decode_from_trap_frame(&frame);
        assert_eq!(request.internal_nr, Some(INTERNAL_SYS_NEWFSTATAT));
        assert_eq!(request.args, [usize::MAX - 99, 0x5000, 0x6000, 0, 5, 6]);

        frame.regs[10..16].copy_from_slice(&[7, 0x6000, 2, 3, 4, 5]);
        frame.regs[17] = RISCV_SYS_FSTAT;
        let request = decode_from_trap_frame(&frame);
        assert_eq!(request.internal_nr, Some(INTERNAL_SYS_FSTAT));
        assert_eq!(request.args, [7, 0x6000, 2, 3, 4, 5]);
    }

    // AGENT: pin the four asm-generic RISC-V signal syscall numbers to the
    // existing migrated signal-semantic entries.
    #[cfg_attr(test, test)]
    fn signal_syscalls_map_to_migrated_semantics() {
        assert_eq!(map_riscv_nr(RISCV_SYS_KILL), Some(INTERNAL_SYS_KILL));
        assert_eq!(
            map_riscv_nr(RISCV_SYS_RT_SIGACTION),
            Some(INTERNAL_SYS_RT_SIGACTION)
        );
        assert_eq!(
            map_riscv_nr(RISCV_SYS_RT_SIGPROCMASK),
            Some(INTERNAL_SYS_RT_SIGPROCMASK)
        );
        assert_eq!(
            map_riscv_nr(RISCV_SYS_RT_SIGRETURN),
            Some(INTERNAL_SYS_RT_SIGRETURN)
        );
    }
}
