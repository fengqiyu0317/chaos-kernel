use super::{
    CheckpointError, CheckpointHeader, CheckpointImage, RestorePolicy, SavedFdEntry, SavedFdKind,
    SavedRunState, SavedTimer, SavedTimerTargetKind, CHECKPOINT_ARCH_RISCV64, CHECKPOINT_MAGIC,
    CHECKPOINT_PAGE_SIZE, CHECKPOINT_VERSION,
};

// AGENT: enforce the M10 first-version scope before checkpoint bytes are
// emitted or accepted by restore.
pub(super) fn validate_first_version(image: &CheckpointImage) -> Result<(), CheckpointError> {
    validate_header_static(&image.header)?;
    let process = image
        .process
        .as_ref()
        .ok_or(CheckpointError::MissingProcess)?;
    if image.trap_frame.is_none() {
        return Err(CheckpointError::MissingTrapFrame);
    }
    if process.thread_count != 1 || process.restore_policy != RestorePolicy::NewPid {
        return Err(CheckpointError::UnsupportedState);
    }
    match process.run_state {
        SavedRunState::SyscallSafePoint | SavedRunState::ExplicitQuiescentPoint => {}
        SavedRunState::BlockedWait => return Err(CheckpointError::UnsupportedState),
    }
    validate_region(process.stack_base, process.stack_len)?;
    for vma in &image.vmas {
        validate_region(vma.start, vma.len)?;
    }
    for page in &image.pages {
        if !is_page_aligned(page.vaddr) {
            return Err(CheckpointError::BadAlignment);
        }
        if page.bytes.len() != image.header.page_size as usize {
            return Err(CheckpointError::BadPageLength);
        }
    }
    validate_fd_scope(&image.fds)?;
    validate_open_descriptions(&image.fds)?;
    validate_timer_scope(&image.timers)?;
    Ok(())
}

// AGENT: validate fixed header fields that are independent of section payloads.
pub(super) fn validate_header_static(header: &CheckpointHeader) -> Result<(), CheckpointError> {
    if header.magic != CHECKPOINT_MAGIC {
        return Err(CheckpointError::BadMagic);
    }
    if header.version != CHECKPOINT_VERSION {
        return Err(CheckpointError::UnsupportedVersion);
    }
    if header.arch != CHECKPOINT_ARCH_RISCV64 {
        return Err(CheckpointError::UnsupportedArch);
    }
    if header.page_size != CHECKPOINT_PAGE_SIZE {
        return Err(CheckpointError::BadPageSize);
    }
    Ok(())
}

// AGENT: reject unsupported fd-backed objects before they are placed in an image
// that restore cannot faithfully recreate.
fn validate_fd_scope(fds: &[SavedFdEntry]) -> Result<(), CheckpointError> {
    for fd in fds {
        match fd.kind {
            SavedFdKind::Stdin | SavedFdKind::Stdout | SavedFdKind::Stderr => {}
            SavedFdKind::RegularMemoryFile
            | SavedFdKind::Pipe
            | SavedFdKind::Epoll
            | SavedFdKind::Socket
            | SavedFdKind::Tty => return Err(CheckpointError::UnsupportedFd),
        }
    }
    Ok(())
}

// AGENT: enforce that duplicated fd entries sharing an open-file-description
// also share the same serializable status and offset state.
fn validate_open_descriptions(fds: &[SavedFdEntry]) -> Result<(), CheckpointError> {
    for i in 0..fds.len() {
        for j in i + 1..fds.len() {
            if fds[i].description_id == fds[j].description_id
                && (fds[i].status_flags != fds[j].status_flags
                    || fds[i].kind != fds[j].kind
                    || fds[i].object_id != fds[j].object_id
                    || fds[i].offset != fds[j].offset)
            {
                return Err(CheckpointError::InconsistentOpenDescription);
            }
        }
    }
    Ok(())
}

// AGENT: first-version timer restore supports only logical-clock timers bound to
// the saved single task; unbound wait-token timers are rejected before restore.
fn validate_timer_scope(timers: &[SavedTimer]) -> Result<(), CheckpointError> {
    for timer in timers {
        if timer.clock_id != 0 {
            return Err(CheckpointError::UnsupportedState);
        }
        if timer.target_task_id == 0 {
            return Err(CheckpointError::InvalidEnum);
        }
        match timer.target_kind {
            SavedTimerTargetKind::WakeTask => {
                if timer.signo != 0 || timer.sender_tid != 0 {
                    return Err(CheckpointError::InvalidEnum);
                }
            }
            SavedTimerTargetKind::SignalTask => {
                if timer.signo <= 0 {
                    return Err(CheckpointError::InvalidEnum);
                }
            }
        }
    }
    Ok(())
}

// AGENT: require page-granular mappings in the first version.
fn validate_region(start: u64, len: u64) -> Result<(), CheckpointError> {
    if len == 0 || !is_page_aligned(start) || !is_page_aligned(len) {
        return Err(CheckpointError::BadAlignment);
    }
    start
        .checked_add(len)
        .ok_or(CheckpointError::LengthOverflow)?;
    Ok(())
}

// AGENT: common page-alignment helper for serialized virtual addresses.
fn is_page_aligned(value: u64) -> bool {
    value % CHECKPOINT_PAGE_SIZE as u64 == 0
}
