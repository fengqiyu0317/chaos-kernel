use super::*;
use crate::trap::TrapFrame;

impl Kernel {
    // AGENT: assemble a first-version checkpoint image for the current CPU task
    // at a syscall-safe trap-frame boundary.
    pub fn checkpoint_current_image(
        &self,
        cpu: usize,
        trap_frame: SavedTrapFrame,
    ) -> Result<CheckpointImage, &'static str> {
        let task = self.cur_task(cpu).ok_or("esrch")?;
        let thread_count = u32::try_from(task.process.thread_count()).map_err(|_| "enotsup")?;
        if thread_count != 1 {
            return Err("enotsup");
        }

        let (brk, vmas, pages) = {
            let addr_space = task.process.addr_space.lock().unwrap();
            let (vmas, pages) = addr_space.snapshot_checkpoint_memory()?;
            (addr_space.brk(), vmas, pages)
        };

        let mut image = CheckpointImage::new_riscv64();
        image.process = Some(SavedProcess {
            brk: brk as u64,
            thread_count,
            run_state: SavedRunState::SyscallSafePoint,
        });
        image.trap_frame = Some(trap_frame);
        image.vmas = vmas;
        image.pages = pages;
        image.fds = task.snapshot_checkpoint_fds()?;
        image.timers = self.timers.lock().snapshot_checkpoint_timers(task.id())?;
        image
            .validate_first_version()
            .map_err(checkpoint_error_to_errno)?;
        Ok(image)
    }

    // AGENT: write the current process checkpoint bytes to an already-open fd;
    // the fd implementation owns offset and writability checks.
    pub fn checkpoint_current_to_fd(
        &self,
        cpu: usize,
        trap_frame: SavedTrapFrame,
        fd: usize,
    ) -> Result<usize, &'static str> {
        let task = self.cur_task(cpu).ok_or("esrch")?;
        let output = task.get_fd_entry(fd).ok_or("ebadf")?;
        let image = self.checkpoint_current_image(cpu, trap_frame)?;
        let bytes = image
            .encode_first_version()
            .map_err(checkpoint_error_to_errno)?;
        output.write(&bytes)
    }

    // AGENT: decode a checkpoint image and restore it as a fresh runnable
    // process with a new pid.
    pub fn restore_process_from_image(
        &self,
        image: CheckpointImage,
    ) -> Result<usize, &'static str> {
        image
            .validate_first_version()
            .map_err(checkpoint_error_to_errno)?;
        let process = image.process.as_ref().ok_or("einval")?;
        let trap_frame = image.trap_frame.clone().ok_or("einval")?;
        let brk = usize::try_from(process.brk).map_err(|_| "einval")?;
        let addr_space =
            AddrSpace::restore_checkpoint_memory(brk, &image.vmas, &image.pages, &self.pool)?;

        let task = self.tasks.spawn()?;
        {
            let mut current_addr_space = task.process.addr_space.lock().unwrap();
            current_addr_space.release_all_pages();
            *current_addr_space = addr_space;
        }
        task.restore_checkpoint_fds(&image.fds)?;
        self.timers
            .lock()
            .restore_checkpoint_timers(&image.timers, task.id())?;
        task.install_user_trap_frame(TrapFrame::from_saved_checkpoint_frame(&trap_frame))?;
        task.set_sched_state(TaskRunState::Runnable);
        task.reset_slice();
        let task_id = task.id();
        self.run_queue.enqueue(task_id, task.sched_policy());
        Ok(task_id)
    }

    // AGENT: restore a process from checkpoint bytes read from the current
    // process fd at its current offset.
    pub fn restore_process_from_fd(&self, cpu: usize, fd: usize) -> Result<usize, &'static str> {
        let task = self.cur_task(cpu).ok_or("esrch")?;
        let input = task.get_fd_entry(fd).ok_or("ebadf")?;
        let bytes = read_fd_to_end(&input)?;
        let image =
            CheckpointImage::decode_first_version(&bytes).map_err(checkpoint_error_to_errno)?;
        self.restore_process_from_image(image)
    }
}

// AGENT: read checkpoint payloads through the fd abstraction so future file
// implementations can own blocking and offset behavior.
fn read_fd_to_end(fd: &FdEntry) -> Result<Vec<u8>, &'static str> {
    let mut out = Vec::new();
    let mut buf = vec![0u8; PAGE_SZ];
    loop {
        let n = fd.read(&mut buf)?;
        if n == 0 {
            break;
        }
        out.extend_from_slice(&buf[..n]);
    }
    Ok(out)
}

// AGENT: translate image-format errors into the existing string errno boundary
// used by migrated kernel-qemu syscalls and helpers.
fn checkpoint_error_to_errno(err: CheckpointError) -> &'static str {
    match err {
        CheckpointError::LengthOverflow => "enomem",
        CheckpointError::UnsupportedVersion
        | CheckpointError::UnsupportedArch
        | CheckpointError::UnsupportedFd
        | CheckpointError::UnsupportedState => "enotsup",
        CheckpointError::BadMagic
        | CheckpointError::BadPageSize
        | CheckpointError::Truncated
        | CheckpointError::InvalidEnum
        | CheckpointError::BadSection
        | CheckpointError::DuplicateSection
        | CheckpointError::MissingProcess
        | CheckpointError::MissingTrapFrame
        | CheckpointError::BadAlignment
        | CheckpointError::BadPageLength
        | CheckpointError::InconsistentOpenDescription => "einval",
    }
}
