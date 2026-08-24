use alloc::vec::Vec;
use core::convert::TryFrom;

use super::codec::{checked_u32_to_usize, checked_u64_to_usize, Cursor};
use super::validate::{validate_current_version, validate_header_static};
use super::{
    CheckpointError, CheckpointHeader, CheckpointImage, SavedFdEntry, SavedFdKind, SavedPage,
    SavedProcess, SavedRunState, SavedTimer, SavedTimerTargetKind, SavedTrapFrame, SavedVma,
    SectionTag, IMAGE_HEADER_LEN,
};

// AGENT: decode bytes and immediately apply the current-version validation
// contract expected by restore.
pub(super) fn decode_current_version(bytes: &[u8]) -> Result<CheckpointImage, CheckpointError> {
    let image = decode_image(bytes)?;
    validate_current_version(&image)?;
    Ok(image)
}

// AGENT: parse a complete image while leaving current-version policy checks to
// CheckpointImage::validate_current_version.
fn decode_image(bytes: &[u8]) -> Result<CheckpointImage, CheckpointError> {
    let mut cursor = Cursor::new(bytes);
    let header = decode_header(&mut cursor)?;
    validate_header_static(&header)?;
    let mut image = CheckpointImage {
        header,
        process: None,
        trap_frame: None,
        vmas: Vec::new(),
        pages: Vec::new(),
        fds: Vec::new(),
        timers: Vec::new(),
    };

    let mut seen_sections = [false; 7];
    for _ in 0..image.header.section_count {
        let tag = SectionTag::try_from(cursor.read_u16()?)?;
        let tag_index = tag as usize;
        if seen_sections[tag_index] {
            return Err(CheckpointError::DuplicateSection);
        }
        seen_sections[tag_index] = true;
        let _section_flags = cursor.read_u16()?;
        let _reserved = cursor.read_u32()?;
        let len = cursor.read_u64()?;
        let payload = cursor.read_bytes(checked_u64_to_usize(len)?)?;
        match tag {
            SectionTag::Process => {
                image.process = Some(decode_process(payload)?);
            }
            SectionTag::TrapFrame => {
                image.trap_frame = Some(decode_trap_frame(payload)?);
            }
            SectionTag::Vmas => {
                image.vmas = decode_vmas(payload)?;
            }
            SectionTag::Pages => {
                image.pages = decode_pages(payload)?;
            }
            SectionTag::Fds => {
                image.fds = decode_fds(payload)?;
            }
            SectionTag::Timers => {
                image.timers = decode_timers(payload)?;
            }
        }
    }
    if cursor.remaining() != 0 {
        return Err(CheckpointError::BadSection);
    }
    Ok(image)
}

// AGENT: decode the fixed image header.
fn decode_header(cursor: &mut Cursor<'_>) -> Result<CheckpointHeader, CheckpointError> {
    let magic = cursor.read_array::<8>()?;
    let version = cursor.read_u16()?;
    let arch = cursor.read_u16()?;
    let page_size = cursor.read_u32()?;
    let flags = cursor.read_u64()?;
    let section_count = cursor.read_u32()?;
    let _reserved = cursor.read_u32()?;
    if flags != 0 {
        return Err(CheckpointError::UnsupportedState);
    }
    debug_assert_eq!(IMAGE_HEADER_LEN, cursor.pos);
    Ok(CheckpointHeader {
        magic,
        version,
        arch,
        page_size,
        section_count,
    })
}

// AGENT: parse process metadata.
fn decode_process(bytes: &[u8]) -> Result<SavedProcess, CheckpointError> {
    let mut cursor = Cursor::new(bytes);
    let process = SavedProcess {
        start_brk: cursor.read_u64()?,
        brk: cursor.read_u64()?,
        thread_count: cursor.read_u32()?,
        run_state: SavedRunState::try_from(cursor.read_u16()?)?,
    };
    cursor.expect_end()?;
    Ok(process)
}

// AGENT: parse the saved trap frame.
fn decode_trap_frame(bytes: &[u8]) -> Result<SavedTrapFrame, CheckpointError> {
    let mut cursor = Cursor::new(bytes);
    let mut regs = [0u64; 32];
    for reg in &mut regs {
        *reg = cursor.read_u64()?;
    }
    let frame = SavedTrapFrame {
        regs,
        sstatus: cursor.read_u64()?,
        sepc: cursor.read_u64()?,
    };
    cursor.expect_end()?;
    Ok(frame)
}

// AGENT: parse all VMA entries.
fn decode_vmas(bytes: &[u8]) -> Result<Vec<SavedVma>, CheckpointError> {
    let mut cursor = Cursor::new(bytes);
    let count = checked_u32_to_usize(cursor.read_u32()?)?;
    let mut vmas = Vec::new();
    for _ in 0..count {
        vmas.push(SavedVma {
            start: cursor.read_u64()?,
            len: cursor.read_u64()?,
            flags: cursor.read_u32()?,
        });
    }
    cursor.expect_end()?;
    Ok(vmas)
}

// AGENT: parse resident page payloads.
fn decode_pages(bytes: &[u8]) -> Result<Vec<SavedPage>, CheckpointError> {
    let mut cursor = Cursor::new(bytes);
    let count = checked_u32_to_usize(cursor.read_u32()?)?;
    let mut pages = Vec::new();
    for _ in 0..count {
        let vaddr = cursor.read_u64()?;
        let len = checked_u32_to_usize(cursor.read_u32()?)?;
        pages.push(SavedPage {
            vaddr,
            bytes: cursor.read_bytes(len)?.to_vec(),
        });
    }
    cursor.expect_end()?;
    Ok(pages)
}

// AGENT: parse fd table entries.
fn decode_fds(bytes: &[u8]) -> Result<Vec<SavedFdEntry>, CheckpointError> {
    let mut cursor = Cursor::new(bytes);
    let count = checked_u32_to_usize(cursor.read_u32()?)?;
    let mut fds = Vec::new();
    for _ in 0..count {
        let fd = cursor.read_u32()?;
        let description_id = cursor.read_u32()?;
        let cloexec = match cursor.read_u8()? {
            0 => false,
            1 => true,
            _ => return Err(CheckpointError::InvalidEnum),
        };
        cursor.skip(3)?;
        let status_flags = cursor.read_u32()?;
        let kind = SavedFdKind::try_from(cursor.read_u16()?)?;
        let _reserved = cursor.read_u16()?;
        fds.push(SavedFdEntry {
            fd,
            description_id,
            cloexec,
            status_flags,
            kind,
            offset: cursor.read_u64()?,
        });
    }
    cursor.expect_end()?;
    Ok(fds)
}

// AGENT: parse restartable timer metadata.
fn decode_timers(bytes: &[u8]) -> Result<Vec<SavedTimer>, CheckpointError> {
    let mut cursor = Cursor::new(bytes);
    let count = checked_u32_to_usize(cursor.read_u32()?)?;
    let mut timers = Vec::new();
    for _ in 0..count {
        let clock_id = cursor.read_u32()?;
        let target_kind = SavedTimerTargetKind::try_from(cursor.read_u16()?)?;
        let _reserved = cursor.read_u16()?;
        let signo = cursor.read_u32()? as i32;
        let _reserved = cursor.read_u32()?;
        let sender_tid = cursor.read_u64()? as i64;
        timers.push(SavedTimer {
            clock_id,
            target_kind,
            signo,
            sender_tid,
            remaining_ticks: cursor.read_u64()?,
            interval_ticks: cursor.read_u64()?,
        });
    }
    cursor.expect_end()?;
    Ok(timers)
}
