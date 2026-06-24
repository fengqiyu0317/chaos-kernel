// AGENT
use super::*;

// AGENT: record whether a resident user page is anonymous or backed by a file.
#[derive(Clone)]
pub enum PageBacking {
    Anonymous,
    File {
        data: Arc<Mutex<Vec<u8>>>,
        offset: usize,
        valid_len: usize,
        shared: bool,
    },
}

impl PageBacking {
    // AGENT: flush MAP_SHARED page bytes back into the valid file-backed range.
    fn flush_range(&self, page: &[u8], page_off: usize, len: usize) -> Result<(), &'static str> {
        let PageBacking::File {
            data,
            offset,
            valid_len,
            shared,
        } = self
        else {
            return Ok(());
        };
        if !*shared || page_off >= *valid_len || page_off >= PAGE_SZ {
            return Ok(());
        }
        let page_end = min(PAGE_SZ, page_off.checked_add(len).ok_or("efault")?);
        let valid_end = min(*valid_len, page_end);
        if valid_end <= page_off {
            return Ok(());
        }
        let copy_len = valid_end - page_off;
        let file_start = offset.checked_add(page_off).ok_or("efault")?;
        let file_end = file_start.checked_add(copy_len).ok_or("efault")?;
        let mut file = data.lock().unwrap();
        if file_end > file.len() {
            file.resize(file_end, 0);
        }
        file[file_start..file_end].copy_from_slice(&page[page_off..valid_end]);
        Ok(())
    }
}

// AGENT: page-table entries now carry backing metadata for mmap writeback.
#[derive(Clone)]
pub struct PageTableEntry {
    pub frame_id: usize,
    pub frame: PgFrame,
    pub data: Arc<Mutex<Vec<u8>>>,
    pub backing: PageBacking,
    pub flags: u32,
    pub writable: bool,
    pub cow: bool,
    pub present: bool,
}

impl PageTableEntry {
    // AGENT: default page-table entries are anonymous zero-filled pages.
    pub fn new(frame_id: usize, frame: PgFrame, flags: u32) -> Self {
        Self::with_backing(frame_id, frame, flags, PageBacking::Anonymous)
    }

    // AGENT: allow mmap to seed resident pages with file backing metadata.
    pub fn with_backing(frame_id: usize, frame: PgFrame, flags: u32, backing: PageBacking) -> Self {
        Self {
            frame_id,
            frame,
            data: Arc::new(Mutex::new(vec![0; PAGE_SZ])),
            backing,
            flags,
            writable: flags & VM_WRITE != 0,
            cow: false,
            present: true,
        }
    }

    fn as_cow(&mut self) {
        self.writable = false;
        self.cow = true;
    }

    fn resolve_write(&mut self, frame_id: usize, frame: PgFrame, data: Vec<u8>) {
        self.frame_id = frame_id;
        self.frame = frame;
        self.data = Arc::new(Mutex::new(data));
        self.writable = self.flags & VM_WRITE != 0;
        self.cow = false;
        self.present = true;
    }

    fn set_flags(&mut self, flags: u32) {
        self.flags = flags;
        self.writable = flags & VM_WRITE != 0 && !self.cow;
    }

    // AGENT: flush a full resident page before unmap or address-space teardown.
    fn flush_shared_file_page(&self) -> Result<(), &'static str> {
        let page = self.data.lock().unwrap();
        self.backing.flush_range(&page, 0, PAGE_SZ)
    }

    // AGENT: drop one resident mapping reference and return the frame when it
    // is no longer shared by another PTE.
    fn release_frame_ref(&self, pool: &FramePool) -> bool {
        if self.frame.count() == 0 {
            return false;
        }
        let prev = self.frame.down();
        if prev == 1 {
            pool.put(self.frame_id);
            return true;
        }
        false
    }
}

pub struct AddrSpace {
    pub vm_map: VmMap,
    pub page_table_root: usize,
    pub asid: u16,
    pub page_table: Mutex<BTreeMap<usize, PageTableEntry>>,
}

static ADDR_SPACE_TOKEN_SEQ: AtomicUsize = AtomicUsize::new(1);

impl AddrSpace {
    pub fn new() -> Self {
        let page_table_root = next_vm_token();
        Self {
            vm_map: VmMap::new(),
            page_table_root,
            asid: asid_from_token(page_table_root),
            page_table: Mutex::new(BTreeMap::new()),
        }
    }

    pub fn vm_token(&self) -> usize {
        self.page_table_root
    }

