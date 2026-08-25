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

// AGENT: keep normal raw-brk rejection distinct from address-space invariant
// failures so the syscall can return the old break without hiding kernel bugs.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BrkResizeError {
    Rejected,
    Internal(&'static str),
}

// AGENT: describe one semantic userspace access independently from the RISC-V
// trap cause so hardware faults and kernel usercopy share the same resolver.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum UserPageAccess {
    Read,
    Write,
    Execute,
}

// AGENT: keep ordinary userspace mapping/protection failures and physical-memory
// pressure distinct from address-space invariant failures at the fault boundary.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum UserPageFault {
    NotMapped,
    Protection,
    OutOfMemory,
    Internal(&'static str),
}

// AGENT: tell the trap path whether retrying the faulting instruction can make
// progress while still returning the resolved physical byte to kernel usercopy.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum UserPageResolution {
    AlreadyAccessible(usize),
    Installed(usize),
    PermissionUpdated(usize),
}

// AGENT: expose the translated physical byte uniformly to usercopy callers.
impl UserPageResolution {
    fn paddr(self) -> usize {
        match self {
            Self::AlreadyAccessible(paddr)
            | Self::Installed(paddr)
            | Self::PermissionUpdated(paddr) => paddr,
        }
    }
}

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

// AGENT: own the immutable heap lower bound and byte-granular current break in
// addition to VMA, resident-page, architecture-leaf, and Sv39 state.
pub struct AddrSpace {
    vm_map: VmMap,
    start_brk: usize,
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

// AGENT: carry one fully initialized resident owner and its exact first leaf
// permissions across fallible file I/O and transactional VMA publication.
struct PreparedResidentPage {
    vaddr: usize,
    page: SharedPage,
    leaf_flags: usize,
}

impl AddrSpace {
    // AGENT: construct an address space with matching initial break bounds,
    // empty VMA metadata, and no allocated Sv39 root.
    pub fn new() -> Self {
        Self {
            vm_map: VmMap::new(),
            start_brk: INITIAL_BRK,
            brk: INITIAL_BRK,
            resident_pages: ResidentPages::new(),
            sv39: PageTable::new(),
            arch: ArchMappings::new(),
        }
    }

    // AGENT: install the dedicated U-mode rt_sigreturn code page once, or
    // verify an inherited/restored copy before the address space enters U-mode.
    pub(crate) fn ensure_user_sigtramp(&mut self, pool: &FramePool) -> Result<(), &'static str> {
        if let Some(flags) = self.mapped_region(USER_SIGTRAMP).map(|region| region.flags) {
            if flags != (VM_READ | VM_EXEC) {
                return Err("eacces");
            }
            let mut code = [0u8; USER_SIGTRAMP_CODE.len()];
            self.read_user_bytes(USER_SIGTRAMP, &mut code, pool)?;
            return if code == USER_SIGTRAMP_CODE {
                Ok(())
            } else {
                Err("eacces")
            };
        }

        self.map_region(
            VmRegion::new(USER_SIGTRAMP, PAGE_SZ, VM_READ | VM_WRITE),
            pool,
        )?;
        if let Err(err) = self.write_user_bytes(USER_SIGTRAMP, &USER_SIGTRAMP_CODE, pool) {
            self.unmap_range(USER_SIGTRAMP, PAGE_SZ, pool)
                .expect("new signal trampoline mapping should roll back");
            return Err(err);
        }
        if let Err(err) = self.protect(USER_SIGTRAMP, PAGE_SZ, VM_READ | VM_EXEC) {
            self.unmap_range(USER_SIGTRAMP, PAGE_SZ, pool)
                .expect("new signal trampoline protection should roll back");
            return Err(err);
        }
        Ok(())
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

    // AGENT: preserve one non-fixed mmap hint through the address-space API
    // while keeping the sorted VMA collection private to this owner.
    pub fn find_free_region_from(&self, start: usize, len: usize, align: usize) -> Option<usize> {
        self.vm_map.find_free_from(start, len, align)
    }

    // AGENT: expose the byte-granular current program break owned by AddrSpace.
    pub fn brk(&self) -> usize {
        self.brk
    }

    // AGENT: expose the immutable lower bound needed by checkpoint and focused
    // syscall regressions without allowing callers to bypass resize policy.
    pub fn start_brk(&self) -> usize {
        self.start_brk
    }

