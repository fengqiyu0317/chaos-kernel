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

// AGENT: page-table entries reference a SharedPage for resident frame/data
// ownership and keep only PTE-local permission plus backing metadata here.
pub struct PageTableEntry {
    pub page: SharedPage,
    pub backing: PageBacking,
    pub flags: u32,
    pub writable: bool,
    pub cow: bool,
    pub present: bool,
}

impl PageTableEntry {
    // AGENT: default page-table entries are anonymous zero-filled pages.
    pub fn new(frame: PgFrame, flags: u32) -> Self {
        Self::with_backing(SharedPage::new(frame), flags, PageBacking::Anonymous)
    }

    // AGENT: allow mmap to seed resident pages with file backing metadata.
    pub fn with_backing(page: SharedPage, flags: u32, backing: PageBacking) -> Self {
        Self {
            page,
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

    fn resolve_write(&mut self, pool: &FramePool) -> Result<usize, &'static str> {
        let paddr = self.page.fault(pool)?;
        self.writable = self.flags & VM_WRITE != 0;
        self.cow = false;
        self.present = true;
        Ok(paddr)
    }

    fn set_flags(&mut self, flags: u32) {
        self.flags = flags;
        self.writable = flags & VM_WRITE != 0 && !self.cow;
    }

    pub fn frame_id(&self) -> usize {
        self.page.frame_id()
    }

    // AGENT: clone only when a new PTE mapping should share the same frame.
    fn clone_mapping(&self) -> Self {
        Self {
            page: self.page.clone(),
            backing: self.backing.clone(),
            flags: self.flags,
            writable: self.writable,
            cow: self.cow,
            present: self.present,
        }
    }

    // AGENT: flush a full resident page before unmap or address-space teardown.
    fn flush_shared_file_page(&self) -> Result<(), &'static str> {
        let page_data = self.page.data();
        let page = page_data.lock().unwrap();
        self.backing.flush_range(&page, 0, PAGE_SZ)
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
            if flags & VM_WRITE != 0 && flags & VM_SHARED == 0 {
                parent_entry.as_cow();
            }
            child_pt.insert(page_addr, parent_entry.clone_mapping());
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
            return Ok(pte.page.paddr());
        }
        if !pte.cow {
            return Err("segfault");
        }

