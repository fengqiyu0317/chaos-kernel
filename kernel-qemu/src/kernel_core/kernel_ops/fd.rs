// AGENT: join per-process fd-table mutations with Kernel-owned POSIX record-lock
// lifecycle cleanup without letting deferred fd teardown reach global state.
use super::*;
use crate::kernel::proc::task::fd::CloseEffect;

// AGENT: apply record-lock release before ordinary OFD/epoll destruction, with
// every operation occurring after the fd-table lock has already been released.
impl Kernel {
    fn apply_close_effect(&self, process_pid: usize, effect: CloseEffect) {
        if let Some(identity) = effect.file_identity() {
            self.record_locks.release_file(process_pid, identity);
        }
        effect.run();
    }

    // AGENT: make explicit close release all process locks for the closed file,
    // even when another dup or independent open remains in the fd table.
    pub fn close_task_fd(&self, task: &Task, fd: usize) -> Result<(), &'static str> {
        let effect = task.prepare_close_fd(fd)?;
        self.apply_close_effect(task.process.pid(), effect);
        Ok(())
    }

    // AGENT: make exec's FD_CLOEXEC phase share the exact explicit-close path so
    // every implicitly closed file releases that process's POSIX locks.
    pub(crate) fn close_cloexec_task_fds(&self, task: &Task, close_fds: &[usize]) {
        for &fd in close_fds {
            let _ = self.close_task_fd(task, fd);
        }
    }

    // AGENT: apply dup3 exact-target record-lock cleanup while leaving flag and
    // same-fd ABI validation at the syscall boundary.
    pub fn dup3_task_fd(
        &self,
        task: &Task,
        old_fd: usize,
        new_fd: usize,
        cloexec: bool,
    ) -> Result<usize, &'static str> {
        let (fd, effect) = task.dup3_fd_with_close_effect(old_fd, new_fd, cloexec)?;
        if let Some(effect) = effect {
            self.apply_close_effect(task.process.pid(), effect);
        }
        Ok(fd)
    }
}