    // AGENT: expose resident accounting without leaking mutable page ownership
    // so lazy-allocation tests and diagnostics can distinguish VMA from RSS.
    pub fn resident_page_count(&self) -> usize {
        self.resident_pages.entries.len()
    }

    // AGENT: report whether one page has a software owner and therefore a
    // matching Sv39 leaf under the address-space consistency invariant.
    pub fn is_page_resident(&self, addr: usize) -> bool {
        self.resident_pages
            .entries
            .contains_key(&align_down(addr, PAGE_SZ))
    }

    // AGENT: initialize or restore exact brk metadata after image mappings have
    // already been built through transactional AddrSpace helpers.
    pub(crate) fn set_brk_metadata(
        &mut self,
        start_brk: usize,
        brk: usize,
    ) -> Result<(), &'static str> {
        if start_brk > brk || brk > USER_TOP {
            return Err("einval");
        }
        self.start_brk = start_brk;
        self.brk = brk;
        Ok(())
    }

    // AGENT: enforce VMA/backing/resident/Sv39 invariants in both directions
    // without allocating a leaf snapshot, keeping teardown audits safe.
    pub(crate) fn check_page_table_consistency(&self) -> Result<(), &'static str> {
        for (&vaddr, entry) in &self.resident_pages.entries {
            let region = self.vm_map.find(vaddr).ok_or("resident page outside VMA")?;
            Self::check_resident_backing(vaddr, region, entry)?;
            let expected_flags = Self::resident_leaf_flags(region.flags, entry);

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

    // AGENT: derive hardware permissions from VMA policy plus mapping-local COW
    // and first-write tracking state, leaving sticky dirty state backing-wide.
    fn resident_leaf_flags(flags: u32, page: &SharedPage) -> usize {
        let mut leaf_flags = vm_flags_to_pte_flags(flags);
        if page.cow || page.needs_shared_write_fault() {
            leaf_flags = pte_flags_without_write(leaf_flags);
        }
        leaf_flags
    }

    // AGENT: verify each resident page retains the same anonymous/file kind,
    // inode, positioned offset, and sharing policy declared by its owning VMA.
    fn check_resident_backing(
        vaddr: usize,
        region: &VmRegion,
        page: &SharedPage,
    ) -> Result<(), &'static str> {
        match (&region.backing, &page.backing) {
            (VmBacking::Anonymous, PageBacking::Anonymous) => Ok(()),
            (
                VmBacking::File {
                    source: region_source,
                    offset: region_offset,
                },
                PageBacking::FilePrivate {
                    source,
                    offset,
                    valid_len,
                },
            ) if region.flags & VM_SHARED == 0 => {
                let displacement = vaddr.checked_sub(region.base).ok_or("efault")?;
                let expected_offset = region_offset.checked_add(displacement).ok_or("efault")?;
                if region_source.file_identity() != source.file_identity()
                    || expected_offset != *offset
                    || *valid_len > PAGE_SZ
                {
                    return Err("resident private-file backing disagrees with VMA");
                }
                Ok(())
            }
            (
                VmBacking::File {
                    source: region_source,
                    offset: region_offset,
                },
                PageBacking::FileShared(state),
            ) if region.flags & VM_SHARED != 0 => {
                let displacement = vaddr.checked_sub(region.base).ok_or("efault")?;
                let expected_offset = region_offset.checked_add(displacement).ok_or("efault")?;
                if region_source.file_identity() != state.source.file_identity()
                    || expected_offset != state.offset
                    || state.valid_len > PAGE_SZ
                {
                    return Err("resident shared-file backing disagrees with VMA");
                }
                Ok(())
            }
            _ => Err("resident backing kind disagrees with VMA"),
        }
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

    // AGENT: export only anonymous VMA/page images and reject file backing
    // explicitly until checkpoint has stable file-reopen serialization.
    pub fn snapshot_checkpoint_memory(
        &self,
    ) -> Result<(Vec<SavedVma>, Vec<SavedPage>), &'static str> {
        self.check_page_table_consistency()?;
        let regions = self.vm_map.clone_regions();
        if regions.iter().any(VmRegion::is_file_backed) {
            return Err("enotsup");
        }
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

    // AGENT: rebuild anonymous checkpoint memory and both exact break bounds by
    // recreating VMAs, replaying page bytes, then restoring final protections.
    pub fn restore_checkpoint_memory(
        start_brk: usize,
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
            let region = VmRegion::new(start, len, temp_flags);
            if flags & VM_HEAP != 0 {
                addr_space.reserve_anonymous_region(region)?;
            } else {
                addr_space.map_region(region, pool)?;
            }
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
        addr_space.set_brk_metadata(start_brk, brk)?;
        Ok(addr_space)
    }

    // AGENT: fork copies both break bounds, turns writable private pages into
    // COW, and retains writable shared-file mapping semantics.
    pub fn fork_from(parent: &mut AddrSpace, pool: &FramePool) -> Result<Self, &'static str> {
        parent.debug_check_page_table_consistency()?;
        let mut child = Self::new();
        child.start_brk = parent.start_brk;
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

    // AGENT: resolve either a private COW store or the first write to one eager
    // shared-file page while rejecting every non-writable/nonresident fault.
    pub fn handle_write_fault(
        &mut self,
        addr: usize,
        pool: &FramePool,
    ) -> Result<usize, &'static str> {
        self.debug_check_page_table_consistency()?;
        let page_addr = align_down(addr, PAGE_SZ);
        let flags = self.vm_map.find(addr).ok_or("segfault")?.flags;
        if flags & VM_WRITE == 0 {
            return Err("segfault");
        }
        let (is_cow, needs_shared_write, paddr) = self
            .resident_pages
            .entries
            .get(&page_addr)
            .map(|page| {
                (
                    page.cow,
                    page.needs_shared_write_fault(),
                    page.frame.paddr(),
                )
            })
            .ok_or("segfault")?;
        if is_cow {
            return self.handle_cow_fault(addr, pool);
        }
        if !needs_shared_write {
            return Err("segfault");
        }

        let old_leaf = self.sv39.leaf_mapping(page_addr)?;
        if old_leaf.paddr != paddr || old_leaf.flags & PTE_W != 0 {
            return Err("efault");
        }
        let page = self
            .resident_pages
            .entries
            .get_mut(&page_addr)
            .ok_or("segfault")?;
        page.enable_shared_write()?;
        let new_leaf_flags = Self::resident_leaf_flags(flags, page);
        if let Err(error) = self.sv39.update_leaf(page_addr, paddr, new_leaf_flags) {
            self.resident_pages
                .entries
                .get_mut(&page_addr)
                .expect("shared-file write fault page should remain resident")
                .shared_write_enabled = false;
            return Err(error);
        }
        crate::csr::sfence_vma();
        self.debug_check_page_table_consistency()?;
        Ok(paddr)
    }

