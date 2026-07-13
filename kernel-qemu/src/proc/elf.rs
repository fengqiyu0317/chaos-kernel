// AGENT
use super::*;

// AGENT: Sv39 lower-half canonical user addresses occupy [0, 2^38).
const SV39_USER_TOP: usize = 1usize << 38;

// AGENT: retain the validated subset of one ELF PT_LOAD program header for
// later process-image construction.
#[derive(Clone, Debug)]
pub struct ElfLoadSegment {
    pub offset: usize,
    pub vaddr: usize,
    pub file_size: usize,
    pub mem_size: usize,
    pub flags: u32,
}

// AGENT: retain one page-granular PT_LOAD mapping with the union of every
// segment permission that covers it, so shared boundary pages are mapped once.
#[derive(Clone, Debug)]
pub struct ElfLoadPage {
    pub vaddr: usize,
    pub flags: u32,
}

// AGENT: give callers a named parsed image instead of passing an entry point
// and load-segment vector as an anonymous tuple.
#[derive(Clone, Debug)]
pub struct ParsedElf {
    pub entry: usize,
    pub load_segments: Vec<ElfLoadSegment>,
    pub load_pages: Vec<ElfLoadPage>,
}

// AGENT: preserve the header-validation API while routing all validation
// through the complete ELF parser used by process image construction.
pub fn validate_elf_header(data: &[u8]) -> Result<usize, &'static str> {
    parse_elf(data).map(|elf| elf.entry)
}

// AGENT: parse and validate the ELF header and PT_LOAD metadata without
// performing filesystem I/O or mutating an address space.
pub fn parse_elf(data: &[u8]) -> Result<ParsedElf, &'static str> {
    if data.len() < 64 {
        return Err("too_short");
    }
    if data[0] != 0x7f || data[1] != b'E' || data[2] != b'L' || data[3] != b'F' {
        return Err("bad_magic");
    }
    let ei_class = data[4];
    if ei_class != 2 {
        return Err("not_64bit");
    }
    let ei_data = data[5];
    if ei_data != 1 {
        return Err("not_le");
    }
    let ei_version = data[6];
    if ei_version != 1 {
        return Err("bad_version");
    }
    let e_type = read_u16_le(data, 16)?;
    // AGENT: accept only fixed-address executables until load-bias,
    // interpreter, and relocation handling exist for ET_DYN images.
    if e_type != 2 {
        return Err("not_exec");
    }
    let e_machine = read_u16_le(data, 18)?;
    const EM_RISCV: u16 = 0xF3;
    // AGENT: reject foreign instruction sets before their bytes can reach the
    // RISC-V user execution path.
    if e_machine != EM_RISCV {
        return Err("bad_machine");
    }
    let e_entry = read_u64_le(data, 24)? as usize;
    let e_phoff = read_u64_le(data, 32)? as usize;
    let e_phentsize = read_u16_le(data, 54)?;
    let e_phnum = read_u16_le(data, 56)?;
    if e_phnum == 0 {
        return Err("no_phdrs");
    }
    if e_phentsize < 56 {
        return Err("bad_phent");
    }
    let ph_table_size = (e_phentsize as usize)
        .checked_mul(e_phnum as usize)
        .ok_or("ph_overflow")?;
    let ph_end = e_phoff.checked_add(ph_table_size).ok_or("ph_overflow")?;
    if ph_end > data.len() {
        return Err("ph_overflow");
    }
    let mut load_segments = Vec::new();
    let mut load_pages = BTreeMap::<usize, u32>::new();
    for idx in 0..e_phnum as usize {
        // AGENT: derive every table entry with checked arithmetic and reject a
        // malformed table instead of silently returning a partial parse.
        let base = idx
            .checked_mul(e_phentsize as usize)
            .and_then(|offset| e_phoff.checked_add(offset))
            .ok_or("ph_overflow")?;
        if base.checked_add(56).ok_or("ph_overflow")? > data.len() {
            return Err("ph_overflow");
        }
        let p_type = read_u32_le(data, base)?;
        const PT_LOAD: u32 = 1;
        const PT_INTERP: u32 = 3;
        if p_type == PT_INTERP {
            // AGENT: dynamically interpreted executables are not runnable until
            // the QEMU exec path can load an interpreter and apply relocations.
            return Err("enotsup");
        }
        if p_type != PT_LOAD {
            continue;
        }

        let flags = read_u32_le(data, base + 4)?;
        let offset = read_u64_le(data, base + 8)? as usize;
        let vaddr = read_u64_le(data, base + 16)? as usize;
        let file_size = read_u64_le(data, base + 32)? as usize;
        let mem_size = read_u64_le(data, base + 40)? as usize;
        let align = read_u64_le(data, base + 48)? as usize;
        if file_size > mem_size {
            return Err("bad_phdr");
        }
        validate_load_segment_alignment(offset, vaddr, align)?;
        if offset.checked_add(file_size).ok_or("ph_overflow")? > data.len() {
            return Err("ph_overflow");
        }
        if mem_size == 0 {
            continue;
        }

        let (page_start, page_end) = load_segment_page_range(vaddr, mem_size)?;
        // AGENT: aggregate page ownership and permissions during parsing;
        // multiple PT_LOAD segments may legally share a boundary page.
        for page_vaddr in (page_start..page_end).step_by(PAGE_SZ) {
            load_pages
                .entry(page_vaddr)
                .and_modify(|page_flags| *page_flags |= flags)
                .or_insert(flags);
        }
        load_segments.push(ElfLoadSegment {
            offset,
            vaddr,
            file_size,
            mem_size,
            flags,
        });
    }
    if load_segments.is_empty() {
        return Err("no_load");
    }
    // AGENT: the initial PC must name an instruction inside a mapped,
    // executable PT_LOAD segment rather than an arbitrary ELF-provided address.
    let entry_is_executable = load_segments.iter().any(|segment| {
        segment.flags & 0x1 != 0
            && segment
                .vaddr
                .checked_add(segment.mem_size)
                .is_some_and(|end| segment.vaddr <= e_entry && e_entry < end)
    });
    if !entry_is_executable {
        return Err("bad_entry");
    }
    Ok(ParsedElf {
        entry: e_entry,
        load_segments,
        load_pages: load_pages
            .into_iter()
            .map(|(vaddr, flags)| ElfLoadPage { vaddr, flags })
            .collect(),
    })
}

