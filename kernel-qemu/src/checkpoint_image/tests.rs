use alloc::vec;

use super::*;
use crate::kernel::VM_GROWSDOWN;

// AGENT: build the smallest valid first-version checkpoint image used by
// the local round-trip and validation tests.
fn sample_image() -> CheckpointImage {
    let mut regs = [0u64; 32];
    regs[2] = 0x8000_0000;
    regs[10] = 0;

    let mut image = CheckpointImage::new_riscv64();
    image.process = Some(SavedProcess {
        brk: 0x4000_0000,
        thread_count: 1,
        run_state: SavedRunState::SyscallSafePoint,
    });
    image.trap_frame = Some(SavedTrapFrame {
        regs,
        sstatus: 0x20,
        sepc: 0x1000_0004,
    });
    image.vmas.push(SavedVma {
        start: 0x1000_0000,
        len: CHECKPOINT_PAGE_SIZE as u64,
        flags: 0b111 | VM_GROWSDOWN,
    });
    image.pages.push(SavedPage {
        vaddr: 0x1000_0000,
        bytes: vec![0xaa; CHECKPOINT_PAGE_SIZE as usize],
    });
    image.fds.push(SavedFdEntry {
        fd: 1,
        description_id: 1,
        cloexec: false,
        status_flags: 1,
        kind: SavedFdKind::Stdout,
        offset: 0,
    });
    image.timers.push(SavedTimer {
        clock_id: 0,
        target_kind: SavedTimerTargetKind::SignalTask,
        signo: 14,
        sender_tid: -1,
        remaining_ticks: 21,
        interval_ticks: 3,
    });
    image
}

// AGENT: ensure the explicit binary format can carry the first supported
// register, VMA, page, fd, timer, and process metadata sections.
#[test]
fn checkpoint_image_round_trips_supported_state() {
    let image = sample_image();
    let bytes = image.encode_first_version().unwrap();
    assert!(bytes.len() > IMAGE_HEADER_LEN + SECTION_HEADER_LEN);
    let decoded = CheckpointImage::decode_first_version(&bytes).unwrap();
    let mut expected = image;
    expected.header.section_count = 6;
    assert_eq!(decoded, expected);
}

// AGENT: first-version restore must reject wait-state snapshots.
#[test]
fn validation_rejects_blocked_wait_state() {
    let mut image = sample_image();
    image.process.as_mut().unwrap().run_state = SavedRunState::BlockedWait;
    assert_eq!(
        image.validate_first_version(),
        Err(CheckpointError::UnsupportedState)
    );
}

// AGENT: first-version restore only accepts full page payloads at page
// aligned virtual addresses.
#[test]
fn validation_rejects_short_page_payload() {
    let mut image = sample_image();
    image.pages[0].bytes.pop();
    assert_eq!(
        image.validate_first_version(),
        Err(CheckpointError::BadPageLength)
    );
}

// AGENT: unsupported fd-backed kernel objects should fail before image
// serialization.
#[test]
fn validation_rejects_unsupported_fd_kind() {
    let mut image = sample_image();
    image.fds[0].kind = SavedFdKind::Epoll;
    assert_eq!(
        image.validate_first_version(),
        Err(CheckpointError::UnsupportedFd)
    );
}

// AGENT: typed stdio terminals are nonseekable, so first-version images must
// reject offsets left by the old path-tagged regular-file representation.
#[test]
fn validation_rejects_stdio_offset() {
    let mut image = sample_image();
    image.fds[0].offset = 1;
    assert_eq!(
        image.validate_first_version(),
        Err(CheckpointError::UnsupportedFd)
    );
}

// AGENT: duplicate descriptors sharing one open-file-description must agree
// on serializable offset and status state.
#[test]
fn validation_rejects_inconsistent_open_description() {
    let mut image = sample_image();
    image.fds.push(SavedFdEntry {
        fd: 2,
        description_id: 1,
        cloexec: false,
        status_flags: 0,
        kind: SavedFdKind::Stdout,
        offset: 0,
    });
    assert_eq!(
        image.validate_first_version(),
        Err(CheckpointError::InconsistentOpenDescription)
    );
}
