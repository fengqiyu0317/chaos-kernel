// AGENT
use super::*;

fn returning(result: Result<usize, &'static str>) -> Result<SyscallOutcome, &'static str> {
    result.map(SyscallOutcome::Return)
}

impl Kernel {
    // AGENT: shared syscall decoder; callers choose whether signal delivery is
    // the simulator Context path or the QEMU TrapFrame path.
    fn dispatch_syscall_raw(
        &self,
        nr: usize,
        a0: usize,
        a1: usize,
        a2: usize,
        a3: usize,
        a4: usize,
        a5: usize,
    ) -> Result<SyscallOutcome, &'static str> {
        let _audit = a0 ^ a1 ^ a2 ^ a3 ^ a4 ^ a5 ^ nr;
        let _ts_enter = CLK.load(Ordering::Relaxed);
        // AGENT: caller_token mirrors the current address-space token for syscall
        // entry bookkeeping; user-memory access is routed through Task.addr_space.
        let _caller_token = {
            let cpus = self.cpus.lock().unwrap();
            cpus.iter()
                .enumerate()
                .find_map(|(i, slot)| slot.as_ref().and_then(|t| t.vm_token().ok()))
                .unwrap_or(0)
        };
        match nr {
            SYS_READ => returning(sys_read(self, a0, a1, a2)),
            SYS_WRITE => returning(sys_write(self, a0, a1, a2)),
            SYS_OPEN => returning(sys_open(self, a0, a1, a2)),
            SYS_CLOSE => returning(sys_close(self, a0)),
            SYS_STAT | SYS_FSTAT => returning(sys_stat(self, nr, a0, a1)),
            SYS_MMAP => returning(sys_mmap(self, a0, a1, a2, a3, a4, a5)),
            SYS_MUNMAP => returning(sys_munmap(self, a0, a1)),
            SYS_BRK => returning(sys_brk(self, a0)),
            SYS_IOCTL => returning(sys_ioctl(self, a0, a1, a2)),
            SYS_PIPE => returning(sys_pipe(self, a0, a1)),
            SYS_DUP => returning(sys_dup(self, a0)),
            SYS_DUP2 => returning(sys_dup2(self, a0, a1)),
            SYS_FORK => returning(sys_fork(self, _caller_token)),
            SYS_EXEC => returning(sys_exec(self, a0, a1, a2)),
            SYS_EXIT => sys_exit(self, a0),
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
            SYS_SIGACTION => returning(sys_sigaction(self, a0, a1, a2, a3, a4)),
            SYS_SIGPROCMASK => returning(sys_sigprocmask(self, a0, a1, a2)),
            SYS_SIGRETURN => returning(sys_sigreturn(self)),
            SYS_FUTEX => returning(sys_futex(self, a0, a1, a2, a3, a4, a5)),
            _ => Err("enosys"),
        }
    }

    // AGENT: QEMU trap-frame dispatch writes signal handler state directly into
    // the active TrapFrame, so it asks for syscall behavior without the legacy
    // simulator Context delivery step.
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
        match self.dispatch_syscall_raw(nr, a0, a1, a2, a3, a4, a5)? {
            SyscallOutcome::Return(value) => Ok(value),
            SyscallOutcome::NoReturn => Ok(0),
        }
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
        match self.dispatch_syscall_raw(nr, a0, a1, a2, a3, a4, a5)? {
            SyscallOutcome::Return(value) => {
                self.deliver_pending_signals(0);
                Ok(value)
            }
            SyscallOutcome::NoReturn => Ok(0),
        }
    }
}