    // AGENT: install one zero-filled anonymous heap page only after its VMA has
    // been published, committing the resident owner after the Sv39 leaf exists.
    fn install_lazy_zero_page(
        &mut self,
        page_addr: usize,
        vm_flags: u32,
        pool: &FramePool,
    ) -> Result<usize, UserPageFault> {
        self.debug_check_page_table_consistency()
            .map_err(UserPageFault::Internal)?;
        if self.resident_pages.entries.contains_key(&page_addr) {
            return Err(UserPageFault::Internal("lazy page already resident"));
        }

        let frame = pool.alloc_pg_frame().ok_or(UserPageFault::OutOfMemory)?;
        zero_page(frame.paddr());
        let page = SharedPage::new(frame);
        let paddr = page.paddr();
        let leaf_flags = Self::resident_leaf_flags(vm_flags, &page);
        self.sv39
            .map_leaf(page_addr, paddr, leaf_flags, pool)
            .map_err(|err| match err {
                "enomem" | "oom" => UserPageFault::OutOfMemory,
                internal => UserPageFault::Internal(internal),
            })?;

        let replaced = self.resident_pages.entries.insert(page_addr, page);
        assert!(
            replaced.is_none(),
            "AddrSpace lock must exclude duplicate lazy resident installation"
        );
        crate::csr::sfence_vma();
        self.debug_check_page_table_consistency()
            .map_err(UserPageFault::Internal)?;
        Ok(paddr)
    }

