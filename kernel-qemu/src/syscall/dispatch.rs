// AGENT
use super::*;
use crate::trap::TrapFrame;

fn returning(result: Result<usize, &'static str>) -> Result<SyscallOutcome, &'static str> {
    result.map(SyscallOutcome::Return)
}

impl Kernel {
    // AGENT: shared syscall decoder; the QEMU trap caller may additionally
    // supply its complete live frame for context-sensitive operations.
    fn dispatch_syscall_raw(
        &self,
        nr: usize,
        a0: usize,
        a1: usize,
        a2: usize,
        a3: usize,
        a4: usize,
        a5: usize,
        caller_frame: Option<&TrapFrame>,
    ) -> Result<SyscallOutcome, &'static str> {
        let _audit = a0 ^ a1 ^ a2 ^ a3 ^ a4 ^ a5 ^ nr;
        let _ts_enter = CLK.load(Ordering::Relaxed);
        match nr {
            SYS_READ => returning(sys_read(self, a0, a1, a2)),
            SYS_WRITE => returning(sys_write(self, a0, a1, a2)),
            SYS_OPEN => returning(sys_open(self, a0, a1, a2)),
            SYS_MOUNT => returning(sys_mount(self, a0, a1, a2, a3, a4)),
            SYS_UMOUNT2 => returning(sys_umount2(self, a0, a1)),
            SYS_CLOSE => returning(sys_close(self, a0)),
            SYS_STAT | SYS_FSTAT => returning(sys_stat(self, nr, a0, a1)),
            SYS_MMAP => returning(sys_mmap(self, a0, a1, a2, a3, a4, a5)),
            SYS_MUNMAP => returning(sys_munmap(self, a0, a1)),
            SYS_BRK => returning(sys_brk(self, a0)),
            SYS_IOCTL => returning(sys_ioctl(self, a0, a1, a2)),
            SYS_PIPE => returning(sys_pipe(self, a0, a1)),
            SYS_DUP => returning(sys_dup(self, a0)),
            SYS_DUP2 => returning(sys_dup2(self, a0, a1)),
            SYS_FORK => returning(sys_fork(self, caller_frame)),
            SYS_EXEC => sys_exec(self, a0, a1, a2),
            SYS_EXIT => sys_exit(self, a0),
            SYS_EXIT_GROUP => sys_exit_group(self, a0),
            SYS_WAIT4 => returning(sys_wait4(self, a0, a1, a2, a3)),
            SYS_KILL => returning(sys_kill(self, a0, a1)),
            SYS_FCNTL => returning(sys_fcntl(self, a0, a1, a2)),
            SYS_GETPID => returning(sys_getpid(self)),
            SYS_GETPPID => returning(sys_getppid(self)),
            SYS_SETPGID => returning(sys_setpgid(self, a0, a1)),
            SYS_GETPGID => returning(sys_getpgid(self, a0)),
            SYS_SETSID => returning(sys_setsid(self)),
            SYS_EPOLL_CREATE => returning(sys_epoll_create(self, a0)),
            SYS_EPOLL_CTL => returning(sys_epoll_ctl(self, a0, a1, a2, a3)),
            SYS_EPOLL_WAIT => returning(sys_epoll_wait(self, a0, a1, a2, a3)),
            SYS_CLOCK_GETTIME => returning(sys_clock_gettime(self, a0, a1)),
            SYS_SPLICE => returning(sys_splice(self, a0, a1, a2, a3, a4, a5)),
            SYS_SIGACTION => returning(sys_sigaction(self, a0, a1, a2, a3)),
            SYS_SIGPROCMASK => returning(sys_sigprocmask(self, a0, a1, a2, a3)),
            SYS_SIGRETURN => sys_sigreturn(self),
            SYS_FUTEX => returning(sys_futex(self, a0, a1, a2, a3, a4, a5)),
            _ => Err("enosys"),
        }
    }

    // AGENT: keep the task-owned-frame, no-signal-delivery adapter confined to
    // direct filesystem, process, and scheduler semantic selftests that need it.
    #[cfg(any(
        test,
        feature = "qemu-fs-selftest",
        feature = "qemu-proc-selftest",
        feature = "qemu-sched-selftest"
    ))]
    pub(crate) fn dispatch_syscall_without_signal_delivery(
        &self,
        nr: usize,
        a0: usize,
        a1: usize,
        a2: usize,
        a3: usize,
        a4: usize,
        a5: usize,
    ) -> Result<usize, &'static str> {
        match self.dispatch_syscall_raw(nr, a0, a1, a2, a3, a4, a5, None)? {
            SyscallOutcome::Return(value) => Ok(value),
            SyscallOutcome::ReplaceUserContext {
                entry,
                stack_pointer,
            } => {
                let task = self.cur_task(0).ok_or("esrch")?;
                task.install_user_trap_frame(TrapFrame::for_user_entry(entry, stack_pointer))?;
                Ok(0)
            }
            SyscallOutcome::RestoreUserContext(frame) => {
                let value = frame.regs[10];
                let task = self.cur_task(0).ok_or("esrch")?;
                task.install_user_trap_frame(frame)?;
                Ok(value)
            }
            SyscallOutcome::NoReturn => {
                unreachable!("non-returning syscalls require the trap handoff path")
            }
        }
    }

    // AGENT: expose the architecture-sensitive syscall outcome to the owner of
    // the live TrapFrame instead of installing through a second task-stack alias.
    pub(crate) fn dispatch_syscall_from_trap(
        &self,
        nr: usize,
        a0: usize,
        a1: usize,
        a2: usize,
        a3: usize,
        a4: usize,
        a5: usize,
        caller_frame: &TrapFrame,
    ) -> Result<SyscallOutcome, &'static str> {
        self.dispatch_syscall_raw(nr, a0, a1, a2, a3, a4, a5, Some(caller_frame))
    }

    pub fn dispatch_syscall(
        &self,
        nr: usize,
        a0: usize,
        a1: usize,
        a2: usize,
        a3: usize,
        a4: usize,
        a5: usize,
    ) -> Result<usize, &'static str> {
        match self.dispatch_syscall_raw(nr, a0, a1, a2, a3, a4, a5, None)? {
            SyscallOutcome::Return(value) => {
                self.deliver_pending_signals(0);
                Ok(value)
            }
            SyscallOutcome::ReplaceUserContext {
                entry,
                stack_pointer,
            } => {
                let task = self.cur_task(0).ok_or("esrch")?;
                task.install_user_trap_frame(TrapFrame::for_user_entry(entry, stack_pointer))?;
                self.deliver_pending_signals(0);
                Ok(0)
            }
            SyscallOutcome::RestoreUserContext(frame) => {
                let value = frame.regs[10];
                let task = self.cur_task(0).ok_or("esrch")?;
                task.install_user_trap_frame(frame)?;
                Ok(value)
            }
            SyscallOutcome::NoReturn => {
                unreachable!("non-returning syscalls require the trap handoff path")
            }
        }
    }
}
