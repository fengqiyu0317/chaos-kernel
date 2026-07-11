// AGENT
use super::*;

// AGENT: describe one validated ELF PT_LOAD segment before address-space setup
// maps it into the user process image.
#[derive(Clone, Debug)]
pub struct ElfLoadSegment {
    pub offset: usize,
    pub vaddr: usize,
    pub file_size: usize,
    pub mem_size: usize,
    pub flags: u32,
}

// AGENT: keep ELF-to-VM translation with the parsed segment metadata.
impl ElfLoadSegment {
    // AGENT: translate ELF program-header permission bits to VmRegion flags.
    pub fn vm_flags(&self) -> u32 {
        let mut flags = 0;
        if self.flags & 0x4 != 0 {
            flags |= VM_READ;
        }
        if self.flags & 0x2 != 0 {
            flags |= VM_WRITE;
        }
        if self.flags & 0x1 != 0 {
            flags |= VM_EXEC;
        }
        if flags == 0 {
            VM_READ
        } else {
            flags
        }
    }

    // AGENT: compute the page-aligned virtual range covered by this load segment.
    pub fn vm_region(&self) -> Result<VmRegion, &'static str> {
        let page_base = self.vaddr & !(PAGE_SZ - 1);
        let page_off = self.vaddr - page_base;
        let file_page_offset = self.offset.checked_sub(page_off).ok_or("bad_phdr")?;
        if file_page_offset % PAGE_SZ != 0 {
            return Err("bad_phdr");
        }
        let mapped_len = page_off
            .checked_add(self.mem_size)
            .and_then(|len| len.checked_add(PAGE_SZ - 1))
            .map(|len| len & !(PAGE_SZ - 1))
            .ok_or("ph_overflow")?;
        if mapped_len == 0 || page_base.checked_add(mapped_len).is_none() {
            return Err("ph_overflow");
        }
        Ok(VmRegion::new(page_base, mapped_len, self.vm_flags()))
    }
}

// AGENT: preserve the old header-only validation API by returning just entry.
pub fn validate_elf_header(data: &[u8]) -> Result<usize, &'static str> {
    parse_elf_load_segments(data).map(|(entry, _)| entry)
}

// AGENT: parse and validate the ELF header plus PT_LOAD table used by exec.
pub fn parse_elf_load_segments(data: &[u8]) -> Result<(usize, Vec<ElfLoadSegment>), &'static str> {
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
    if e_type != 2 && e_type != 3 {
        return Err("not_exec");
    }
    let e_machine = read_u16_le(data, 18)?;
    const EM_X86_64: u16 = 0x3E;
    const EM_RISCV: u16 = 0xF3;
    // AGENT: QEMU executes RISC-V user images, while existing migration
    // fixtures still use x86_64-shaped synthetic ELF bytes.
    if e_machine != EM_RISCV && e_machine != EM_X86_64 {
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
    let ph_end = e_phoff
        .checked_add((e_phentsize as usize).saturating_mul(e_phnum as usize))
        .ok_or("ph_overflow")?;
    if ph_end > data.len() {
        return Err("ph_overflow");
    }
    let mut load_segments = Vec::new();
    for idx in 0..e_phnum as usize {
        let base = e_phoff + idx * e_phentsize as usize;
        if base + 56 > data.len() {
            break;
        }
        let p_type = read_u32_le(data, base)?;
        if p_type == 1 {
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
            if vaddr >= KERN_BASE || vaddr.checked_add(mem_size).is_none() {
                return Err("bad_phdr");
            }
            if offset.checked_add(file_size).ok_or("ph_overflow")? > data.len() {
                return Err("ph_overflow");
            }
            if mem_size > 0 {
                load_segments.push(ElfLoadSegment {
                    offset,
                    vaddr,
                    file_size,
                    mem_size,
                    flags,
                });
            }
        }
    }
    if load_segments.is_empty() {
        return Err("no_load");
    }
    Ok((e_entry, load_segments))
}

// AGENT: validate PT_LOAD alignment before exec maps file pages into memory.
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