    // AGENT: resolve lazy anonymous allocation, private COW, and shared-file
    // first-write state through one address-space-locked transition machine.
    pub fn resolve_user_page(
        &mut self,
        addr: usize,
        access: UserPageAccess,
        pool: &FramePool,
    ) -> Result<UserPageResolution, UserPageFault> {
        if addr >= USER_TOP {
            return Err(UserPageFault::NotMapped);
        }
        self.debug_check_page_table_consistency()
            .map_err(UserPageFault::Internal)?;

        let (vm_flags, lazy_heap) = {
            let region = self.vm_map.find(addr).ok_or(UserPageFault::NotMapped)?;
            let allowed = match access {
                UserPageAccess::Read => region.flags & VM_READ != 0,
                UserPageAccess::Write => region.flags & VM_WRITE != 0,
                UserPageAccess::Execute => region.flags & VM_EXEC != 0,
            };
            if !allowed {
                return Err(UserPageFault::Protection);
            }
            (
                region.flags,
                region.flags & VM_HEAP != 0 && matches!(&region.backing, VmBacking::Anonymous),
            )
        };

        let page_addr = align_down(addr, PAGE_SZ);
        let page_offset = addr - page_addr;
        if let Some(page) = self.resident_pages.entries.get(&page_addr) {
            let needs_write_transition = page.cow || page.needs_shared_write_fault();
            if access == UserPageAccess::Write && needs_write_transition {
                let page_paddr = self
                    .handle_write_fault(addr, pool)
                    .map_err(|err| match err {
                        "enomem" | "oom" => UserPageFault::OutOfMemory,
                        internal => UserPageFault::Internal(internal),
                    })?;
                return Ok(UserPageResolution::PermissionUpdated(
                    page_paddr + page_offset,
                ));
            }

            let hardware_access = match access {
                UserPageAccess::Read => PageAccess::Read,
                UserPageAccess::Write => PageAccess::Write,
                UserPageAccess::Execute => PageAccess::Execute,
            };
            let paddr = self
                .sv39
                .translate(addr, hardware_access)
                .map_err(UserPageFault::Internal)?;
            return Ok(UserPageResolution::AlreadyAccessible(paddr));
        }

        if !lazy_heap {
            return Err(UserPageFault::Internal(
                "non-heap VMA unexpectedly lacks a resident page",
            ));
        }
        let page_paddr = self.install_lazy_zero_page(page_addr, vm_flags, pool)?;
        Ok(UserPageResolution::Installed(page_paddr + page_offset))
    }