    pub fn fork_from(parent: &AddrSpace) -> Self {
        let mut child = Self::new();
        child.vm_map.brk = parent.vm_map.brk;
        child.vm_map.mmap_base = parent.vm_map.mmap_base;
        for region in parent.vm_map.regions.iter() {
            if region.flags & VM_DONTCOPY != 0 {
                continue;
            }
            let new_region = VmRegion {
                base: region.base,
                len: region.len,
                flags: region.flags,
                offset: region.offset,
                tag: region.tag,
            };
            let _ = child.vm_map.insert(new_region);
        }

        let copyable_regions: Vec<(usize, usize, u32)> = parent
            .vm_map
            .regions
            .iter()
            .filter(|region| region.flags & VM_DONTCOPY == 0)
            .map(|region| (region.base, region.end(), region.flags))
            .collect();
        let mut parent_pt = parent.page_table.lock().unwrap();
        let mut child_pt = child.page_table.lock().unwrap();
        for (&page_addr, parent_entry) in parent_pt.iter_mut() {
            let Some((_, _, flags)) = copyable_regions
                .iter()
                .find(|(base, end, _)| page_addr >= *base && page_addr < *end)
            else {
                continue;
            };
            if !parent_entry.present {
                continue;
            }
            parent_entry.frame.up();
            if flags & VM_WRITE != 0 && flags & VM_SHARED == 0 {
                parent_entry.as_cow();
            }
            child_pt.insert(page_addr, parent_entry.clone());
        }
        drop(child_pt);
        child
    }

    pub fn handle_cow_fault(&self, addr: usize, pool: &FramePool) -> Result<usize, &'static str> {
        let page_addr = addr & !(PAGE_SZ - 1);
        let region = self.vm_map.find(addr).ok_or("segfault")?;
        if region.flags & VM_WRITE == 0 {
            return Err("segfault");
        }
        let mut pt = self.page_table.lock().unwrap();
        let pte = pt.get_mut(&page_addr).ok_or("segfault")?;
        if !pte.present {
            return Err("segfault");
        }
        if pte.writable && !pte.cow {
            return Ok(pte.frame_id * PAGE_SZ + MEM_OFF);
        }
        if !pte.cow {
            return Err("segfault");
        }

        let old_data = pte.data.lock().unwrap().clone();
        if pte.frame.count() <= 1 {
            pte.writable = pte.flags & VM_WRITE != 0;
            pte.cow = false;
            return Ok(pte.frame_id * PAGE_SZ + MEM_OFF);
        }

