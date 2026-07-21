// AGENT
use super::*;
// AGENT: import resident page-table metadata from its dedicated module so this
// file only coordinates address-space operations.
use super::page_table::{pte_flags_without_write, vm_flags_to_pte_flags, ResidentPages};

// AGENT: keep the empty-image program break shared by construction and teardown
// so a released address space returns to the same metadata state as a new one.
const INITIAL_BRK: usize = 0x0040_0000;

const TRAMPOLINE_FLAGS: usize = PTE_R | PTE_X | PTE_A;
const TRAP_CONTEXT_FLAGS: usize = PTE_R | PTE_W | PTE_A | PTE_D;

// AGENT: track supervisor-only leaves separately from VmMap/resident_pages so
// trap transport does not become user-visible VMA, COW, or checkpoint state.
struct ArchMappings {
    trampoline_paddr: Option<usize>,
    trap_context_paddr: Option<usize>,
}

// AGENT: centralize the two architecture-leaf invariants used by binding,
// reverse page-table validation, and address-space teardown.
impl ArchMappings {
    // AGENT: start without architecture leaves so ordinary semantic-only
    // address-space construction remains allocation-free.
    const fn new() -> Self {
        Self {
            trampoline_paddr: None,
            trap_context_paddr: None,
        }
    }

    // AGENT: resolve only the two reserved supervisor virtual addresses to the
    // physical identity and exact permissions owned by this architecture state.
    fn expected_leaf(&self, vaddr: usize) -> Option<(usize, usize)> {
        match vaddr {
            TRAMPOLINE => self.trampoline_paddr.map(|paddr| (paddr, TRAMPOLINE_FLAGS)),
            TRAP_CONTEXT => self
                .trap_context_paddr
                .map(|paddr| (paddr, TRAP_CONTEXT_FLAGS)),
            _ => None,
        }
    }

    // AGENT: include installed architecture leaves in the reverse Sv39 owner
    // count without treating absent lazy mappings as resident pages.
    fn leaf_count(&self) -> usize {
        usize::from(self.trampoline_paddr.is_some())
            + usize::from(self.trap_context_paddr.is_some())
    }
}

// AGENT: own address-space-wide layout metadata and coordinate VmMap, resident
// page metadata, architecture leaves, and the Sv39 page table at their shared
// consistency boundary.
pub struct AddrSpace {
    vm_map: VmMap,
    brk: usize,
    resident_pages: ResidentPages,
    sv39: PageTable,
    arch: ArchMappings,
}

// AGENT: carry the resolved state for one bounded user-memory write so the
// public copy path does not mix validation, COW, translation, and byte copying.
struct UserWriteChunk {
    paddr: usize,
    len: usize,
}

// AGENT: stage one protection transition with a live-leaf rollback snapshot so
// every fallible hardware update completes before VMA policy is committed.
struct LeafFlagUpdate {
    vaddr: usize,
    paddr: usize,
    old_flags: usize,
    new_flags: usize,
}

impl AddrSpace {
    // AGENT: construct an address space with its initial program break, empty VMA
    // metadata, and no allocated Sv39 root.
    pub fn new() -> Self {
        Self {
            vm_map: VmMap::new(),
            brk: INITIAL_BRK,
            resident_pages: ResidentPages::new(),
            sv39: PageTable::new(),
            arch: ArchMappings::new(),
        }
    }