    // AGENT: validate user copy boundaries before touching VmMap or Sv39 state.
    fn checked_user_end(addr: usize, len: usize) -> Result<usize, &'static str> {
        let end = addr.checked_add(len).ok_or("efault")?;
        if end > USER_TOP {
            return Err("efault");
        }
        Ok(end)
    }

    // AGENT: fault in lazy heap pages before copying user bytes, then use the
    // resolver's live Sv39 translation as the physical read authority.
    pub fn read_user_bytes(
        &mut self,
        addr: usize,
        dst: &mut [u8],
        pool: &FramePool,
    ) -> Result<(), &'static str> {
        if dst.is_empty() {
            return Ok(());
        }
        let end = Self::checked_user_end(addr, dst.len())?;
        let mut copied = 0usize;
        while copied < dst.len() {
            let cur = addr + copied;
            let page_off = cur & (PAGE_SZ - 1);
            let chunk = min(end - cur, PAGE_SZ - page_off);
            let paddr = self
                .resolve_user_page(cur, UserPageAccess::Read, pool)
                .map_err(|_| "efault")?
                .paddr();
            copy_from_phys(paddr, &mut dst[copied..copied + chunk]);
            copied += chunk;
        }
        Ok(())
    }

    // AGENT: fault in the contiguous readable heap prefix before write-like
    // syscalls consume external state, preserving the existing short-I/O edge.
    pub fn readable_user_prefix_len(
        &mut self,
        addr: usize,
        len: usize,
        pool: &FramePool,
    ) -> Result<usize, &'static str> {
        if len == 0 {
            return Ok(0);
        }
        let end = Self::checked_user_end(addr, len)?;
        let mut checked = 0usize;
        while checked < len {
            let cur = addr + checked;
            if self
                .resolve_user_page(cur, UserPageAccess::Read, pool)
                .is_err()
            {
                return if checked == 0 {
                    Err("efault")
                } else {
                    Ok(checked)
                };
            }
            let page_off = cur & (PAGE_SZ - 1);
            checked += min(end - cur, PAGE_SZ - page_off);
        }
        Ok(checked)
    }

    // AGENT: fault in lazy heap pages and resolve writable transitions before a
    // read-like syscall consumes state, retaining the contiguous-prefix rule.
    pub fn writable_user_prefix_len(
        &mut self,
        addr: usize,
        len: usize,
        pool: &FramePool,
    ) -> Result<usize, &'static str> {
        if len == 0 {
            return Ok(0);
        }
        let end = Self::checked_user_end(addr, len)?;
        let mut checked = 0usize;
        while checked < len {
            let cur = addr + checked;
            let resolution = self.resolve_user_page(cur, UserPageAccess::Write, pool);
            if resolution.is_err() {
                return if checked == 0 {
                    Err("efault")
                } else {
                    Ok(checked)
                };
            }

            let page_off = cur & (PAGE_SZ - 1);
            checked += min(end - cur, PAGE_SZ - page_off);
        }
        Ok(checked)
    }

    // AGENT: read scalar user data through the unified byte-copy path.
    pub fn read_user_usize(
        &mut self,
        addr: usize,
        pool: &FramePool,
    ) -> Result<usize, &'static str> {
        let mut bytes = [0u8; mem::size_of::<usize>()];
        self.read_user_bytes(addr, &mut bytes, pool)?;
        Ok(usize::from_ne_bytes(bytes))
    }

    // AGENT: prepare one write chunk under the exclusive AddrSpace borrow,
    // resolving COW/shared dirty faults before checking the live Sv39 leaf.
    fn prepare_user_write_chunk(
        &mut self,
        cur: usize,
        end: usize,
        pool: &FramePool,
    ) -> Result<UserWriteChunk, &'static str> {
        let region_end = self.vm_map.find(cur).ok_or("efault")?.end();
        let page_off = cur & (PAGE_SZ - 1);
        let len = min(end - cur, min(PAGE_SZ - page_off, region_end - cur));
        let paddr = self
            .resolve_user_page(cur, UserPageAccess::Write, pool)
            .map_err(|_| "efault")?
            .paddr();
        Ok(UserWriteChunk { paddr, len })
    }

    // AGENT: user writes resolve COW or shared-file first-write tracking through
    // resident metadata before copying through the writable Sv39 translation.
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

    // AGENT: write every sticky-dirty shared-file resident in a range and flush
    // each affected filesystem before any VMA, leaf, or frame owner is removed.
    pub fn flush_shared_file_pages(&self, start: usize, len: usize) -> Result<(), &'static str> {
        if len == 0 || start % PAGE_SZ != 0 || len % PAGE_SZ != 0 {
            return Err("einval");
        }
        let end = start.checked_add(len).ok_or("efault")?;
        if end > USER_TOP {
            return Err("efault");
        }
        self.debug_check_page_table_consistency()?;

        let mut flush_sources: Vec<MmapFileSource> = Vec::new();
        for (&vaddr, page) in self.resident_pages.entries.range(start..end) {
            let PageBacking::FileShared(state) = &page.backing else {
                continue;
            };
            if !state.is_dirty() {
                continue;
            }
            let leaf = self.sv39.leaf_mapping(vaddr)?;
            if leaf.paddr != page.frame.paddr() {
                return Err("resident and Sv39 physical pages disagree");
            }
            if state.valid_len != 0 {
                let mut bytes = vec![0u8; PAGE_SZ];
                copy_from_phys(page.frame.paddr(), &mut bytes);
                let written = state
                    .source
                    .write_at(state.offset, &bytes[..state.valid_len])?;
                if written != state.valid_len {
                    return Err("eio");
                }
            }
            if !flush_sources
                .iter()
                .any(|source| source.filesystem_id() == state.source.filesystem_id())
            {
                flush_sources.push(state.source.clone());
            }
        }
        for source in flush_sources {
            source.flush()?;
        }
        Ok(())
    }

    // AGENT: expose complete shared-file writeback for exec and process teardown
    // while retaining range validation in the single transactional helper.
    pub fn flush_all_shared_file_pages(&self) -> Result<(), &'static str> {
        self.flush_shared_file_pages(0, USER_TOP)
    }

    // AGENT: write back shared-file pages before delegating the already-audited
    // hardware/VMA removal transaction to its no-I/O commit phase.
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
        self.flush_shared_file_pages(start, len)?;
        self.unmap_range_after_writeback(start, len, pool)
    }

    // AGENT: preflight only resident pages inside a writeback-complete range,
    // unmap leaves transactionally, then drop resident owners and VMA metadata.
    fn unmap_range_after_writeback(
        &mut self,
        start: usize,
        len: usize,
        pool: &FramePool,
    ) -> Result<usize, &'static str> {
        let end = start.checked_add(len).ok_or("efault")?;
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

    // AGENT: prepare an anonymous replacement only after old shared-file bytes
    // are durable, leaving the live mapping intact on allocation failure.
    pub fn replace_region(
        &mut self,
        region: VmRegion,
        pool: &FramePool,
    ) -> Result<(), &'static str> {
        Self::validate_eager_region(&region)?;
        let start = region.base;
        let len = region.len;
        self.debug_check_page_table_consistency()?;
        self.flush_shared_file_pages(start, len)?;
        let prepared = Self::prepare_anonymous_region(&region, pool)?;
        self.replace_prepared_region(region, prepared, pool)
    }

    // AGENT: flush an overwritten shared mapping before positioned reads build
    // the new fixed file image, then use the common rollback transaction.
    pub fn replace_file_region(
        &mut self,
        region: VmRegion,
        pool: &FramePool,
    ) -> Result<(), &'static str> {
        Self::validate_eager_region(&region)?;
        let start = region.base;
        let len = region.len;
        self.debug_check_page_table_consistency()?;
        self.flush_shared_file_pages(start, len)?;
        let prepared = Self::prepare_file_region(&region, pool)?;
        self.replace_prepared_region(region, prepared, pool)
    }

    // AGENT: replace a fixed range by retaining its complete VMA, resident,
    // backing, COW, and Sv39 snapshots until prepared-page installation commits.
    fn replace_prepared_region(
        &mut self,
        region: VmRegion,
        prepared: Vec<PreparedResidentPage>,
        pool: &FramePool,
    ) -> Result<(), &'static str> {
        let start = region.base;
        let len = region.len;
        let end = region.checked_end().ok_or("einval")?;

        let old_regions = self.vm_map.clone_regions();
        let mut old_pages = Vec::new();
        for (&vaddr, page) in self.resident_pages.entries.range(start..end) {
            let leaf = self.sv39.leaf_mapping(vaddr)?;
            if leaf.paddr != page.frame.paddr() {
                return Err("resident and Sv39 physical pages disagree");
            }
            old_pages.push((vaddr, page.clone(), leaf.flags));
        }

        self.unmap_range_after_writeback(start, len, pool)?;
        match self.install_prepared_region(region, prepared, pool) {
            Ok(()) => Ok(()),
            Err(err) => {
                self.vm_map.regions = old_regions;
                for (vaddr, page, leaf_flags) in old_pages {
                    self.sv39
                        .map_leaf(vaddr, page.frame.paddr(), leaf_flags, pool)
                        .expect("preflighted fixed-mmap rollback leaf should restore");
                    let replaced = self.resident_pages.entries.insert(vaddr, page);
                    assert!(
                        replaced.is_none(),
                        "fixed-mmap rollback must restore into an empty resident slot"
                    );
                }
                crate::csr::sfence_vma();
                self.debug_check_page_table_consistency()?;
                Err(err)
            }
        }
    }

    // AGENT: process teardown removes hardware leaves before resident frames,
    // releases Sv39/dynamic state, and resets both program-break bounds.
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
        self.start_brk = INITIAL_BRK;
        self.brk = INITIAL_BRK;
        crate::csr::sfence_vma();
    }

    // AGENT: snapshot leaf flags for rollback and keep clean shared-file pages
    // write-protected when committing the requested VMA protection policy.
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
                let new_pte_flags = Self::resident_leaf_flags(flags, pte);
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

    // AGENT: validate an eager page-granular mapping before either anonymous
    // allocation or positioned file I/O can publish observable address state.
    fn validate_eager_region(region: &VmRegion) -> Result<(), &'static str> {
        if region.len == 0 || region.base % PAGE_SZ != 0 || region.len % PAGE_SZ != 0 {
            return Err("einval");
        }
        if region.checked_end().ok_or("einval")? > USER_TOP {
            return Err("einval");
        }
        Ok(())
    }

    // AGENT: allocate and zero every anonymous resident before VMA publication
    // so partial frame exhaustion drops all prepared owners automatically.
    fn prepare_anonymous_region(
        region: &VmRegion,
        pool: &FramePool,
    ) -> Result<Vec<PreparedResidentPage>, &'static str> {
        if !matches!(&region.backing, VmBacking::Anonymous) {
            return Err("einval");
        }
        let mut prepared = Vec::with_capacity(region.len / PAGE_SZ);
        for vaddr in page_range(region.base, region.len) {
            let frame = pool.alloc_pg_frame().ok_or("enomem")?;
            zero_page(frame.paddr());
            let page = SharedPage::new(frame);
            prepared.push(PreparedResidentPage {
                vaddr,
                leaf_flags: Self::resident_leaf_flags(region.flags, &page),
                page,
            });
        }
        Ok(prepared)
    }

    // AGENT: eagerly populate zeroed physical pages through positioned reads,
    // retaining per-page valid EOF bytes and private/shared backing identity.
    fn prepare_file_region(
        region: &VmRegion,
        pool: &FramePool,
    ) -> Result<Vec<PreparedResidentPage>, &'static str> {
        let VmBacking::File {
            source,
            offset: region_offset,
        } = &region.backing
        else {
            return Err("einval");
        };
        let shared = region.flags & VM_SHARED != 0;
        let mut prepared = Vec::with_capacity(region.len / PAGE_SZ);
        for vaddr in page_range(region.base, region.len) {
            let displacement = vaddr.checked_sub(region.base).ok_or("eoverflow")?;
            let file_offset = region_offset.checked_add(displacement).ok_or("eoverflow")?;
            let frame = pool.alloc_pg_frame().ok_or("enomem")?;
            zero_page(frame.paddr());
            let mut bytes = vec![0u8; PAGE_SZ];
            let valid_len = source.read_at(file_offset, &mut bytes)?;
            if valid_len > PAGE_SZ {
                return Err("eio");
            }
            if valid_len != 0 {
                copy_to_phys(frame.paddr(), &bytes[..valid_len]);
            }
            let page = if shared {
                SharedPage::new_file_shared(frame, source.clone(), file_offset, valid_len)
            } else {
                SharedPage::new_file_private(frame, source.clone(), file_offset, valid_len)
            };
            prepared.push(PreparedResidentPage {
                vaddr,
                leaf_flags: Self::resident_leaf_flags(region.flags, &page),
                page,
            });
        }
        Ok(prepared)
    }

    // AGENT: install one fully prepared region through VMA, Sv39, and resident
    // owners, rolling back every published leaf if page-table allocation fails.
    fn install_prepared_region(
        &mut self,
        region: VmRegion,
        prepared: Vec<PreparedResidentPage>,
        pool: &FramePool,
    ) -> Result<(), &'static str> {
        if prepared.len() != region.len / PAGE_SZ {
            return Err("einval");
        }
        let region_base = region.base;
        let region_len = region.len;
        if let Err(err) = self.vm_map.insert(region) {
            return Err(err);
        }

        let mut mapped: Vec<(usize, SharedPage)> = Vec::with_capacity(prepared.len());
        for prepared_page in prepared {
            if let Err(err) = self.sv39.map_leaf(
                prepared_page.vaddr,
                prepared_page.page.paddr(),
                prepared_page.leaf_flags,
                pool,
            ) {
                for (mapped_addr, _) in mapped.iter() {
                    self.sv39
                        .unmap_leaf(*mapped_addr)
                        .expect("prepared Sv39 leaf rollback should succeed");
                }
                self.vm_map.remove_range(region_base, region_len);
                if !mapped.is_empty() {
                    crate::csr::sfence_vma();
                }
                return Err(err);
            }
            mapped.push((prepared_page.vaddr, prepared_page.page));
        }

        for (vaddr, page) in mapped {
            let replaced = self.resident_pages.entries.insert(vaddr, page);
            assert!(
                replaced.is_none(),
                "prepared mapping must install into empty resident slots"
            );
        }
        crate::csr::sfence_vma();
        self.debug_check_page_table_consistency()?;
        Ok(())
    }

    // AGENT: preserve the anonymous mapping API while routing allocation and
    // publication through the common prepared-resident transaction.
    pub fn map_region(&mut self, region: VmRegion, pool: &FramePool) -> Result<(), &'static str> {
        Self::validate_eager_region(&region)?;
        self.debug_check_page_table_consistency()?;
        let prepared = Self::prepare_anonymous_region(&region, pool)?;
        self.install_prepared_region(region, prepared, pool)
    }

    // AGENT: publish an anonymous VMA without allocating resident frames or
    // Sv39 leaves so VM_HEAP pages can be materialized by the shared fault path.
    pub(crate) fn reserve_anonymous_region(
        &mut self,
        region: VmRegion,
    ) -> Result<(), &'static str> {
        Self::validate_eager_region(&region)?;
        if region.flags & VM_HEAP == 0 || !matches!(&region.backing, VmBacking::Anonymous) {
            return Err("einval");
        }
        self.debug_check_page_table_consistency()?;
        self.vm_map.insert(region)?;
        self.debug_check_page_table_consistency()
    }

    // AGENT: eagerly read one file-backed VMA before publishing any of its
    // pages, preserving the OFD offset and private/shared backing policy.
    pub fn map_file_region(
        &mut self,
        region: VmRegion,
        pool: &FramePool,
    ) -> Result<(), &'static str> {
        Self::validate_eager_region(&region)?;
        self.debug_check_page_table_consistency()?;
        let prepared = Self::prepare_file_region(&region, pool)?;
        self.install_prepared_region(region, prepared, pool)
    }

    // AGENT: map an existing shared segment into this address space without
    // allocating anonymous frames or turning writable pages into COW mappings.
    pub fn map_shared_pages(
        &mut self,
        mut region: VmRegion,
        shared_pages: &[SharedPage],
        pool: &FramePool,
    ) -> Result<(), &'static str> {
        Self::validate_eager_region(&region)?;
        if !matches!(&region.backing, VmBacking::Anonymous) {
            return Err("einval");
        }
        if shared_pages.len() != region.len / PAGE_SZ {
            return Err("einval");
        }
        self.debug_check_page_table_consistency()?;

        region.flags |= VM_SHARED;
        let mut prepared = Vec::with_capacity(shared_pages.len());
        for (vaddr, page) in page_range(region.base, region.len).zip(shared_pages.iter()) {
            debug_assert!(!page.cow);
            let page = page.clone();
            prepared.push(PreparedResidentPage {
                vaddr,
                leaf_flags: Self::resident_leaf_flags(region.flags, &page),
                page,
            });
        }
        self.install_prepared_region(region, prepared, pool)
    }

    // AGENT: retain a byte-granular break while mapping only complete VM_HEAP
    // pages, rejecting unsafe shrink ownership and one-page-guard collisions.
    pub fn resize_brk(&mut self, new_brk: usize, pool: &FramePool) -> Result<(), BrkResizeError> {
        let old_brk = self.brk;
        if new_brk < self.start_brk || new_brk > USER_TOP {
            return Err(BrkResizeError::Rejected);
        }

        let old_mapped_end = checked_align_up(old_brk, PAGE_SZ).ok_or(BrkResizeError::Rejected)?;
        let new_mapped_end = checked_align_up(new_brk, PAGE_SZ).ok_or(BrkResizeError::Rejected)?;
        if old_mapped_end == new_mapped_end {
            self.brk = new_brk;
            return Ok(());
        }

        if new_mapped_end < old_mapped_end {
            let len = old_mapped_end - new_mapped_end;
            if self.vm_map.has_non_heap_overlap(new_mapped_end, len) {
                return Err(BrkResizeError::Rejected);
            }
            self.unmap_range(new_mapped_end, len, pool)
                .map_err(BrkResizeError::Internal)?;
        } else {
            let guarded_end = new_mapped_end
                .checked_add(PAGE_SZ)
                .ok_or(BrkResizeError::Rejected)?;
            let guarded_len = guarded_end
                .checked_sub(old_mapped_end)
                .ok_or(BrkResizeError::Rejected)?;
            if !self.vm_map.range_is_free(old_mapped_end, guarded_len) {
                return Err(BrkResizeError::Rejected);
            }

            let heap = VmRegion::new(
                old_mapped_end,
                new_mapped_end - old_mapped_end,
                VM_READ | VM_WRITE | VM_HEAP,
            );
            self.reserve_anonymous_region(heap)
                .map_err(|err| match err {
                    "enomem" | "oom" => BrkResizeError::Rejected,
                    internal => BrkResizeError::Internal(internal),
                })?;
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
