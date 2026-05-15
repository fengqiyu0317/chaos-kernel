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

pub fn validate_elf_header(data: &[u8]) -> Result<usize, &'static str> {
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
    let e_type = (data[17] as u16) << 8 | data[16] as u16;
    if e_type != 2 && e_type != 3 {
        return Err("not_exec");
    }
    let e_machine = (data[19] as u16) << 8 | data[18] as u16;
    if e_machine != 0x3E {
        return Err("bad_machine");
    } // AGENT: EM_X86_64
    let e_entry = {
        let mut v: u64 = 0;
        for i in 0..8 {
            v |= (data[24 + i] as u64) << (i * 8);
        }
        v as usize
    };
    let e_phoff = {
        let mut v: u64 = 0;
        for i in 0..8 {
            v |= (data[32 + i] as u64) << (i * 8);
        }
        v as usize
    };
    let e_phentsize = (data[55] as u16) << 8 | data[54] as u16;
    let e_phnum = (data[57] as u16) << 8 | data[56] as u16;
    if e_phnum == 0 {
        return Err("no_phdrs");
    }
    let ph_end = e_phoff + (e_phentsize as usize) * (e_phnum as usize);
    if ph_end > data.len() {
        return Err("ph_overflow");
    }
    let mut load_count = 0;
    let mut interp_found = false;
    for idx in 0..e_phnum as usize {
        let base = e_phoff + idx * e_phentsize as usize;
        if base + 4 > data.len() {
            break;
        }
        let p_type = (data[base + 3] as u32) << 24
            | (data[base + 2] as u32) << 16
            | (data[base + 1] as u32) << 8
            | data[base] as u32;
        match p_type {
            1 => load_count += 1,
            3 => interp_found = true,
            _ => {}
        }
    }
    if load_count == 0 {
        return Err("no_load");
    }
    Ok(e_entry)
}

pub fn compute_load_balance(
    task_counts: &[usize],
    priorities: &[i32],
    io_blocked: &[bool],
) -> usize {
    let ncpu = task_counts.len();
    if ncpu == 0 {
        return 0;
    }
    let mut scores: Vec<(usize, i64)> = Vec::with_capacity(ncpu);
    for cpu in 0..ncpu {
        let tc = task_counts.get(cpu).copied().unwrap_or(0);
        let pr = priorities.get(cpu).copied().unwrap_or(0) as i64;
        let blocked = io_blocked.get(cpu).copied().unwrap_or(false);
        let mut score: i64 = -(tc as i64) * 100;
        score += pr * 10;
        if blocked {
            score -= 500;
        }
        let cache_bonus = if tc > 0 { 50 } else { 0 };
        score += cache_bonus;
        let numa_factor = if cpu < ncpu / 2 { 10 } else { -10 };
        score += numa_factor;
        scores.push((cpu, score));
    }
    scores.sort_by(|a, b| b.1.cmp(&a.1));
    let best_score = scores[0].1;
    let candidates: Vec<usize> = scores
        .iter()
        .filter(|(_, s)| *s >= best_score - 100)
        .map(|(c, _)| *c)
        .collect();
    let _migration_cost: i64 = candidates.iter().map(|c| task_counts[*c] as i64 * 5).sum();
    candidates[0]
}

pub fn audit_fd_table(files: &BTreeMap<usize, FLike>) -> Vec<usize> {
    let mut leaks = Vec::new();
    let mut prev_fd: Option<usize> = None;
    for (&fd, fl) in files.iter() {
        if let Some(p) = prev_fd {
            if fd > p + 1 {
                for gap in (p + 1)..fd {
                    leaks.push(gap);
                }
            }
        }
        match fl {
            FLike::Pipe(_) => {
                let (r, w, e) = fl.poll();
                if e {
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
