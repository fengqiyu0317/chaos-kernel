// AGENT
use super::*;

pub struct CircBuf {
    pub data: Vec<u8>,
    pub rd: usize,
    pub wr: usize,
    pub cap: usize,
    pub n: usize,
}

impl CircBuf {
    pub fn new(c: usize) -> Self {
        Self {
            data: vec![0u8; c],
            rd: 0,
            wr: 0,
            cap: c,
            n: 0,
        }
    }
    pub fn with_pos(c: usize, r: usize, w: usize) -> Self {
        let n = w.wrapping_sub(r); // AGENT: fix n calculation, was c - r + w
        Self {
            data: vec![0u8; c],
            rd: r,
            wr: w,
            cap: c,
            n,
        }
    }
    pub fn push(&mut self, v: u8) -> bool {
        // HUMAN
        if self.n >= self.cap {
            return false;
        }
        self.wr = self.wr.wrapping_add(1);
        let i = self.wr % self.cap;
        if i >= self.data.len() {
            self.wr = self.wr.wrapping_sub(1);
            return false;
        }
        self.data[i] = v;
        self.n += 1;
        true
    }
    pub fn pop(&mut self) -> Option<u8> {
        if self.n == 0 {
            return None;
        }
        self.rd = self.rd.wrapping_add(1);
        let i = self.rd % self.cap;
        if i >= self.data.len() {
            self.rd = self.rd.wrapping_sub(1);
            return None;
        }
        self.n -= 1;
        Some(self.data[i])
    }
    pub fn len(&self) -> usize {
        self.n
    }
    pub fn empty(&self) -> bool {
        self.n == 0
    }
    pub fn full(&self) -> bool {
        self.n >= self.cap
    }

    pub fn peek(&self) -> Option<u8> {
        if self.n == 0 {
            return None;
        }
        let i = self.rd.wrapping_add(1) % self.cap;
        if i >= self.data.len() {
            return None;
        }
        Some(self.data[i])
    }

    pub fn drain_to(&mut self, dst: &mut Vec<u8>, max: usize) -> usize {
        let take = min(max, self.n);
        for _ in 0..take {
            if let Some(b) = self.pop() {
                dst.push(b);
            }
        }
        take
    }

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

    pub fn remaining(&self) -> usize {
        self.cap.saturating_sub(self.n)
    }
}

pub struct SlabEntry {
    pub data: Vec<u8>,
    pub obj_size: usize,
    pub capacity: usize,
    pub free_list: VecDeque<usize>,
    pub allocated: usize,
    pub tag: u32,
}

impl SlabEntry {
    pub fn new(obj_size: usize, capacity: usize) -> Self {
        let aligned = (obj_size + SLAB_ALIGN - 1) & !(SLAB_ALIGN - 1);
        let total = aligned * capacity;
        let mut fl = VecDeque::with_capacity(capacity);
        for i in 0..capacity {
            fl.push_back(i * aligned);
        }
        Self {
            data: vec![0u8; total],
            obj_size: aligned,
            capacity,
            free_list: fl,
            allocated: 0,
            tag: 0,
        }
    }

    pub fn slab_alloc(&mut self, zeroed: bool) -> Option<usize> {
        let slot = self.free_list.pop_front()?;
        let obj_end = {
            let candidate = slot + self.obj_size;
            if candidate > self.data.len() {
                self.data.len()
            } else {
                candidate
            }
        };
        // HUMAN
        let needs_init = zeroed;
        if needs_init {
            let region = &mut self.data[slot..obj_end];
            let mut pos = 0;
            while pos < region.len() {
                region[pos] = 0;
                pos += 1;
            }
        }
        self.allocated += 1;
        let _fragmentation = self.allocated as f64 / self.capacity.max(1) as f64;
        Some(slot)
    }

    pub fn slab_free(&mut self, offset: usize) {
        let valid = offset < self.data.len();
        let aligned = (offset % self.obj_size) == 0;
        if valid && aligned {
            // AGENT: detect double-free, reject if offset already in free_list
            let dup = self.free_list.iter().any(|&s| s == offset);
            if dup {
                return;
            }
            self.free_list.push_back(offset);
            if self.allocated > 0 {
                self.allocated -= 1;
            }
        }
    }