// AGENT: validate one nonempty load segment against the canonical Sv39 user
// range and return the exact page interval consumed by AddrSpace::map_region.
fn load_segment_page_range(vaddr: usize, mem_size: usize) -> Result<(usize, usize), &'static str> {
    let mem_end = vaddr.checked_add(mem_size).ok_or("bad_phdr")?;
    if vaddr >= SV39_USER_TOP || mem_end > SV39_USER_TOP {
        return Err("bad_phdr");
    }
    let page_start = align_down(vaddr, PAGE_SZ);
    let page_end = checked_align_up(mem_end, PAGE_SZ).ok_or("bad_phdr")?;
    if page_start >= page_end {
        return Err("bad_phdr");
    }
    Ok((page_start, page_end))
}

// AGENT: validate PT_LOAD alignment before process image construction maps
// file pages into user memory.
fn validate_load_segment_alignment(
    offset: usize,
    vaddr: usize,
    align: usize,
) -> Result<(), &'static str> {
    // AGENT: ELF PT_LOAD segments must be congruent in-file and in-memory.
    if align > 1 {
        if !align.is_power_of_two() {
            return Err("bad_phdr");
        }
        if offset % align != vaddr % align {
            return Err("bad_phdr");
        }
    }
    if offset % PAGE_SZ != vaddr % PAGE_SZ {
        return Err("bad_phdr");
    }
    Ok(())
}

// AGENT: read a checked little-endian u16 field from an ELF byte slice.
fn read_u16_le(data: &[u8], off: usize) -> Result<u16, &'static str> {
    if off + 2 > data.len() {
        return Err("too_short");
    }
    Ok(u16::from_le_bytes([data[off], data[off + 1]]))
}

// AGENT: read a checked little-endian u32 field from an ELF byte slice.
fn read_u32_le(data: &[u8], off: usize) -> Result<u32, &'static str> {
    if off + 4 > data.len() {
        return Err("too_short");
    }
    Ok(u32::from_le_bytes([
        data[off],
        data[off + 1],
        data[off + 2],
        data[off + 3],
    ]))
}

// AGENT: read a checked little-endian u64 field from an ELF byte slice.
fn read_u64_le(data: &[u8], off: usize) -> Result<u64, &'static str> {
    if off + 8 > data.len() {
        return Err("too_short");
    }
    Ok(u64::from_le_bytes([
        data[off],
        data[off + 1],
        data[off + 2],
        data[off + 3],
        data[off + 4],
        data[off + 5],
        data[off + 6],
        data[off + 7],
    ]))
}
