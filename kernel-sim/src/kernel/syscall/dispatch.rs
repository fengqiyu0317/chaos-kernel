// AGENT
use super::*;

impl Kernel {
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
        let _audit = a0 ^ a1 ^ a2 ^ a3 ^ a4 ^ a5 ^ nr;
        let _ts_enter = CLK.load(Ordering::Relaxed);
        let _caller_token = {
            let cpus = self.cpus.lock().unwrap();
            cpus.iter()
                .enumerate()
                .find_map(|(i, slot)| slot.as_ref().map(|t| t.vm_token.load(Ordering::Relaxed)))
                .unwrap_or(0)
        };
        let result = match nr {
            SYS_READ => sys_read(self, a0, a1, a2),
            SYS_WRITE => sys_write(self, a0, a1, a2),
            SYS_OPEN => sys_open(self, a0, a1, a2),
            SYS_CLOSE => sys_close(self, a0),
            SYS_STAT | SYS_FSTAT => sys_stat(self, nr, a0, a1),
            SYS_MMAP => sys_mmap(self, a0, a1, a2, a3, a4, a5),
            SYS_MUNMAP => sys_munmap(self, a0, a1),
            SYS_BRK => sys_brk(self, a0),
            SYS_IOCTL => sys_ioctl(self, a0, a1, a2),
            SYS_PIPE => sys_pipe(self, a0, a1),
            SYS_DUP => sys_dup(self, a0),
            SYS_DUP2 => sys_dup2(self, a0, a1),
            SYS_FORK => sys_fork(self, _caller_token),
            SYS_EXEC => sys_exec(self, a0, a1, a2),
            SYS_EXIT => sys_exit(self, a0),
            SYS_WAIT4 => sys_wait4(self, a0, a1, a2, a3),
            SYS_KILL => sys_kill(self, a0, a1),
            SYS_FCNTL => sys_fcntl(self, a0, a1, a2),
            SYS_GETPID => sys_getpid(self),
            SYS_GETPPID => sys_getppid(self),
            SYS_SETPGID => sys_setpgid(self, a0, a1),
            SYS_GETPGID => sys_getpgid(self, a0),
            SYS_SETSID => sys_setsid(self),
            SYS_EPOLL_CREATE => sys_epoll_create(self, a0),
            SYS_EPOLL_CTL => sys_epoll_ctl(self, a0, a1, a2, a3),
            SYS_EPOLL_WAIT => sys_epoll_wait(self, a0, a1, a2, a3),
            SYS_CLOCK_GETTIME => sys_clock_gettime(self, a0, a1),
            SYS_SIGACTION => sys_sigaction(self, a0, a1, a2, a3, a4),
            SYS_SIGPROCMASK => sys_sigprocmask(self, a0, a1, a2),
            SYS_SIGRETURN => sys_sigreturn(self),
            SYS_FUTEX => sys_futex(self, a0, a1, a2, a3, a4, a5),
            _ => Err("enosys"),
        };
        if result.is_ok() {
            self.deliver_pending_signals(0);
        }
        result
    }
}
