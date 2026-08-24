use super::{
    CheckpointError, CheckpointHeader, CheckpointImage, SavedFdEntry, SavedFdKind, SavedProcess,
    SavedRunState, SavedTimer, SavedTimerTargetKind, SavedVma, CHECKPOINT_ARCH_RISCV64,
    CHECKPOINT_MAGIC, CHECKPOINT_PAGE_SIZE, CHECKPOINT_VERSION,
};
use crate::kernel::{USER_TOP, VM_GROWSDOWN, VM_HEAP};

// AGENT: enforce the current M10 format scope and exact break-bound ordering
// before checkpoint bytes are emitted or accepted by restore.
pub(super) fn validate_current_version(image: &CheckpointImage) -> Result<(), CheckpointError> {
    validate_header_static(&image.header)?;
    let process = image
        .process
        .as_ref()
        .ok_or(CheckpointError::MissingProcess)?;
    if image.trap_frame.is_none() {
        return Err(CheckpointError::MissingTrapFrame);
    }
    if process.thread_count != 1 {
        return Err(CheckpointError::UnsupportedState);
    }
    if process.start_brk > process.brk || process.brk > USER_TOP as u64 {
        return Err(CheckpointError::UnsupportedState);
    }
    match process.run_state {
        SavedRunState::SyscallSafePoint | SavedRunState::ExplicitQuiescentPoint => {}
        SavedRunState::BlockedWait => return Err(CheckpointError::UnsupportedState),
    }
    validate_stack_vma(&image.vmas)?;
    for vma in &image.vmas {
        validate_region(vma.start, vma.len)?;
    }
    validate_heap_vmas(process, &image.vmas)?;
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

// AGENT: accept holes or non-heap replacements below current brk, but require
// every serialized VM_HEAP fragment to stay inside the exact saved heap extent.
fn validate_heap_vmas(process: &SavedProcess, vmas: &[SavedVma]) -> Result<(), CheckpointError> {
    let heap_start = page_align(process.start_brk).ok_or(CheckpointError::LengthOverflow)?;
    let heap_end = page_align(process.brk).ok_or(CheckpointError::LengthOverflow)?;
    for vma in vmas.iter().filter(|vma| vma.flags & VM_HEAP != 0) {
        let end = vma
            .start
            .checked_add(vma.len)
            .ok_or(CheckpointError::LengthOverflow)?;
        if vma.start < heap_start || end > heap_end {
            return Err(CheckpointError::UnsupportedState);
        }
    }
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
            // AGENT: current-version stdio is restored as a nonseekable typed
            // terminal, so path-tagged legacy offsets cannot be represented.
            SavedFdKind::Stdin | SavedFdKind::Stdout | SavedFdKind::Stderr => {
                if fd.offset != 0 {
                    return Err(CheckpointError::UnsupportedFd);
                }
            }
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
                    || fds[i].offset != fds[j].offset)
            {
                return Err(CheckpointError::InconsistentOpenDescription);
            }
        }
    }
    Ok(())
}

// AGENT: current-version timer restore supports only logical-clock timers bound to
// the saved single task; unbound wait-token timers are rejected before restore.
fn validate_timer_scope(timers: &[SavedTimer]) -> Result<(), CheckpointError> {
    for timer in timers {
        if timer.clock_id != 0 {
            return Err(CheckpointError::UnsupportedState);
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

// AGENT: stack identity is already part of the VMA list; require a grow-down
// VMA without duplicating its base and length in SavedProcess.
fn validate_stack_vma(vmas: &[SavedVma]) -> Result<(), CheckpointError> {
    if vmas.iter().any(|vma| vma.flags & VM_GROWSDOWN != 0) {
        Ok(())
    } else {
        Err(CheckpointError::UnsupportedState)
    }
}

// AGENT: require page-granular mappings in the current version.
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

// AGENT: derive page-granular heap ownership bounds from exact saved breaks
// while rejecting u64 overflow before restore converts into usize.
fn page_align(value: u64) -> Option<u64> {
    let mask = CHECKPOINT_PAGE_SIZE as u64 - 1;
    value.checked_add(mask).map(|rounded| rounded & !mask)
}
