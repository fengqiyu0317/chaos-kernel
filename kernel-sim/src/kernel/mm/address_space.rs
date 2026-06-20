// AGENT
use super::*;

#[derive(Clone)]
pub struct PageTableEntry {
    pub frame_id: usize,
    pub frame: PgFrame,
    pub flags: u32,
    pub writable: bool,
    pub cow: bool,
    pub present: bool,
}

impl PageTableEntry {
    pub fn new(frame_id: usize, frame: PgFrame, flags: u32) -> Self {
        Self {
            frame_id,
            frame,
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

    fn resolve_write(&mut self, frame_id: usize, frame: PgFrame) {
        self.frame_id = frame_id;
        self.frame = frame;
        self.writable = self.flags & VM_WRITE != 0;
        self.cow = false;
        self.present = true;
    }

    fn set_flags(&mut self, flags: u32) {
        self.flags = flags;
        self.writable = flags & VM_WRITE != 0 && !self.cow;
    }
}

pub struct AddrSpace {
    pub vm_map: VmMap,
    pub page_table_root: usize,
    pub asid: u16,
    pub ref_count: AtomicUsize,
    pub page_table: Mutex<BTreeMap<usize, PageTableEntry>>,
}

impl AddrSpace {
    pub fn new(asid: u16) -> Self {
        Self {
            vm_map: VmMap::new(),
            page_table_root: asid as usize,
            asid,
            ref_count: AtomicUsize::new(1),
            page_table: Mutex::new(BTreeMap::new()),
        }
    }

    pub fn fork_from(parent: &AddrSpace, new_asid: u16) -> Self {
        let mut child = Self::new(new_asid);
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

        if pte.frame.count() <= 1 {
            pte.writable = pte.flags & VM_WRITE != 0;
            pte.cow = false;
            return Ok(pte.frame_id * PAGE_SZ + MEM_OFF);
        }

        let new_frame_id = pool.get_inner().ok_or("oom")?;
        pte.frame.down();
        pte.resolve_write(new_frame_id, PgFrame::with_rc(1));
        Ok(new_frame_id * PAGE_SZ + MEM_OFF)
    }

    pub fn unmap_range(&mut self, start: usize, len: usize) -> usize {
        let end = start + len;
        self.vm_map.remove_range(start, len);
        let mut pt = self.page_table.lock().unwrap();
        let pages_to_unmap: Vec<usize> = pt
            .keys()
            .filter(|&&addr| addr >= start && addr < end)
            .copied()
            .collect();
        for addr in &pages_to_unmap {
            if let Some(pte) = pt.remove(addr) {
                pte.frame.down();
            }
        }
        pages_to_unmap.len()
    }

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
            if pte.frame.count() == 0 {
                continue;
            }
            let prev = pte.frame.down();
            if prev == 1 {
                pool.put(pte.frame_id);
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

    pub fn resize_brk(&mut self, new_brk: usize, pool: &FramePool) -> Result<(), &'static str> {
        let old_brk = self.vm_map.brk;
        if new_brk < old_brk {
            self.unmap_range(new_brk, old_brk - new_brk);
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
