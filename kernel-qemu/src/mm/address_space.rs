// AGENT
use super::*;

// AGENT: QEMU PTE metadata keeps current hardware leaf state while VmRegion
// remains the single source of VM flags.
pub struct PageTableEntry {
    pub frame: SharedPage,
    pub pte_flags: usize,
    pub cow: bool,
}

impl PageTableEntry {
    // AGENT: default page-table entries are anonymous zero-filled pages.
    pub fn new(frame: PgFrame, flags: u32) -> Self {
        Self {
            frame: SharedPage::new(frame),
            pte_flags: vm_flags_to_pte_flags(flags),
            cow: false,
        }
    }

    fn as_cow(&mut self) {
        self.cow = true;
        self.pte_flags = pte_flags_without_write(self.pte_flags);
    }

    // AGENT: resolve COW frame ownership and restore write permissions from the
    // owning VmRegion flags instead of keeping a duplicate PTE-side copy.
    fn resolve_write(&mut self, flags: u32, pool: &FramePool) -> Result<usize, &'static str> {
        let paddr = self.frame.fault(pool)?;
        self.pte_flags = vm_flags_to_pte_flags(flags);
        self.cow = false;
        Ok(paddr)
    }

    // AGENT: update only hardware-facing leaf flags; VmRegion owns VM flags.
    fn set_flags(&mut self, flags: u32) {
        self.pte_flags = vm_flags_to_pte_flags(flags);
        if self.cow {
            self.pte_flags = pte_flags_without_write(self.pte_flags);
        }
    }

    // AGENT: derive current direct-write access from the Sv39 leaf flags instead
    // of storing a duplicate software boolean.
    fn is_writable(&self) -> bool {
        self.pte_flags & PTE_W != 0
    }

    pub fn frame_id(&self) -> usize {
        self.frame.frame_id()
    }

    // AGENT: clone only when a new PTE mapping should share the same frame.
    fn clone_mapping(&self) -> Self {
        Self {
            frame: self.frame.clone(),
            pte_flags: self.pte_flags,
            cow: self.cow,
        }
    }
}

// AGENT: isolate ownership of the real Sv39 page-table root and intermediate
// table frames from higher-level address-space metadata.
struct Sv39PageTable {
    root_paddr: usize,
    root_frame: Option<PgFrame>,
    table_frames: Vec<PgFrame>,
}

impl Sv39PageTable {
    // AGENT: start without a hardware root because early ProcessState creation
    // does not yet have access to the FramePool.
    fn new() -> Self {
        Self {
            root_paddr: 0,
            root_frame: None,
            table_frames: Vec::new(),
        }
    }

    // AGENT: expose the live root only after the first mapping allocates it.
    fn root_paddr(&self) -> Result<usize, &'static str> {
        if self.root_paddr == 0 {
            Err("efault")
        } else {
            Ok(self.root_paddr)
        }
    }

    // AGENT: lazily allocate the real Sv39 root on the first mapping operation.
    fn ensure_root(&mut self, pool: &FramePool) -> Result<usize, &'static str> {
        if self.root_paddr != 0 {
            return Ok(self.root_paddr);
        }
        let frame = pool.alloc_pg_frame().ok_or("enomem")?;
        zero_page(frame.paddr());
        self.root_paddr = frame.paddr();
        self.root_frame = Some(frame);
        Ok(self.root_paddr)
    }

    // AGENT: create a hardware leaf mapping while keeping intermediate table
    // frame ownership inside Sv39PageTable.
    fn map_leaf(
        &mut self,
        va: usize,
        pa: usize,
        flags: usize,
        pool: &FramePool,
    ) -> Result<(), &'static str> {
        let root = self.ensure_root(pool)?;
        map(root, va, pa, flags, pool, &mut self.table_frames)
    }

    // AGENT: update an existing hardware leaf through the owned Sv39 root.
    fn update_leaf(&self, va: usize, pa: usize, flags: usize) -> Result<(), &'static str> {
        update_leaf(self.root_paddr()?, va, pa, flags)
    }

    // AGENT: validate that resident metadata still has a matching hardware leaf
    // before mutating COW ownership.
    fn leaf_paddr(&self, va: usize) -> Result<usize, &'static str> {
        leaf_paddr(self.root_paddr()?, va)
    }

    // AGENT: keep callers simple when a not-yet-mapped address space has no root.
    fn update_leaf_if_present(
        &self,
        va: usize,
        pa: usize,
        flags: usize,
    ) -> Result<(), &'static str> {
        if self.root_paddr == 0 {
            Ok(())
        } else {
            update_leaf(self.root_paddr, va, pa, flags)
        }
    }

    // AGENT: remove a hardware leaf if this address space has already allocated
    // a real Sv39 root.
    fn unmap_leaf_if_present(&self, va: usize) {
        if self.root_paddr != 0 {
            let _ = unmap(self.root_paddr, va);
        }
    }

    // AGENT: route user memory translation through the owned Sv39 tree.
    fn translate(&self, va: usize, access: PageAccess) -> Result<usize, &'static str> {
        translate(self.root_paddr()?, va, access)
    }

    // AGENT: drop all hardware page-table frames during exec or process teardown.
    fn clear(&mut self) {
        self.table_frames.clear();
        self.root_frame = None;
        self.root_paddr = 0;
    }
}

// AGENT: store software resident-page metadata separately from the real Sv39
// page table so the BTreeMap is not mistaken for hardware page-table storage.
struct ResidentPageTable {
    entries: Mutex<BTreeMap<usize, PageTableEntry>>,
}

impl ResidentPageTable {
    // AGENT: initialize the software resident-page table independently of VmMap
    // and Sv39 root allocation.
    fn new() -> Self {
        Self {
            entries: Mutex::new(BTreeMap::new()),
        }
    }