        pte.resolve_write(pool)
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
                pte.page.data()
            };
            let page = page_data.lock().unwrap();
            dst[copied..copied + chunk].copy_from_slice(&page[page_off..page_off + chunk]);
            copied += chunk;
        }
        Ok(())
    }

    // AGENT: report the contiguous readable prefix of a user buffer so syscalls
    // can return short I/O instead of faulting after partial progress.
    pub fn readable_user_prefix_len(&self, addr: usize, len: usize) -> Result<usize, &'static str> {
        self.accessible_user_prefix_len(addr, len, VM_READ)
    }

    // AGENT: report the contiguous writable prefix of a user buffer; COW pages
    // count as writable because write_user_bytes can resolve them later.
    pub fn writable_user_prefix_len(&self, addr: usize, len: usize) -> Result<usize, &'static str> {
        self.accessible_user_prefix_len(addr, len, VM_WRITE)
    }

    // AGENT: shared prefix scanner for syscall copy-in/copy-out validation.
    fn accessible_user_prefix_len(
        &self,
        addr: usize,
        len: usize,
        required: u32,
    ) -> Result<usize, &'static str> {
        if len == 0 {
            return Ok(0);
        }
        let end = Self::checked_user_end(addr, len)?;
        let mut checked = 0usize;
        while checked < len {
            let cur = addr + checked;
            let Some(region) = self.vm_map.find(cur) else {
                return if checked == 0 {
                    Err("efault")
                } else {
                    Ok(checked)
                };
            };
            if region.flags & required == 0 {
                return if checked == 0 {
                    Err("efault")
                } else {
                    Ok(checked)
                };
            }
            let page_addr = cur & !(PAGE_SZ - 1);
            let page_off = cur & (PAGE_SZ - 1);
            let chunk = min(end - cur, min(PAGE_SZ - page_off, region.end() - cur));
            let page_accessible = {
                let pt = self.page_table.lock().unwrap();
                match pt.get(&page_addr) {
                    Some(pte) if pte.present => {
                        if required & VM_WRITE != 0 {
                            pte.writable || pte.cow
                        } else {
                            true
                        }
                    }
                    _ => false,
                }
            };
            if !page_accessible {
                return if checked == 0 {
                    Err("efault")
                } else {
                    Ok(checked)
                };
            }
            checked += chunk;
        }
        Ok(checked)
    }

    pub fn read_user_usize(&self, addr: usize) -> Result<usize, &'static str> {
        let mut bytes = [0u8; mem::size_of::<usize>()];
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
                (pte.page.data(), pte.backing.clone())
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
        _pool: &FramePool,
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
            let _dropped = pt.remove(addr);
        }
        Ok(pages_to_unmap.len())
    }

    // AGENT: process teardown flushes shared file-backed pages before dropping frames.
    pub fn release_all_pages(&mut self, _pool: &FramePool) -> usize {
        self.vm_map.regions.clear();
        let entries = {
            let mut pt = self.page_table.lock().unwrap();
            mem::take(&mut *pt)
        };
        let mut released = 0;
        for pte in entries.into_values() {
            if !pte.present {
                continue;
            }
            let _ = pte.flush_shared_file_page();
            if pte.page.is_unique() {
                released += 1;
            }
        }
        released
    }

    // AGENT: reject overflowed protection ranges before comparing mapped regions.
    pub fn protect(
        &mut self,
        start: usize,
        len: usize,
        new_flags: u32,
    ) -> Result<(), &'static str> {
        let end = start.checked_add(len).ok_or("efault")?;
        if end > KERN_BASE {
            return Err("efault");
        }
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
            .filter(|pte| pte.cow && pte.page.sharers() > 1)
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

    // AGENT: validate region endpoints before deriving page ranges or allocating frames.
    pub fn map_region(&mut self, region: VmRegion, pool: &FramePool) -> Result<(), &'static str> {
        if region.base % PAGE_SZ != 0 || region.len % PAGE_SZ != 0 {
            return Err("einval");
        }
        let region_end = region.checked_end().ok_or("einval")?;
        if region_end > KERN_BASE {
            return Err("einval");
        }
        let flags = region.flags;
        let pages: Vec<usize> = page_range(region.base, region.len).collect();
        let mut allocated = Vec::with_capacity(pages.len());
        for _ in pages.iter() {
            match pool.alloc_pg_frame() {
                Some(frame) => allocated.push(frame),
                None => {
                    return Err("enomem");
                }
            }
        }
        if let Err(err) = self.vm_map.insert(region) {
            return Err(err);
        }
        let mut pt = self.page_table.lock().unwrap();
        for (page_addr, frame) in pages.into_iter().zip(allocated.into_iter()) {
            pt.insert(page_addr, PageTableEntry::new(frame, flags));
        }
        Ok(())
    }

    // AGENT: create resident file-backed mmap pages, preserving private snapshots,
    // shared writeback metadata, and checked VM/file offsets for each page.
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
        let region_end = region.checked_end().ok_or("einval")?;
        if region_end > KERN_BASE {
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
            match pool.alloc_pg_frame() {
                Some(frame) => allocated.push(frame),
                None => {
                    return Err("enomem");
                }
            }
        }

        let file_snapshot = file_data.lock().unwrap().clone();
        if let Err(err) = self.vm_map.insert(region) {
            return Err(err);
        }

        let mut pt = self.page_table.lock().unwrap();
        for ((page_addr, frame), file_offset) in pages
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
            let pte = PageTableEntry::with_backing(SharedPage::new(frame), flags, backing);
            if valid_len > 0 {
                let page_data = pte.page.data();
                page_data.lock().unwrap()[..valid_len]
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

// AGENT: keep page iteration panic-free; callers still validate ranges before use.
fn page_range(base: usize, len: usize) -> impl Iterator<Item = usize> {
    let start = base & !(PAGE_SZ - 1);
    let end = match base
        .checked_add(len)
        .and_then(|end| end.checked_add(PAGE_SZ - 1))
    {
        Some(end) => end & !(PAGE_SZ - 1),
        None => start,
    };
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