    // AGENT: derive the switch token from the live Sv39 root instead of storing
    // a simulator-only vm_token_id.
    pub fn vm_token(&self) -> Result<usize, &'static str> {
        self.sv39.root_paddr().map(crate::csr::make_satp_sv39)
    }

    // AGENT: lazily install the shared trampoline and rebind CPU0's fixed trap
    // alias to the selected task stack while the kernel page table is active.
    pub(crate) fn bind_cpu0_user_trap(
        &mut self,
        trampoline_paddr: usize,
        trap_context_paddr: usize,
        pool: &FramePool,
    ) -> Result<usize, &'static str> {
        match self.arch.trampoline_paddr {
            Some(existing) if existing != trampoline_paddr => return Err("etrampoline"),
            Some(_) => {}
            None => {
                self.sv39
                    .map_leaf(TRAMPOLINE, trampoline_paddr, TRAMPOLINE_FLAGS, pool)?;
                self.arch.trampoline_paddr = Some(trampoline_paddr);
            }
        }

        match self.arch.trap_context_paddr {
            Some(existing) if existing == trap_context_paddr => {}
            Some(_) => {
                self.sv39
                    .update_leaf(TRAP_CONTEXT, trap_context_paddr, TRAP_CONTEXT_FLAGS)?;
                self.arch.trap_context_paddr = Some(trap_context_paddr);
            }
            None => {
                self.sv39
                    .map_leaf(TRAP_CONTEXT, trap_context_paddr, TRAP_CONTEXT_FLAGS, pool)?;
                self.arch.trap_context_paddr = Some(trap_context_paddr);
            }
        }

        self.check_page_table_consistency()?;
        crate::csr::sfence_vma();
        self.vm_token()
    }

    // AGENT: expose VMA lookup without allowing callers to mutate policy behind
    // the resident/Sv39 transaction boundary.
    pub fn mapped_region(&self, addr: usize) -> Option<&VmRegion> {
        self.vm_map.find(addr)
    }

    // AGENT: expose free-range search as a read-only AddrSpace operation.
    pub fn find_free_region(&self, len: usize, align: usize) -> Option<usize> {
        self.vm_map.find_free(len, align)
    }

    // AGENT: expose the current page-aligned program break owned by AddrSpace.
    pub fn brk(&self) -> usize {
        self.brk
    }

    // AGENT: initialize or restore brk metadata after its image mappings have
    // already been built through transactional AddrSpace helpers.
    pub(crate) fn set_brk_metadata(&mut self, brk: usize) -> Result<(), &'static str> {
        if brk % PAGE_SZ != 0 || brk > USER_TOP {
            return Err("einval");
        }
        self.brk = brk;
        Ok(())
    }

    // AGENT: enforce the AddrSpace invariant in both directions without
    // allocating a leaf snapshot, keeping the audit safe on teardown paths.
    pub(crate) fn check_page_table_consistency(&self) -> Result<(), &'static str> {
        for (&vaddr, entry) in &self.resident_pages.entries {
            let region = self.vm_map.find(vaddr).ok_or("resident page outside VMA")?;
            let mut expected_flags = vm_flags_to_pte_flags(region.flags);
            if entry.cow {
                expected_flags = pte_flags_without_write(expected_flags);
            }

            let leaf = self
                .sv39
                .leaf_mapping(vaddr)
                .map_err(|_| "resident page missing Sv39 leaf")?;
            if leaf.paddr != entry.frame.paddr() {
                return Err("resident and Sv39 physical pages disagree");
            }
            if leaf.flags != expected_flags {
                return Err("Sv39 flags disagree with VMA/COW policy");
            }
            if leaf.flags & (PTE_A | PTE_D) != PTE_A | PTE_D {
                return Err("Sv39 leaf has unset accessed/dirty state");
            }
            if entry.cow && leaf.flags & PTE_W != 0 {
                return Err("writable COW Sv39 leaf");
            }
        }

        let leaf_count = self.sv39.for_each_leaf(|leaf| {
            if self.resident_pages.entries.contains_key(&leaf.vaddr) {
                return Ok(());
            }
            let Some((expected_paddr, expected_flags)) = self.arch.expected_leaf(leaf.vaddr) else {
                return Err("Sv39 leaf missing resident or architecture owner");
            };
            if leaf.paddr != expected_paddr || leaf.flags != expected_flags {
                return Err("Sv39 architecture leaf disagrees with trap mapping");
            }
            Ok(())
        })?;
        if leaf_count != self.resident_pages.entries.len() + self.arch.leaf_count() {
            return Err("resident and Sv39 leaf counts disagree");
        }
        Ok(())
    }

    // AGENT: keep the exhaustive resident/Sv39 audit on development paths while
    // compiling it out of release hot paths that already use local preflight and
    // transactional rollback for the pages they mutate.
    fn debug_check_page_table_consistency(&self) -> Result<(), &'static str> {
        #[cfg(debug_assertions)]
        {
            self.check_page_table_consistency()?;
        }
        Ok(())
    }

    // AGENT: export VMA metadata and resident page bytes for process-level
    // checkpoint images without exposing the internal resident page table.
    pub fn snapshot_checkpoint_memory(
        &self,
    ) -> Result<(Vec<SavedVma>, Vec<SavedPage>), &'static str> {
        self.check_page_table_consistency()?;
        let regions = self.vm_map.clone_regions();
        let mut saved_vmas = Vec::with_capacity(regions.len());
        for region in &regions {
            saved_vmas.push(SavedVma {
                start: region.base as u64,
                len: region.len as u64,
                flags: region.flags,
            });
        }

        let mut saved_pages = Vec::with_capacity(self.resident_pages.entries.len());
        for (&vaddr, pte) in &self.resident_pages.entries {
            let paddr = pte.frame.paddr();
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
        addr_space.set_brk_metadata(brk)?;
        Ok(addr_space)
    }

    // AGENT: fork exclusively updates parent COW metadata and Sv39 leaves, with
    // exhaustive whole-address-space validation retained for debug builds.
    pub fn fork_from(parent: &mut AddrSpace, pool: &FramePool) -> Result<Self, &'static str> {
        parent.debug_check_page_table_consistency()?;
        let mut child = Self::new();
        child.brk = parent.brk;
        for region in parent.vm_map.clone_regions() {
            child.vm_map.insert(region)?;
        }

        let mut parent_leaf_changed = false;
        let mut child_entries = Vec::new();
        for (&page_addr, parent_entry) in parent.resident_pages.entries.iter_mut() {
            let Some(region) = parent.vm_map.find(page_addr) else {
                continue;
            };
            let flags = region.flags;
            let parent_leaf = parent.sv39.leaf_mapping(page_addr)?;
            if parent_leaf.paddr != parent_entry.frame.paddr() {
                return Err("resident and Sv39 physical pages disagree");
            }
            let mut child_leaf_flags = parent_leaf.flags;
            if flags & VM_WRITE != 0 && flags & VM_SHARED == 0 {
                let cow_flags = pte_flags_without_write(parent_leaf.flags);
                if let Err(err) =
                    parent
                        .sv39
                        .update_leaf(page_addr, parent_entry.frame.paddr(), cow_flags)
                {
                    if parent_leaf_changed {
                        crate::csr::sfence_vma();
                    }
                    return Err(err);
                }
                parent_entry.as_cow();
                child_leaf_flags = cow_flags;
                parent_leaf_changed = true;
            }
            child_entries.push((page_addr, parent_entry.clone(), child_leaf_flags));
        }

        for (page_addr, entry, leaf_flags) in child_entries.iter() {
            let mapped = child
                .sv39
                .map_leaf(*page_addr, entry.frame.paddr(), *leaf_flags, pool);
            if let Err(err) = mapped {
                if parent_leaf_changed {
                    crate::csr::sfence_vma();
                }
                return Err(err);
            }
        }
        for (page_addr, entry, _) in child_entries {
            child.resident_pages.entries.insert(page_addr, entry);
        }
        if parent_leaf_changed {
            crate::csr::sfence_vma();
        }
        parent.debug_check_page_table_consistency()?;
        child.debug_check_page_table_consistency()?;
        Ok(child)
    }

    // AGENT: reject non-COW faults, then stage COW state, update Sv39, commit
    // resident ownership, flush stale translations, and only then allow the old
    // frame owner to drop.
    pub fn handle_cow_fault(
        &mut self,
        addr: usize,
        pool: &FramePool,
    ) -> Result<usize, &'static str> {
        self.debug_check_page_table_consistency()?;
        let page_addr = align_down(addr, PAGE_SZ);
        let region = self.vm_map.find(addr).ok_or("segfault")?;
        let flags = region.flags;
        if flags & VM_WRITE == 0 {
            return Err("segfault");
        }
        let (old_paddr, is_cow) = self
            .resident_pages
            .entries
            .get(&page_addr)
            .map(|page| (page.frame.paddr(), page.cow))
            .ok_or("segfault")?;
        let old_leaf = self.sv39.leaf_mapping(page_addr)?;
        if old_leaf.paddr != old_paddr {
            return Err("efault");
        }
        if !is_cow {
            return Err("segfault");
        }
        if old_leaf.flags & PTE_W != 0 {
            return Err("efault");
        }

        let replacement = self
            .resident_pages
            .entries
            .get(&page_addr)
            .expect("staged COW resident page should remain present")
            .prepare_resolved_write(pool)?;
        let paddr = replacement.frame.paddr();
        self.sv39
            .update_leaf(page_addr, paddr, vm_flags_to_pte_flags(flags))?;
        let pte = self
            .resident_pages
            .entries
            .get_mut(&page_addr)
            .expect("staged COW resident page should remain present");
        let old_entry = mem::replace(pte, replacement);
        crate::csr::sfence_vma();
        drop(old_entry);
        self.debug_check_page_table_consistency()?;
        Ok(paddr)
    }

    // AGENT: validate user copy boundaries before touching VmMap or Sv39 state.
    fn checked_user_end(addr: usize, len: usize) -> Result<usize, &'static str> {
        let end = addr.checked_add(len).ok_or("efault")?;
        if end > USER_TOP {
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

    // AGENT: prepare one write chunk under the exclusive AddrSpace borrow,
    // resolving COW before verifying metadata against the live Sv39 leaf.
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
        let (is_cow, resident_paddr) = self
            .resident_pages
            .entries
            .get(&page_addr)
            .map(|pte| (pte.cow, pte.frame.paddr()))
            .ok_or("efault")?;
        let frame_paddr = if is_cow {
            self.handle_cow_fault(cur, pool).map_err(|_| "efault")?
        } else {
            resident_paddr
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

    // AGENT: preflight only resident pages inside the requested BTreeMap range,
    // unmap hardware leaves transactionally, flush stale translations, then drop
    // resident frame ownership and VMA metadata.
    pub fn unmap_range(
        &mut self,
        start: usize,
        len: usize,
        pool: &FramePool,
    ) -> Result<usize, &'static str> {
        if len == 0 || start % PAGE_SZ != 0 || len % PAGE_SZ != 0 {
            return Err("einval");
        }
        let end = start.checked_add(len).ok_or("efault")?;
        if end > USER_TOP {
            return Err("efault");
        }
        self.debug_check_page_table_consistency()?;
        let mut pages_to_unmap = Vec::new();
        for (&addr, page) in self.resident_pages.entries.range(start..end) {
            let leaf = self.sv39.leaf_mapping(addr)?;
            if leaf.paddr != page.frame.paddr() {
                return Err("resident and Sv39 physical pages disagree");
            }
            pages_to_unmap.push((addr, leaf.paddr, leaf.flags));
        }

        let mut unmapped = 0usize;
        for &(addr, _, _) in &pages_to_unmap {
            if let Err(err) = self.sv39.unmap_leaf(addr) {
                for &(rollback_addr, rollback_paddr, rollback_flags) in
                    pages_to_unmap[..unmapped].iter().rev()
                {
                    self.sv39
                        .map_leaf(rollback_addr, rollback_paddr, rollback_flags, pool)
                        .expect("preflighted Sv39 unmap rollback should succeed");
                }
                if unmapped != 0 {
                    crate::csr::sfence_vma();
                }
                return Err(err);
            }
            unmapped += 1;
        }
        crate::csr::sfence_vma();

        self.vm_map.remove_range(start, len);
        for &(addr, _, _) in &pages_to_unmap {
            let _dropped = self.resident_pages.entries.remove(&addr);
        }
        Ok(pages_to_unmap.len())
    }

    // AGENT: process teardown removes hardware leaves before resident frames
    // drop through RAII, then releases Sv39 frames and dynamic metadata backing
    // allocations without reporting an ambiguous aliased-page count.
    pub fn release_all_pages(&mut self) {
        self.check_page_table_consistency()
            .expect("address space should be consistent before release");

        for &addr in self.resident_pages.entries.keys() {
            self.sv39
                .unmap_leaf(addr)
                .expect("resident Sv39 leaf should unmap");
        }

        crate::csr::sfence_vma();
        self.sv39.deactivate_if_current();
        drop(self.resident_pages.take_all());
        self.sv39.clear();
        self.arch = ArchMappings::new();
        self.vm_map.regions = Vec::new();
        self.brk = INITIAL_BRK;
        crate::csr::sfence_vma();
    }

    // AGENT: snapshot live leaf flags for rollback, apply every Sv39 protection
    // change, then commit the matching VMA policy.
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
        if end > USER_TOP {
            return Err("efault");
        }

        let mut covered = start;
        while covered < end {
            let region = self.vm_map.find(covered).ok_or("efault")?;
            let region_end = min(region.end(), end);
            covered = region_end;
        }

        self.debug_check_page_table_consistency()?;
        let prot_mask = VM_READ | VM_WRITE | VM_EXEC;
        let requested_prot = new_flags & prot_mask;
        let mut updates = Vec::new();
        for (&vaddr, pte) in &self.resident_pages.entries {
            if vaddr >= start && vaddr < end {
                let region = self.vm_map.find(vaddr).ok_or("efault")?;
                let leaf = self.sv39.leaf_mapping(vaddr)?;
                if leaf.paddr != pte.frame.paddr() {
                    return Err("resident and Sv39 physical pages disagree");
                }
                let flags = (region.flags & !prot_mask) | requested_prot;
                let mut new_pte_flags = vm_flags_to_pte_flags(flags);
                if pte.cow {
                    new_pte_flags = pte_flags_without_write(new_pte_flags);
                }
                updates.push(LeafFlagUpdate {
                    vaddr,
                    paddr: leaf.paddr,
                    old_flags: leaf.flags,
                    new_flags: new_pte_flags,
                });
            }
        }

        let old_regions = self.vm_map.clone_regions();
        if let Err(err) = self.vm_map.split_at_boundary(end) {
            self.vm_map.regions = old_regions;
            return Err(err);
        }
        if let Err(err) = self.vm_map.split_at_boundary(start) {
            self.vm_map.regions = old_regions;
            return Err(err);
        }

        let mut applied = 0usize;
        for update in &updates {
            if let Err(err) = self
                .sv39
                .update_leaf(update.vaddr, update.paddr, update.new_flags)
            {
                for rollback in updates[..applied].iter().rev() {
                    self.sv39
                        .update_leaf(rollback.vaddr, rollback.paddr, rollback.old_flags)
                        .expect("preflighted Sv39 protection rollback should succeed");
                }
                self.vm_map.regions = old_regions;
                if applied != 0 {
                    crate::csr::sfence_vma();
                }
                return Err(err);
            }
            applied += 1;
        }

        for region in self.vm_map.regions.iter_mut() {
            if region.base >= start && region.end() <= end {
                region.flags = (region.flags & !prot_mask) | requested_prot;
            }
        }
        crate::csr::sfence_vma();
        Ok(())
    }

    // AGENT: validate VmMap metadata, allocate resident frames, and install Sv39
    // leaves through the split page-table owner.
    pub fn map_region(&mut self, region: VmRegion, pool: &FramePool) -> Result<(), &'static str> {
        if region.len == 0 || region.base % PAGE_SZ != 0 || region.len % PAGE_SZ != 0 {
            return Err("einval");
        }
        let region_end = region.checked_end().ok_or("einval")?;
        if region_end > USER_TOP {
            return Err("einval");
        }
        self.debug_check_page_table_consistency()?;

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
                    self.sv39
                        .unmap_leaf(*mapped_addr)
                        .expect("new anonymous Sv39 leaf rollback should succeed");
                }
                self.vm_map.remove_range(region_base, region_len);
                if !mapped.is_empty() {
                    crate::csr::sfence_vma();
                }
                return Err(err);
            }
            mapped.push((page_addr, frame));
        }

        for (page_addr, frame) in mapped.into_iter() {
            self.resident_pages
                .entries
                .insert(page_addr, SharedPage::new(frame));
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
        if region.checked_end().ok_or("einval")? > USER_TOP {
            return Err("einval");
        }
        if shared_pages.len() != region.len / PAGE_SZ {
            return Err("einval");
        }
        self.debug_check_page_table_consistency()?;

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
                    self.sv39
                        .unmap_leaf(*mapped_addr)
                        .expect("new shared Sv39 leaf rollback should succeed");
                }
                self.vm_map.remove_range(region_base, region_len);
                if !mapped.is_empty() {
                    crate::csr::sfence_vma();
                }
                return Err(err);
            }
            mapped.push((page_addr, page.clone()));
        }

        for (page_addr, page) in mapped.into_iter() {
            debug_assert!(!page.cow);
            self.resident_pages.entries.insert(page_addr, page);
        }
        crate::csr::sfence_vma();
        Ok(())
    }

    // AGENT: resize heap through the public mapping helpers so VmMap, resident
    // metadata, and Sv39 leaves stay synchronized.
    pub fn resize_brk(&mut self, new_brk: usize, pool: &FramePool) -> Result<(), &'static str> {
        let old_brk = self.brk;
        if new_brk < old_brk {
            self.unmap_range(new_brk, old_brk - new_brk, pool)?;
        } else if new_brk > old_brk {
            let heap = VmRegion::new(old_brk, new_brk - old_brk, VM_READ | VM_WRITE);
            self.map_region(heap, pool)?;
        }
        self.brk = new_brk;
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