    // AGENT: atomically detach all resident metadata before dropping pages.
    fn take_all(&self) -> BTreeMap<usize, PageTableEntry> {
        let mut entries = self.entries.lock().unwrap();
        mem::take(&mut *entries)
    }

    // AGENT: report resident page count without exposing the backing BTreeMap as
    // the address space's hardware page table.
    fn len(&self) -> usize {
        self.entries.lock().unwrap().len()
    }
}

// AGENT: coordinate VmMap, resident page metadata, and the owned Sv39 page table
// without storing page-table implementation fields directly on AddrSpace.
pub struct AddrSpace {
    pub vm_map: VmMap,
    resident_pages: ResidentPageTable,
    sv39: Sv39PageTable,
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
            sv39: Sv39PageTable::new(),
        }
    }

    // AGENT: derive the switch token from the live Sv39 root instead of storing
    // a simulator-only vm_token_id.
    pub fn vm_token(&self) -> Result<usize, &'static str> {
        self.sv39.root_paddr().map(crate::csr::make_satp_sv39)
    }

    // AGENT: fork copies VmMap separately from resident-page metadata and then
    // mirrors each resident leaf into the child's owned Sv39 page table.
    pub fn fork_from(parent: &AddrSpace, pool: &FramePool) -> Result<Self, &'static str> {
        let mut child = Self::new();
        child.vm_map.brk = parent.vm_map.brk;
        child.vm_map.mmap_base = parent.vm_map.mmap_base;
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
        let page_addr = addr & !(PAGE_SZ - 1);
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
        let page_addr = cur & !(PAGE_SZ - 1);
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
        if (paddr & !(PAGE_SZ - 1)) != frame_paddr {
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
        let end = start.checked_add(len).ok_or("efault")?;
        let mut entries = self.resident_pages.entries.lock().unwrap();
        let pages_to_unmap: Vec<usize> = entries
            .keys()
            .filter(|&&addr| addr >= start && addr < end)
            .copied()
            .collect();
        self.vm_map.remove_range(start, len);
        for addr in &pages_to_unmap {
            self.sv39.unmap_leaf_if_present(*addr);
            let _dropped = entries.remove(addr);
        }
        crate::csr::sfence_vma();
        Ok(pages_to_unmap.len())
    }

    // AGENT: process teardown drops resident metadata before clearing the
    // separately owned Sv39 page-table frames.
    pub fn release_all_pages(&mut self, _pool: &FramePool) -> usize {
        self.vm_map.regions.clear();
        let entries = self.resident_pages.take_all();
        let mut released = 0;
        for pte in entries.into_values() {
            if pte.frame.is_unique() {
                released += 1;
            }
        }
        self.sv39.clear();
        crate::csr::sfence_vma();
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
        let mut entries = self.resident_pages.entries.lock().unwrap();
        for (addr, pte) in entries.iter_mut() {
            if *addr >= start && *addr < end {
                pte.set_flags(new_flags);
                self.sv39
                    .update_leaf_if_present(*addr, pte.frame.paddr(), pte.pte_flags)?;
            }
        }
        crate::csr::sfence_vma();
        Ok(())
    }

    // AGENT: report software-resident pages, not Sv39 intermediate table pages.
    pub fn rss_pages(&self) -> usize {
        self.resident_pages.len()
    }

    // AGENT: count COW sharing in resident page metadata rather than walking the
    // hardware page table.
    pub fn cow_sharers(&self) -> usize {
        let entries = self.resident_pages.entries.lock().unwrap();
        entries
            .values()
            .filter(|pte| pte.cow && pte.frame.sharers() > 1)
            .count()
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
        if region.base % PAGE_SZ != 0 || region.len % PAGE_SZ != 0 {
            return Err("einval");
        }
        let region_end = region.checked_end().ok_or("einval")?;
        if region_end > KERN_BASE {
            return Err("einval");
        }
        let flags = region.flags;
        let region_base = region.base;
        let region_len = region.len;
        let pages: Vec<usize> = page_range(region.base, region.len).collect();
        let mut allocated = Vec::with_capacity(pages.len());
        for _ in pages.iter() {
            match pool.alloc_pg_frame() {
                Some(frame) => {
                    zero_page(frame.paddr());
                    allocated.push(frame);
                }
                None => {
                    return Err("enomem");
                }
            }
        }
        if let Err(err) = self.vm_map.insert(region) {
            return Err(err);
        }
        let mut mapped: Vec<(usize, PgFrame)> = Vec::with_capacity(pages.len());
        for (page_addr, frame) in pages.into_iter().zip(allocated.into_iter()) {
            let pte_flags = vm_flags_to_pte_flags(flags);
            if let Err(err) = self
                .sv39
                .map_leaf(page_addr, frame.paddr(), pte_flags, pool)
            {
                for (mapped_addr, _) in mapped.iter() {
                    self.sv39.unmap_leaf_if_present(*mapped_addr);
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

// AGENT: translate migrated VM flags into Sv39 user leaf permissions.
fn vm_flags_to_pte_flags(flags: u32) -> usize {
    let mut pte_flags = PTE_U | PTE_A;
    if flags & VM_READ != 0 {
        pte_flags |= PTE_R;
    }
    if flags & VM_WRITE != 0 {
        pte_flags |= PTE_W | PTE_D;
    }
    if flags & VM_EXEC != 0 {
        pte_flags |= PTE_X;
    }
    pte_flags
}

// AGENT: strip write/dirty bits when software COW owns the next write fault.
fn pte_flags_without_write(flags: usize) -> usize {
    flags & !(PTE_W | PTE_D)
}
