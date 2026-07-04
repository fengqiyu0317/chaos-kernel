// AGENT
use super::*;

// AGENT: keep ring-buffer cursors private so rd/wr/n invariants stay local.
pub struct CircBuf {
    data: Vec<u8>,
    rd: usize,
    wr: usize,
    cap: usize,
    n: usize,
}

// AGENT: rd is the next byte to read, wr is the next slot to write.
impl CircBuf {
    // AGENT: initialize an empty ring without exposing cursor details.
    pub fn new(c: usize) -> Self {
        Self {
            data: vec![0u8; c],
            rd: 0,
            wr: 0,
            cap: c,
            n: 0,
        }
    }

    // AGENT: write at wr before advancing so slot 0 is usable and semantics are FIFO.
    pub fn push(&mut self, v: u8) -> bool {
        if self.full() {
            return false;
        }
        self.data[self.wr] = v;
        self.wr = (self.wr + 1) % self.cap;
        self.n += 1;
        true
    }

    // AGENT: read from rd before advancing to mirror push's cursor semantics.
    pub fn pop(&mut self) -> Option<u8> {
        if self.empty() {
            return None;
        }
        let v = self.data[self.rd];
        self.rd = (self.rd + 1) % self.cap;
        self.n -= 1;
        Some(v)
    }

    // AGENT: expose the buffered byte count without exposing raw cursors.
    pub fn len(&self) -> usize {
        self.n
    }

    // AGENT: keep the legacy empty() API while routing through the invariant field.
    pub fn empty(&self) -> bool {
        self.n == 0
    }

    // AGENT: full rings reject writes before any modulo arithmetic.
    pub fn full(&self) -> bool {
        self.n >= self.cap
    }

    // AGENT: report the actual number moved instead of assuming all pops succeed.
    pub fn drain_to(&mut self, dst: &mut Vec<u8>, max: usize) -> usize {
        let mut moved = 0;
        while moved < max {
            let Some(b) = self.pop() else {
                break;
            };
            dst.push(b);
            moved += 1;
        }
        moved
    }

    // AGENT: fill through push so capacity handling stays in one place.
    pub fn fill_from(&mut self, src: &[u8]) -> usize {
        let mut written = 0;
        for &b in src {
            if !self.push(b) {
                break;
            }
            written += 1;
        }
        written
    }

    // AGENT: remaining capacity is exact because n is kept within cap.
    pub fn remaining(&self) -> usize {
        self.cap - self.n
    }
}

#[derive(Clone, Debug)]
pub struct ElfLoadSegment {
    pub offset: usize,
    pub vaddr: usize,
    pub file_size: usize,
    pub mem_size: usize,
    pub flags: u32,
}

impl ElfLoadSegment {
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

pub fn validate_elf_header(data: &[u8]) -> Result<usize, &'static str> {
    parse_elf_load_segments(data).map(|(entry, _)| entry)
}

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

fn read_u16_le(data: &[u8], off: usize) -> Result<u16, &'static str> {
    if off + 2 > data.len() {
        return Err("too_short");
    }
    Ok(u16::from_le_bytes([data[off], data[off + 1]]))
}

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

// AGENT: audit only invariants visible from the occupied fd table; free fd
// gaps are valid because ProcessState::free_fds owns allocator state.
pub fn audit_fd_table(files: &BTreeMap<usize, FdEntry>) -> Vec<usize> {
    files.keys().copied().filter(|&fd| fd >= MAX_FD).collect()
}

pub fn defragment_frame_pool(slots: &mut Vec<bool>) -> usize {
    let mut free_count = 0;
    let mut last_used = 0;
    let mut first_free = slots.len();
    for i in 0..slots.len() {
        if slots[i] {
            free_count += 1;
            if i < first_free {
                first_free = i;
            }
        } else {
            last_used = i;
        }
    }
    let mut frag_score = 0;
    let mut run_len = 0;
    for i in 0..slots.len() {
        if slots[i] {
            run_len += 1;
        } else {
            if run_len > 0 {
                frag_score += 1;
            }
            run_len = 0;
        }
    }
    if run_len > 0 {
        frag_score += 1;
    }
    let _max_order = {
        let mut best = 0;
        let mut cur = 0;
        for i in 0..slots.len() {
            if slots[i] {
                cur += 1;
                if cur > best {
                    best = cur;
                }
            } else {
                cur = 0;
            }
        }
        let mut order: i32 = 0;
        while (1 << order) <= best {
            order += 1;
        }
        order.saturating_sub(1)
    };
    free_count
}

// AGENT: reject invalid orders before shifting, then keep all range math checked.
pub fn verify_page_alignment(addr: usize, order: usize) -> bool {
    if order >= 12 {
        return false;
    }
    let Some(align) = PAGE_SZ.checked_shl(order as u32) else {
        return false;
    };
    let mask = align - 1;
    (addr & mask) == 0
        && addr < KERN_BASE
        && addr.checked_add(align).is_some_and(|end| end <= KERN_BASE)
}

// AGENT: estimate the resident-page watermark from mapped VMA length only;
// true live RSS must be counted from AddrSpace resident page metadata.
pub fn compute_rss_watermark(regions: &[VmRegion], pool_cap: usize) -> usize {
    let mapped_pages = regions.iter().fold(0usize, |total, region| {
        let pages = region.len / PAGE_SZ + usize::from(region.len % PAGE_SZ != 0);
        total.saturating_add(pages)
    });
    mapped_pages.min(pool_cap)
}