        let new_frame_id = pool.get_inner().ok_or("oom")?;
        pte.frame.down();
        pte.resolve_write(new_frame_id, PgFrame::with_rc(1), old_data);
        Ok(new_frame_id * PAGE_SZ + MEM_OFF)
    }

    fn checked_user_end(addr: usize, len: usize) -> Result<usize, &'static str> {
        let end = addr.checked_add(len).ok_or("efault")?;
        if end > KERN_BASE {
            return Err("efault");
        }
        Ok(end)
    }

    pub fn read_user_bytes(&self, addr: usize, dst: &mut [u8]) -> Result<(), &'static str> {
        let end = Self::checked_user_end(addr, dst.len())?;
        let mut copied = 0usize;
        while copied < dst.len() {
            let cur = addr + copied;
            let region = self.vm_map.find(cur).ok_or("efault")?;
            if region.flags & VM_READ == 0 {
                return Err("efault");
            }
            let page_addr = cur & !(PAGE_SZ - 1);
            let page_off = cur & (PAGE_SZ - 1);
            let chunk = min(end - cur, min(PAGE_SZ - page_off, region.end() - cur));
            let page_data = {
                let pt = self.page_table.lock().unwrap();
                let pte = pt.get(&page_addr).ok_or("efault")?;
                if !pte.present {
                    return Err("efault");
                }
                pte.data.clone()
            };
            let page = page_data.lock().unwrap();
            dst[copied..copied + chunk].copy_from_slice(&page[page_off..page_off + chunk]);
            copied += chunk;
        }
        Ok(())
    }

    pub fn read_user_usize(&self, addr: usize) -> Result<usize, &'static str> {
        let mut bytes = [0u8; std::mem::size_of::<usize>()];
        self.read_user_bytes(addr, &mut bytes)?;
        Ok(usize::from_ne_bytes(bytes))
    }

    // AGENT: user writes to MAP_SHARED file pages are reflected in FileNode data.
    pub fn write_user_bytes(
        &mut self,
        addr: usize,
        src: &[u8],
        pool: &FramePool,
    ) -> Result<(), &'static str> {
        let end = Self::checked_user_end(addr, src.len())?;
        let mut written = 0usize;
        while written < src.len() {
            let cur = addr + written;
            let region = self.vm_map.find(cur).ok_or("efault")?;
            if region.flags & VM_WRITE == 0 {
                return Err("efault");
            }
            let page_addr = cur & !(PAGE_SZ - 1);
            let page_off = cur & (PAGE_SZ - 1);
            let chunk = min(end - cur, min(PAGE_SZ - page_off, region.end() - cur));
            let need_cow = {
                let pt = self.page_table.lock().unwrap();
                let pte = pt.get(&page_addr).ok_or("efault")?;
                if !pte.present {
                    return Err("efault");
                }
                !pte.writable && pte.cow
            };
            if need_cow {
                self.handle_cow_fault(cur, pool).map_err(|_| "efault")?;
            }
            let (page_data, backing) = {
                let pt = self.page_table.lock().unwrap();
                let pte = pt.get(&page_addr).ok_or("efault")?;
                if !pte.present || !pte.writable {
                    return Err("efault");
                }
                (pte.data.clone(), pte.backing.clone())
            };
            let mut page = page_data.lock().unwrap();
            page[page_off..page_off + chunk].copy_from_slice(&src[written..written + chunk]);
            backing.flush_range(&page, page_off, chunk)?;
            written += chunk;
        }
        Ok(())
    }

    // AGENT: unmapping flushes resident shared file-backed pages before
    // removing mappings, and returns last-reference frames to FramePool.
    pub fn unmap_range(
        &mut self,
        start: usize,
        len: usize,
        pool: &FramePool,
    ) -> Result<usize, &'static str> {
        let end = start.checked_add(len).ok_or("efault")?;
        let mut pt = self.page_table.lock().unwrap();
        let pages_to_unmap: Vec<usize> = pt
            .keys()
            .filter(|&&addr| addr >= start && addr < end)
            .copied()
            .collect();
        for addr in &pages_to_unmap {
            if let Some(pte) = pt.get(addr) {
                pte.flush_shared_file_page()?;
            }
        }
        self.vm_map.remove_range(start, len);
        for addr in &pages_to_unmap {
            if let Some(pte) = pt.remove(addr) {
                pte.release_frame_ref(pool);
            }
        }
        Ok(pages_to_unmap.len())
    }

    // AGENT: process teardown flushes shared file-backed pages before dropping frames.
    pub fn release_all_pages(&mut self, pool: &FramePool) -> usize {
        self.vm_map.regions.clear();
        let entries: Vec<PageTableEntry> = {
            let mut pt = self.page_table.lock().unwrap();
            let entries = pt.values().cloned().collect();
            pt.clear();
            entries
        };
        let mut released = 0;
        for pte in entries {
            if !pte.present {
                continue;
            }
            let _ = pte.flush_shared_file_page();
            if pte.release_frame_ref(pool) {
                released += 1;
            }
        }
        released
    }

    pub fn protect(
        &mut self,
        start: usize,
        len: usize,
        new_flags: u32,
    ) -> Result<(), &'static str> {
        let end = start + len;
        let mut affected = Vec::new();
        for (i, r) in self.vm_map.regions.iter().enumerate() {
            if r.base < end && r.end() > start {
                affected.push(i);
            }
        }
        for &idx in affected.iter().rev() {
            if idx < self.vm_map.regions.len() {
                self.vm_map.regions[idx].flags = new_flags;
            }
        }
        let mut pt = self.page_table.lock().unwrap();
        for (addr, pte) in pt.iter_mut() {
            if *addr >= start && *addr < end {
                pte.set_flags(new_flags);
            }
        }
        Ok(())
    }

    pub fn rss_pages(&self) -> usize {
        self.page_table.lock().unwrap().len()
    }

    pub fn cow_sharers(&self) -> usize {
        let pt = self.page_table.lock().unwrap();
        pt.values()
            .filter(|pte| pte.cow && pte.frame.count() > 1)
            .count()
    }

    pub fn split_region(&mut self, addr: usize) -> Result<(), &'static str> {
        let idx = self
            .vm_map
            .regions
            .iter()
            .position(|region| region.contains(addr))
            .ok_or("enomem")?;
        let (left, right) = self.vm_map.regions[idx].split_at(addr).ok_or("einval")?;
        self.vm_map.regions[idx] = left;
        self.vm_map.regions.insert(idx + 1, right);
        Ok(())
    }

    pub fn map_region(&mut self, region: VmRegion, pool: &FramePool) -> Result<(), &'static str> {
        if region.base % PAGE_SZ != 0 || region.len % PAGE_SZ != 0 {
            return Err("einval");
        }
        let flags = region.flags;
        let pages: Vec<usize> = page_range(region.base, region.len).collect();
        let mut allocated = Vec::with_capacity(pages.len());
        for _ in pages.iter() {
            match pool.get_inner() {
                Some(frame_id) => allocated.push(frame_id),
                None => {
                    for frame_id in allocated {
                        pool.put(frame_id);
                    }
                    return Err("enomem");
                }
            }
        }
        if let Err(err) = self.vm_map.insert(region) {
            for frame_id in allocated {
                pool.put(frame_id);
            }
            return Err(err);
        }
        let mut pt = self.page_table.lock().unwrap();
        for (page_addr, frame_id) in pages.into_iter().zip(allocated.into_iter()) {
            pt.insert(
                page_addr,
                PageTableEntry::new(frame_id, PgFrame::with_rc(1), flags),
            );
        }
        Ok(())
    }

    // AGENT: create resident file-backed mmap pages, preserving private snapshots
    // and shared writeback metadata for each page.
    pub fn map_file_region(
        &mut self,
        region: VmRegion,
        file_data: Arc<Mutex<Vec<u8>>>,
        shared: bool,
        pool: &FramePool,
    ) -> Result<(), &'static str> {
        if region.base % PAGE_SZ != 0 || region.len % PAGE_SZ != 0 || region.offset % PAGE_SZ != 0 {
            return Err("einval");
        }
        let flags = region.flags;
        let file_base = region.offset;
        let pages: Vec<usize> = page_range(region.base, region.len).collect();
        let mut file_offsets = Vec::with_capacity(pages.len());
        for idx in 0..pages.len() {
            let delta = idx.checked_mul(PAGE_SZ).ok_or("einval")?;
            file_offsets.push(file_base.checked_add(delta).ok_or("einval")?);
        }

        let mut allocated = Vec::with_capacity(pages.len());
        for _ in pages.iter() {
            match pool.get_inner() {
                Some(frame_id) => allocated.push(frame_id),
                None => {
                    for frame_id in allocated {
                        pool.put(frame_id);
                    }
                    return Err("enomem");
                }
            }
        }

        let file_snapshot = file_data.lock().unwrap().clone();
        if let Err(err) = self.vm_map.insert(region) {
            for frame_id in allocated {
                pool.put(frame_id);
            }
            return Err(err);
        }

        let mut pt = self.page_table.lock().unwrap();
        for ((page_addr, frame_id), file_offset) in pages
            .into_iter()
            .zip(allocated.into_iter())
            .zip(file_offsets.into_iter())
        {
            let valid_len = if file_offset < file_snapshot.len() {
                min(PAGE_SZ, file_snapshot.len() - file_offset)
            } else {
                0
            };
            let backing = PageBacking::File {
                data: file_data.clone(),
                offset: file_offset,
                valid_len,
                shared,
            };
            let pte = PageTableEntry::with_backing(frame_id, PgFrame::with_rc(1), flags, backing);
            if valid_len > 0 {
                pte.data.lock().unwrap()[..valid_len]
                    .copy_from_slice(&file_snapshot[file_offset..file_offset + valid_len]);
            }
            pt.insert(page_addr, pte);
        }
        Ok(())
    }

    pub fn resize_brk(&mut self, new_brk: usize, pool: &FramePool) -> Result<(), &'static str> {
        let old_brk = self.vm_map.brk;
        if new_brk < old_brk {
            self.unmap_range(new_brk, old_brk - new_brk, pool)?;
        } else if new_brk > old_brk {
            let heap = VmRegion::new(old_brk, new_brk - old_brk, VM_READ | VM_WRITE);
            self.map_region(heap, pool)?;
        }
        self.vm_map.brk = new_brk;
        Ok(())
    }
}

fn page_range(base: usize, len: usize) -> impl Iterator<Item = usize> {
    let start = base & !(PAGE_SZ - 1);
    let end = (base + len + PAGE_SZ - 1) & !(PAGE_SZ - 1);
    (start..end).step_by(PAGE_SZ)
}

fn next_vm_token() -> usize {
    // AGENT TODO: This is a simulation-only address-space token. A fuller MMU
    // model should allocate a real page-table root/satp token and pair ASID
    // reuse with generation tracking plus TLB invalidation.
    ADDR_SPACE_TOKEN_SEQ
        .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |token| {
            token.checked_add(1)
        })
        .expect("address-space token exhausted")
}

fn asid_from_token(token: usize) -> u16 {
    let max_asid = u16::MAX as usize;
    ((token - 1) % max_asid + 1) as u16
}