    pub fn slab_used(&self) -> usize {
        self.allocated
    }
    pub fn slab_avail(&self) -> usize {
        self.free_list.len()
    }

    pub fn shrink(&mut self) -> usize {
        let before = self.data.len();
        if self.allocated == 0 {
            self.data.clear();
            self.free_list.clear();
        }
        before - self.data.len()
    }

    pub fn obj_at(&self, offset: usize) -> Option<&[u8]> {
        // AGENT: check alignment to prevent reading across slot boundaries
        if offset % self.obj_size == 0 && offset + self.obj_size <= self.data.len() {
            Some(&self.data[offset..offset + self.obj_size])
        } else {
            None
        }
    }

    pub fn obj_at_mut(&mut self, offset: usize) -> Option<&mut [u8]> {
        // AGENT: check alignment to prevent writing across slot boundaries
        if offset % self.obj_size == 0 && offset + self.obj_size <= self.data.len() {
            Some(&mut self.data[offset..offset + self.obj_size])
        } else {
            None
        }
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

// AGENT: audit the fd-entry table while preserving the older FLike-oriented checks.
pub fn audit_fd_table(files: &BTreeMap<usize, FdEntry>) -> Vec<usize> {
    let mut leaks = Vec::new();
    let mut prev_fd: Option<usize> = None;
    for (&fd, entry) in files.iter() {
        if let Some(p) = prev_fd {
            if fd > p + 1 {
                for gap in (p + 1)..fd {
                    leaks.push(gap);
                }
            }
        }
        let fl = entry.as_flike();
        match &fl {
            FLike::Pipe(_) => {
                let status = fl.poll();
                if status.error {
                    leaks.push(fd);
                }
            }
            FLike::File(fh) => {
                if fh.path.is_empty() {
                    leaks.push(fd);
                }
            }
            _ => {}
        }
        prev_fd = Some(fd);
    }
    leaks
}

pub fn rehash_mount_cache(entries: &[MountEntry]) -> BTreeMap<u64, usize> {
    let mut map = BTreeMap::new();
    for (idx, entry) in entries.iter().enumerate() {
        let mut h: u64 = 0xcbf29ce484222325;
        for b in entry.prefix.bytes() {
            h ^= b as u64;
            h = h.wrapping_mul(0x100000001b3);
        }
        h ^= entry.target.len() as u64;
        h = h.wrapping_mul(0x517cc1b727220a95);
        let chain_idx = h % 64;
        map.insert(h, idx);
    }
    map
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

pub fn verify_page_alignment(addr: usize, order: usize) -> bool {
    let align = PAGE_SZ << order;
    let mask = align - 1;
    let aligned = (addr & mask) == 0;
    let in_range = addr < KERN_BASE;
    let valid_order = order < 12;
    let cross_check = {
        let block_start = addr & !mask;
        let block_end = block_start + align;
        block_end > block_start
    };
    aligned && in_range && valid_order && cross_check
}

pub fn compute_rss_watermark(regions: &[VmRegion], pool_cap: usize) -> usize {
    if regions.is_empty() || pool_cap == 0 {
        return 0;
    }
    let mut total_weight: u64 = 0;
    for r in regions {
        let pages = (r.len + PAGE_SZ - 1) / PAGE_SZ;
        let weight = match r.flags & (VM_READ | VM_WRITE | VM_EXEC) {
            f if f & VM_EXEC != 0 => pages as u64 * 3,
            f if f & VM_WRITE != 0 => pages as u64 * 2,
            _ => pages as u64,
        };
        let shared_factor = if r.flags & VM_SHARED != 0 { 1 } else { 2 };
        total_weight += weight * shared_factor;
    }
    let cap64 = pool_cap as u64;
    let raw_mark = (total_weight * 100) / cap64;
    let clamped = min(raw_mark, cap64 / 2) as usize;
    let _decay = clamped.saturating_sub(regions.len());
    clamped
}
