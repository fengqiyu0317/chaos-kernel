use alloc::vec::Vec;

use super::codec::{checked_usize_to_u32, checked_usize_to_u64, put_u16, put_u32, put_u64};
use super::{
    CheckpointError, CheckpointHeader, CheckpointImage, SavedFdEntry, SavedPage, SavedProcess,
    SavedTimer, SavedTrapFrame, SavedVma, SectionTag, IMAGE_HEADER_LEN, SECTION_HEADER_LEN,
};

// AGENT: serialize the validated first-version image into little-endian
// sectioned bytes suitable for storage in a guest file or memory buffer.
pub(super) fn encode_first_version(image: &CheckpointImage) -> Result<Vec<u8>, CheckpointError> {
    image.validate_first_version()?;
    let sections = encoded_sections(image)?;
    let mut header = image.header.clone();
    header.section_count = checked_usize_to_u32(sections.len())?;

    let mut out = Vec::new();
    encode_header(&header, &mut out);
    for (tag, payload) in sections {
        encode_section_header(tag, payload.len(), &mut out)?;
        out.extend_from_slice(&payload);
    }
    Ok(out)
}

// AGENT: build section payloads in a stable order so diffs and tests are
// deterministic.
fn encoded_sections(
    image: &CheckpointImage,
) -> Result<Vec<(SectionTag, Vec<u8>)>, CheckpointError> {
    let mut sections = Vec::new();
    sections.push((
        SectionTag::Process,
        encode_process(
            image
                .process
                .as_ref()
                .ok_or(CheckpointError::MissingProcess)?,
        ),
    ));
    sections.push((
        SectionTag::TrapFrame,
        encode_trap_frame(
            image
                .trap_frame
                .as_ref()
                .ok_or(CheckpointError::MissingTrapFrame)?,
        ),
    ));
    sections.push((SectionTag::Vmas, encode_vmas(&image.vmas)?));
    sections.push((SectionTag::Pages, encode_pages(&image.pages)?));
    sections.push((SectionTag::Fds, encode_fds(&image.fds)?));
    sections.push((SectionTag::Timers, encode_timers(&image.timers)?));
    Ok(sections)
}

// AGENT: encode the fixed image header.
pub(super) fn encode_header(header: &CheckpointHeader, out: &mut Vec<u8>) {
    out.extend_from_slice(&header.magic);
    put_u16(out, header.version);
    put_u16(out, header.arch);
    put_u32(out, header.page_size);
    put_u64(out, 0);
    put_u32(out, header.section_count);
    put_u32(out, 0);
    debug_assert_eq!(IMAGE_HEADER_LEN, out.len());
}

// AGENT: encode one section header before its payload.
fn encode_section_header(
    tag: SectionTag,
    len: usize,
    out: &mut Vec<u8>,
) -> Result<(), CheckpointError> {
    let before = out.len();
    put_u16(out, tag as u16);
    put_u16(out, 0);
    put_u32(out, 0);
    put_u64(out, checked_usize_to_u64(len)?);
    debug_assert_eq!(SECTION_HEADER_LEN, out.len() - before);
    Ok(())
}

// AGENT: serialize process metadata.
fn encode_process(process: &SavedProcess) -> Vec<u8> {
    let mut out = Vec::new();
    put_u64(&mut out, process.brk);
    put_u32(&mut out, process.thread_count);
    put_u16(&mut out, process.run_state as u16);
    out
}

// AGENT: serialize the saved trap frame.
fn encode_trap_frame(frame: &SavedTrapFrame) -> Vec<u8> {
    let mut out = Vec::new();
    for reg in frame.regs {
        put_u64(&mut out, reg);
    }
    put_u64(&mut out, frame.sstatus);
    put_u64(&mut out, frame.sepc);
    out
}

// AGENT: serialize all VMA entries.
fn encode_vmas(vmas: &[SavedVma]) -> Result<Vec<u8>, CheckpointError> {
    let mut out = Vec::new();
    put_u32(&mut out, checked_usize_to_u32(vmas.len())?);
    for vma in vmas {
        put_u64(&mut out, vma.start);
        put_u64(&mut out, vma.len);
        put_u32(&mut out, vma.flags);
    }
    Ok(out)
}

// AGENT: serialize resident page payloads.
fn encode_pages(pages: &[SavedPage]) -> Result<Vec<u8>, CheckpointError> {
    let mut out = Vec::new();
    put_u32(&mut out, checked_usize_to_u32(pages.len())?);
    for page in pages {
        put_u64(&mut out, page.vaddr);
        put_u32(&mut out, checked_usize_to_u32(page.bytes.len())?);
        out.extend_from_slice(&page.bytes);
    }
    Ok(out)
}

// AGENT: serialize fd table entries.
fn encode_fds(fds: &[SavedFdEntry]) -> Result<Vec<u8>, CheckpointError> {
    let mut out = Vec::new();
    put_u32(&mut out, checked_usize_to_u32(fds.len())?);
    for fd in fds {
        put_u32(&mut out, fd.fd);
        put_u32(&mut out, fd.description_id);
        out.push(u8::from(fd.cloexec));
        out.extend_from_slice(&[0, 0, 0]);
        put_u32(&mut out, fd.status_flags);
        put_u16(&mut out, fd.kind as u16);
        put_u16(&mut out, 0);
        put_u64(&mut out, fd.offset);
    }
    Ok(out)
}

// AGENT: serialize restartable timer metadata.
fn encode_timers(timers: &[SavedTimer]) -> Result<Vec<u8>, CheckpointError> {
    let mut out = Vec::new();
    put_u32(&mut out, checked_usize_to_u32(timers.len())?);
    for timer in timers {
        put_u32(&mut out, timer.clock_id);
        put_u16(&mut out, timer.target_kind as u16);
        put_u16(&mut out, 0);
        put_u32(&mut out, timer.signo as u32);
        put_u32(&mut out, 0);
        put_u64(&mut out, timer.sender_tid as u64);
        put_u64(&mut out, timer.deadline_ticks);
        put_u64(&mut out, timer.interval_ticks);
    }
    Ok(out)
}
