// AGENT
use super::*;
// AGENT: import resident page-table metadata from its dedicated module so this
// file only coordinates address-space operations.
use super::page_table::{pte_flags_without_write, vm_flags_to_pte_flags, ResidentPageTable};
// AGENT: preserve the former address_space::PageTableEntry API after moving its
// implementation into the dedicated page_table module.
pub use super::page_table::PageTableEntry;

// AGENT: coordinate VmMap, resident page metadata, and the owned Sv39 page table
// without storing page-table implementation fields directly on AddrSpace.
pub struct AddrSpace {
    pub vm_map: VmMap,
    resident_pages: ResidentPageTable,
    sv39: PageTable,
}

// AGENT: carry the resolved state for one bounded user-memory write so the
// public copy path does not mix validation, COW, translation, and byte copying.
struct UserWriteChunk {
    paddr: usize,
    len: usize,
}

impl AddrSpace {
    // AGENT: construct an address space with VM metadata but no allocated Sv39 root.
    pub fn new() -> Self {
        Self {
            vm_map: VmMap::new(),
            resident_pages: ResidentPageTable::new(),
            sv39: PageTable::new(),
        }
    }

    // AGENT: derive the switch token from the live Sv39 root instead of storing
    // a simulator-only vm_token_id.
    pub fn vm_token(&self) -> Result<usize, &'static str> {
        self.sv39.root_paddr().map(crate::csr::make_satp_sv39)
    }

    // AGENT: export VMA metadata and resident page bytes for process-level
    // checkpoint images without exposing the internal resident page table.
    pub fn snapshot_checkpoint_memory(
        &self,
    ) -> Result<(Vec<SavedVma>, Vec<SavedPage>), &'static str> {
        let regions = self.vm_map.clone_regions();
        let mut saved_vmas = Vec::with_capacity(regions.len());
        for region in &regions {
            saved_vmas.push(SavedVma {
                start: region.base as u64,
                len: region.len as u64,
                flags: region.flags,
            });
        }

        let entries = self.resident_pages.entries.lock().unwrap();
        let mut saved_pages = Vec::with_capacity(entries.len());
        for (&vaddr, pte) in entries.iter() {
            let paddr = self.sv39.leaf_paddr(vaddr)?;
            if paddr != pte.frame.paddr() {
                return Err("efault");
            }
            let mut bytes = vec![0u8; PAGE_SZ];
            copy_from_phys(paddr, &mut bytes);
            saved_pages.push(SavedPage {
                vaddr: vaddr as u64,
                bytes,
            });
        }
        Ok((saved_vmas, saved_pages))
    }

    // AGENT: rebuild anonymous checkpoint memory by recreating VMAs, replaying
    // page bytes, then restoring final protections.
    pub fn restore_checkpoint_memory(
        brk: usize,
        vmas: &[SavedVma],
        pages: &[SavedPage],
        pool: &FramePool,
    ) -> Result<Self, &'static str> {
        let mut addr_space = Self::new();
        let mut final_regions = Vec::with_capacity(vmas.len());

        for vma in vmas {
            let start = checked_u64_to_usize(vma.start)?;
            let len = checked_u64_to_usize(vma.len)?;
            let flags = vma.flags;
            let temp_flags = flags | VM_WRITE;
            addr_space.map_region(VmRegion::new(start, len, temp_flags), pool)?;
            final_regions.push((start, len, flags));
        }

        for page in pages {
            if page.bytes.len() != PAGE_SZ {
                return Err("einval");
            }
            let vaddr = checked_u64_to_usize(page.vaddr)?;
            addr_space.write_user_bytes(vaddr, &page.bytes, pool)?;
        }

        for (start, len, flags) in final_regions {
            addr_space.protect(start, len, flags)?;
        }
        addr_space.vm_map.brk = brk;
        Ok(addr_space)
    }

    // AGENT: fork copies VmMap separately from resident-page metadata and then
    // mirrors each resident leaf into the child's owned Sv39 page table.
    pub fn fork_from(parent: &AddrSpace, pool: &FramePool) -> Result<Self, &'static str> {
        let mut child = Self::new();
        child.vm_map.brk = parent.vm_map.brk;
        for region in parent.vm_map.clone_regions() {
            if region.flags & VM_DONTCOPY != 0 {
                continue;
            }
            child.vm_map.insert(region)?;
        }

        let mut parent_leaf_changed = false;
        let mut parent_entries = parent.resident_pages.entries.lock().unwrap();
        let mut child_entries = Vec::new();
        for (&page_addr, parent_entry) in parent_entries.iter_mut() {
            let Some(region) = parent.vm_map.find(page_addr) else {
                continue;
            };
            if region.flags & VM_DONTCOPY != 0 {
                continue;
            }
            let flags = region.flags;
            if flags & VM_WRITE != 0 && flags & VM_SHARED == 0 {
                let cow_flags = pte_flags_without_write(parent_entry.pte_flags);
                if let Err(err) = parent.sv39.update_leaf_if_present(
                    page_addr,
                    parent_entry.frame.paddr(),
                    cow_flags,
                ) {
                    if parent_leaf_changed {
                        crate::csr::sfence_vma();
                    }
                    return Err(err);
                }
                parent_entry.as_cow();
                parent_leaf_changed = true;
            }
            child_entries.push((page_addr, parent_entry.clone_mapping()));
        }
        drop(parent_entries);

        for (page_addr, entry) in child_entries.iter() {
            let mapped =
                child
                    .sv39
                    .map_leaf(*page_addr, entry.frame.paddr(), entry.pte_flags, pool);
            if let Err(err) = mapped {
                if parent_leaf_changed {
                    crate::csr::sfence_vma();
                }
                return Err(err);
            }
        }
        {
            let mut child_resident = child.resident_pages.entries.lock().unwrap();
            for (page_addr, entry) in child_entries {
                child_resident.insert(page_addr, entry);
            }
        }
        if parent_leaf_changed {
            crate::csr::sfence_vma();
        }
        Ok(child)
    }

    // AGENT: COW fault resolution preflights the Sv39 leaf before mutating
    // resident metadata, then mirrors the changed leaf into the hardware table.
    pub fn handle_cow_fault(&self, addr: usize, pool: &FramePool) -> Result<usize, &'static str> {
        let page_addr = align_down(addr, PAGE_SZ);
        let region = self.vm_map.find(addr).ok_or("segfault")?;
        let flags = region.flags;
        if flags & VM_WRITE == 0 {
            return Err("segfault");
        }
        let mut entries = self.resident_pages.entries.lock().unwrap();
        let pte = entries.get_mut(&page_addr).ok_or("segfault")?;
        if pte.is_writable() && !pte.cow {
            return Ok(pte.frame.paddr());
        }
        if !pte.cow {
            return Err("segfault");
        }
        let old_paddr = pte.frame.paddr();
        if self.sv39.leaf_paddr(page_addr)? != old_paddr {
            return Err("efault");
        }

        let paddr = pte.resolve_write(flags, pool)?;
        self.sv39.update_leaf(page_addr, paddr, pte.pte_flags)?;
        crate::csr::sfence_vma();
        Ok(paddr)
    }

    // AGENT: validate user copy boundaries before touching VmMap or Sv39 state.
    fn checked_user_end(addr: usize, len: usize) -> Result<usize, &'static str> {
        let end = addr.checked_add(len).ok_or("efault")?;
        if end > KERN_BASE {
            return Err("efault");
        }
        Ok(end)
    }

    // AGENT: copy user bytes by trusting the live Sv39 page table as the read
    // authority instead of duplicating resident-page or VmMap validity checks.
    pub fn read_user_bytes(&self, addr: usize, dst: &mut [u8]) -> Result<(), &'static str> {
        if dst.is_empty() {
            return Ok(());
        }
        let end = Self::checked_user_end(addr, dst.len())?;
        let mut copied = 0usize;
        while copied < dst.len() {
            let cur = addr + copied;
            let page_off = cur & (PAGE_SZ - 1);
            let chunk = min(end - cur, PAGE_SZ - page_off);
            let paddr = self.sv39.translate(cur, PageAccess::Read)?;
            copy_from_phys(paddr, &mut dst[copied..copied + chunk]);
            copied += chunk;
        }
        Ok(())
    }

    // AGENT: read scalar user data through the unified byte-copy path.
    pub fn read_user_usize(&self, addr: usize) -> Result<usize, &'static str> {
        let mut bytes = [0u8; mem::size_of::<usize>()];
        self.read_user_bytes(addr, &mut bytes)?;
        Ok(usize::from_ne_bytes(bytes))
    }

    // AGENT: prepare one write chunk by checking VMA permissions, resolving COW
    // outside resident-page locks, and verifying metadata matches the Sv39 leaf.
    fn prepare_user_write_chunk(
        &mut self,
        cur: usize,
        end: usize,
        pool: &FramePool,
    ) -> Result<UserWriteChunk, &'static str> {
        let region = self.vm_map.find(cur).ok_or("efault")?;
        if region.flags & VM_WRITE == 0 {
            return Err("efault");
        }

        let region_end = region.end();
        let page_addr = align_down(cur, PAGE_SZ);
        let page_off = cur & (PAGE_SZ - 1);
        let len = min(end - cur, min(PAGE_SZ - page_off, region_end - cur));
        let need_cow = {
            let entries = self.resident_pages.entries.lock().unwrap();
            let pte = entries.get(&page_addr).ok_or("efault")?;
            !pte.is_writable() && pte.cow
        };
        if need_cow {
            self.handle_cow_fault(cur, pool).map_err(|_| "efault")?;
        }

        let frame_paddr = {
            let entries = self.resident_pages.entries.lock().unwrap();
            let pte = entries.get(&page_addr).ok_or("efault")?;
            if !pte.is_writable() {
                return Err("efault");
            }
            pte.frame.paddr()
        };
        let paddr = self.sv39.translate(cur, PageAccess::Write)?;
        if align_down(paddr, PAGE_SZ) != frame_paddr {
            return Err("efault");
        }
        Ok(UserWriteChunk { paddr, len })
    }

    // AGENT: user writes resolve COW through resident metadata, then translate
    // through Sv39 and copy directly into the target physical page.
    pub fn write_user_bytes(
        &mut self,
        addr: usize,
        src: &[u8],
        pool: &FramePool,
    ) -> Result<(), &'static str> {
        if src.is_empty() {
            return Ok(());
        }
        let end = Self::checked_user_end(addr, src.len())?;
        let mut written = 0usize;
        while written < src.len() {
            let cur = addr + written;
            let chunk = self.prepare_user_write_chunk(cur, end, pool)?;
            copy_to_phys(chunk.paddr, &src[written..written + chunk.len]);
            written += chunk.len;
        }
        Ok(())
    }

    // AGENT: unmapping removes resident metadata and Sv39 leaves; file-backed
    // writeback is intentionally not implemented in kernel-qemu yet.
    pub fn unmap_range(
        &mut self,
        start: usize,
        len: usize,
        _pool: &FramePool,
    ) -> Result<usize, &'static str> {
        if len == 0 || start % PAGE_SZ != 0 || len % PAGE_SZ != 0 {
            return Err("einval");
        }
        let end = start.checked_add(len).ok_or("efault")?;
        if end > KERN_BASE {
            return Err("efault");
        }
        let mut entries = self.resident_pages.entries.lock().unwrap();
        let pages_to_unmap: Vec<(usize, usize)> = entries
            .iter()
            .filter_map(|(&addr, pte)| {
                (addr >= start && addr < end).then(|| (addr, pte.frame.paddr()))
            })
            .collect();
        for &(addr, paddr) in &pages_to_unmap {
            if self.sv39.leaf_paddr(addr)? != paddr {
                return Err("efault");
            }
        }
        self.vm_map.remove_range(start, len);
        for &(addr, _) in &pages_to_unmap {
            self.sv39.unmap_leaf_if_present(addr)?;
            let _dropped = entries.remove(&addr);
        }
        crate::csr::sfence_vma();
        Ok(pages_to_unmap.len())
    }

    // AGENT: process teardown removes hardware leaves before resident frames can
    // drop, then releases the now-inactive Sv39 page-table frames.
    pub fn release_all_pages(&mut self, _pool: &FramePool) -> usize {
        self.vm_map.regions.clear();

        let released = {
            let entries = self.resident_pages.entries.lock().unwrap();
            let mut released = 0;
            for (&addr, pte) in entries.iter() {
                if pte.frame.is_unique() {
                    released += 1;
                }
                let paddr = self
                    .sv39
                    .leaf_paddr(addr)
                    .expect("resident page should have an Sv39 leaf");
                assert_eq!(
                    paddr,
                    pte.frame.paddr(),
                    "resident page and Sv39 leaf disagree"
                );
                self.sv39
                    .unmap_leaf_if_present(addr)
                    .expect("resident Sv39 leaf should unmap");
            }
            released
        };

        crate::csr::sfence_vma();
        self.sv39.deactivate_if_current();
        drop(self.resident_pages.take_all());
        self.sv39.clear();
        crate::csr::sfence_vma();
        released
    }

    // AGENT: split VmMap metadata only when a protection boundary falls inside a
    // mapped region; resident pages and Sv39 leaves stay page-granular.
    fn split_protection_boundary(&mut self, addr: usize) -> Result<(), &'static str> {
        let Some(idx) = self
            .vm_map
            .regions
            .iter()
            .position(|region| region.contains(addr))
        else {
            return Ok(());
        };
        if self.vm_map.regions[idx].base == addr {
            return Ok(());
        }
        let (left, right) = self.vm_map.regions[idx].split_at(addr).ok_or("einval")?;
        self.vm_map.regions[idx] = left;
        self.vm_map.regions.insert(idx + 1, right);
        Ok(())
    }

    // AGENT: apply page-aligned protection changes to VmMap metadata and mirror
    // them into already-resident Sv39 leaves.
    pub fn protect(
        &mut self,
        start: usize,
        len: usize,
        new_flags: u32,
    ) -> Result<(), &'static str> {
        if len == 0 || start % PAGE_SZ != 0 || len % PAGE_SZ != 0 {
            return Err("einval");
        }
        let end = start.checked_add(len).ok_or("efault")?;
        if end > KERN_BASE {
            return Err("efault");
        }

        let mut covered = start;
        while covered < end {
            let region = self.vm_map.find(covered).ok_or("efault")?;
            let region_end = min(region.end(), end);
            if region_end <= covered {
                return Err("efault");
            }
            covered = region_end;
        }

        {
            let entries = self.resident_pages.entries.lock().unwrap();
            for (&addr, pte) in entries.iter() {
                if addr >= start && addr < end && self.sv39.leaf_paddr(addr)? != pte.frame.paddr() {
                    return Err("efault");
                }
            }
        }

        self.split_protection_boundary(end)?;
        self.split_protection_boundary(start)?;

        let prot_mask = VM_READ | VM_WRITE | VM_EXEC;
        let requested_prot = new_flags & prot_mask;
        for region in self.vm_map.regions.iter_mut() {
            if region.base >= start && region.end() <= end {
                region.flags = (region.flags & !prot_mask) | requested_prot;
            }
        }

        let mut entries = self.resident_pages.entries.lock().unwrap();
        for (addr, pte) in entries.iter_mut() {
            if *addr >= start && *addr < end {
                let flags = self.vm_map.find(*addr).ok_or("efault")?.flags;
                pte.set_flags(flags);
                self.sv39
                    .update_leaf_if_present(*addr, pte.frame.paddr(), pte.pte_flags)?;
            }
        }
        crate::csr::sfence_vma();
        Ok(())
    }

    // AGENT: split only VmMap region metadata; resident pages and Sv39 leaves are
    // already page-granular and stay unchanged.
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

    // AGENT: validate VmMap metadata, allocate resident frames, and install Sv39
    // leaves through the split page-table owner.
    pub fn map_region(&mut self, region: VmRegion, pool: &FramePool) -> Result<(), &'static str> {
        if region.len == 0 || region.base % PAGE_SZ != 0 || region.len % PAGE_SZ != 0 {
            return Err("einval");
        }
        let region_end = region.checked_end().ok_or("einval")?;
        if region_end > KERN_BASE {
            return Err("einval");
        }

        let flags = region.flags;
        let region_base = region.base;
        let region_len = region.len;
        let pte_flags = vm_flags_to_pte_flags(flags);
        let pages: Vec<usize> = page_range(region_base, region_len).collect();

        let mut frames = Vec::with_capacity(pages.len());
        for _ in 0..pages.len() {
            let frame = pool.alloc_pg_frame().ok_or("enomem")?;
            zero_page(frame.paddr());
            frames.push(frame);
        }

        if let Err(err) = self.vm_map.insert(region) {
            return Err(err);
        }

        let mut mapped = Vec::with_capacity(pages.len());
        for (page_addr, frame) in pages.into_iter().zip(frames.into_iter()) {
            if let Err(err) = self
                .sv39
                .map_leaf(page_addr, frame.paddr(), pte_flags, pool)
            {
                for (mapped_addr, _) in mapped.iter() {
                    let _ = self.sv39.unmap_leaf_if_present(*mapped_addr);
                }
                self.vm_map.remove_range(region_base, region_len);
                return Err(err);
            }
            mapped.push((page_addr, frame));
        }

        let mut entries = self.resident_pages.entries.lock().unwrap();
        for (page_addr, frame) in mapped.into_iter() {
            entries.insert(page_addr, PageTableEntry::new(frame, flags));
        }
        crate::csr::sfence_vma();
        Ok(())
    }

    // AGENT: map an existing shared segment into this address space without
    // allocating anonymous frames or turning writable pages into COW mappings.
    pub fn map_shared_pages(
        &mut self,
        mut region: VmRegion,
        shared_pages: &[SharedPage],
        pool: &FramePool,
    ) -> Result<(), &'static str> {
        if region.len == 0 || region.base % PAGE_SZ != 0 || region.len % PAGE_SZ != 0 {
            return Err("einval");
        }
        if region.checked_end().ok_or("einval")? > KERN_BASE {
            return Err("einval");
        }
        if shared_pages.len() != region.len / PAGE_SZ {
            return Err("einval");
        }

        region.flags |= VM_SHARED;
        let flags = region.flags;
        let region_base = region.base;
        let region_len = region.len;
        let pte_flags = vm_flags_to_pte_flags(flags);
        let pages: Vec<usize> = page_range(region_base, region_len).collect();

        if let Err(err) = self.vm_map.insert(region) {
            return Err(err);
        }

        let mut mapped = Vec::with_capacity(shared_pages.len());
        for (page_addr, page) in pages.into_iter().zip(shared_pages.iter()) {
            if let Err(err) = self.sv39.map_leaf(page_addr, page.paddr(), pte_flags, pool) {
                for (mapped_addr, _) in mapped.iter() {
                    let _ = self.sv39.unmap_leaf_if_present(*mapped_addr);
                }
                self.vm_map.remove_range(region_base, region_len);
                return Err(err);
            }
            mapped.push((page_addr, page.clone()));
        }

        let mut entries = self.resident_pages.entries.lock().unwrap();
        for (page_addr, page) in mapped.into_iter() {
            entries.insert(page_addr, PageTableEntry::from_shared(page, flags));
        }
        crate::csr::sfence_vma();
        Ok(())
    }

    // AGENT: resize heap through the public mapping helpers so VmMap, resident
    // metadata, and Sv39 leaves stay synchronized.
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
    let start = align_down(base, PAGE_SZ);
    let end = match base
        .checked_add(len)
        .and_then(|end| checked_align_up(end, PAGE_SZ))
    {
        Some(end) => end,
        None => start,
    };
    (start..end).step_by(PAGE_SZ)
}

// AGENT: convert serialized addresses into the current machine word size before
// they are used to allocate or write restored memory.
fn checked_u64_to_usize(value: u64) -> Result<usize, &'static str> {
    usize::try_from(value).map_err(|_| "einval")
}
