// AGENT: Minimal Sv39 page-table helpers used by the migrated AddrSpace layer.
use super::*;
use core::sync::atomic::{AtomicBool, Ordering};

pub const PTE_V: usize = 1 << 0;
pub const PTE_R: usize = 1 << 1;
pub const PTE_W: usize = 1 << 2;
pub const PTE_X: usize = 1 << 3;
pub const PTE_U: usize = 1 << 4;
pub const PTE_G: usize = 1 << 5;
pub const PTE_A: usize = 1 << 6;
pub const PTE_D: usize = 1 << 7;

const PTE_COUNT: usize = 512;
const PTE_FLAG_MASK: usize = 0x3ff;
const PTE_PPN_MASK: usize = ((1usize << 44) - 1) << 10;
const LEAF_INPUT_FLAG_MASK: usize = PTE_FLAG_MASK & !PTE_V;
const SV39_VADDR_BITS: usize = 39;
const SV39_PADDR_BITS: usize = 56;

static DIRECT_MAP_ACTIVE: AtomicBool = AtomicBool::new(false);

#[derive(Clone, Copy)]
pub enum PageAccess {
    Read,
    Write,
    Execute,
}

// AGENT: resolve a physical address through the active kernel access mode.
fn phys_access_addr(paddr: usize) -> usize {
    if DIRECT_MAP_ACTIVE.load(Ordering::Acquire) {
        p2v(paddr)
    } else {
        paddr
    }
}

// AGENT: expose the direct-map state so boot code can report the installed
// address-space mode without reaching into the atomic directly.
pub fn direct_map_active() -> bool {
    DIRECT_MAP_ACTIVE.load(Ordering::Acquire)
}

// AGENT: clear a freshly allocated physical page before using it as data or a
// lower-level page table.
pub fn zero_page(paddr: usize) {
    debug_assert_eq!(paddr % PAGE_SZ, 0);
    unsafe {
        ptr::write_bytes(phys_access_addr(paddr) as *mut u8, 0, PAGE_SZ);
    }
}

// AGENT: copy one complete physical page during software COW resolution.
pub fn copy_page(dst_paddr: usize, src_paddr: usize) {
    debug_assert_eq!(dst_paddr % PAGE_SZ, 0);
    debug_assert_eq!(src_paddr % PAGE_SZ, 0);
    unsafe {
        ptr::copy_nonoverlapping(
            phys_access_addr(src_paddr) as *const u8,
            phys_access_addr(dst_paddr) as *mut u8,
            PAGE_SZ,
        );
    }
}

// AGENT: copy bytes from a kernel slice into translated physical user memory.
pub fn copy_to_phys(dst_paddr: usize, src: &[u8]) {
    unsafe {
        ptr::copy_nonoverlapping(
            src.as_ptr(),
            phys_access_addr(dst_paddr) as *mut u8,
            src.len(),
        );
    }
}

// AGENT: copy bytes from translated physical user memory into a kernel slice.
pub fn copy_from_phys(src_paddr: usize, dst: &mut [u8]) {
    unsafe {
        ptr::copy_nonoverlapping(
            phys_access_addr(src_paddr) as *const u8,
            dst.as_mut_ptr(),
            dst.len(),
        );
    }
}

// AGENT: borrow a physical page through the current kernel-access mapping.
pub fn phys_page_slice(paddr: usize) -> &'static [u8] {
    debug_assert_eq!(paddr % PAGE_SZ, 0);
    unsafe { core::slice::from_raw_parts(phys_access_addr(paddr) as *const u8, PAGE_SZ) }
}

pub fn make_leaf_pte(paddr: usize, flags: usize) -> usize {
    ((paddr >> 12) << 10) | (flags & PTE_FLAG_MASK) | PTE_V
}

fn make_table_pte(paddr: usize) -> usize {
    ((paddr >> 12) << 10) | PTE_V
}

// AGENT: decode only the architectural PPN bits so reserved or extension bits
// above bit 53 cannot be mistaken for part of the physical page address.
fn pte_paddr(pte: usize) -> usize {
    ((pte & PTE_PPN_MASK) >> 10) << 12
}

fn pte_is_valid(pte: usize) -> bool {
    pte & PTE_V != 0
}

// AGENT: classify the architectural R/W/X combinations shared by raw PTE
// decoding and caller-supplied leaf flags.
fn leaf_permissions_are_valid(flags: usize) -> bool {
    let readable = flags & PTE_R != 0;
    let writable = flags & PTE_W != 0;
    let executable = flags & PTE_X != 0;
    (readable || executable) && (!writable || readable)
}

// AGENT: accept only the low Sv39 leaf flag field supplied by callers; PTE_V is
// owned by make_leaf_pte(), while bits 8..9 remain available as software RSW.
fn leaf_flags_are_valid(flags: usize) -> bool {
    flags & !LEAF_INPUT_FLAG_MASK == 0 && leaf_permissions_are_valid(flags)
}

// AGENT: distinguish a valid architectural leaf from invalid and reserved PTE
// encodings instead of treating every nonzero R/W/X combination as a leaf.
fn pte_is_leaf(pte: usize) -> bool {
    pte_is_valid(pte) && leaf_permissions_are_valid(pte)
}

// AGENT: follow an entry as a next-level page-table pointer only when it is
// valid and all three leaf permission bits are clear.
fn pte_is_table(pte: usize) -> bool {
    pte_is_valid(pte) && pte & (PTE_R | PTE_W | PTE_X) == 0
}

fn vpn_indices(va: usize) -> [usize; 3] {
    [(va >> 30) & 0x1ff, (va >> 21) & 0x1ff, (va >> 12) & 0x1ff]
}

// AGENT: reject addresses whose upper bits would be discarded by vpn_indices()
// instead of allowing non-canonical virtual addresses to alias legal mappings.
fn va_is_sv39_canonical(va: usize) -> bool {
    let low_bits = va & ((1usize << SV39_VADDR_BITS) - 1);
    canonicalize_sv39(low_bits) == va
}

// AGENT: validate a 4 KiB virtual leaf address before walking the Sv39 tree.
fn leaf_vaddr_is_valid(va: usize) -> bool {
    va % PAGE_SZ == 0 && va_is_sv39_canonical(va)
}

// AGENT: keep physical leaf and root addresses within the 44-bit PPN field
// decoded by pte_paddr(), in addition to requiring 4 KiB alignment.
fn page_paddr_is_valid(pa: usize) -> bool {
    pa % PAGE_SZ == 0 && pa >> SV39_PADDR_BITS == 0
}

// AGENT: reserve physical address zero as PageTable's uninitialized-root
// sentinel while accepting it as an architecturally encodable leaf address.
fn root_paddr_is_valid(root_paddr: usize) -> bool {
    root_paddr != 0 && page_paddr_is_valid(root_paddr)
}

// AGENT: share leaf input validation between the owned PageTable entry point
// and its private raw-root mutation helpers.
fn leaf_args_are_valid(va: usize, pa: usize, flags: usize) -> bool {
    leaf_vaddr_is_valid(va) && page_paddr_is_valid(pa) && leaf_flags_are_valid(flags)
}

fn pte_addr(table_paddr: usize, index: usize) -> *mut usize {
    debug_assert!(index < PTE_COUNT);
    unsafe { (phys_access_addr(table_paddr) as *mut usize).add(index) }
}

fn read_pte(table_paddr: usize, index: usize) -> usize {
    unsafe { ptr::read_volatile(pte_addr(table_paddr, index)) }
}

fn write_pte(table_paddr: usize, index: usize, value: usize) {
    unsafe {
        ptr::write_volatile(pte_addr(table_paddr, index), value);
    }
}

// AGENT: reconstruct a canonical Sv39 address from the low 39 virtual-address
// bits accumulated while walking a hardware page-table tree.
fn canonicalize_sv39(vaddr: usize) -> usize {
    const SV39_SIGN_BIT: usize = 1usize << (SV39_VADDR_BITS - 1);
    if vaddr & SV39_SIGN_BIT == 0 {
        vaddr
    } else {
        vaddr | (!0usize << SV39_VADDR_BITS)
    }
}

// AGENT: own a real Sv39 root and the intermediate page-table frames allocated
// while mapping either a process address space or the early kernel direct map.
pub struct PageTable {
    root_paddr: usize,
    root_frame: Option<PgFrame>,
    table_frames: Vec<PgFrame>,
}

// AGENT: provide a normalized hardware-leaf view for AddrSpace invariants and
// QEMU selftests; PTE_V is implicit and therefore excluded from flags.
pub(super) struct Sv39Leaf {
    pub(super) vaddr: usize,
    pub(super) paddr: usize,
    pub(super) flags: usize,
}

impl PageTable {
    // AGENT: start without a hardware root because callers may construct VM
    // metadata before they have access to the FramePool.
    pub fn new() -> Self {
        Self {
            root_paddr: 0,
            root_frame: None,
            table_frames: Vec::new(),
        }
    }

    // AGENT: expose the live root only after the first mapping allocates a
    // correctly aligned, architecturally encodable page-table frame.
    pub fn root_paddr(&self) -> Result<usize, &'static str> {
        if !root_paddr_is_valid(self.root_paddr) {
            Err("efault")
        } else {
            Ok(self.root_paddr)
        }
    }

    // AGENT: avoid returning page-table frames while satp still points at this root.
    pub fn deactivate_if_current(&self) {
        if self.root_paddr != 0
            && crate::csr::read_satp() == crate::csr::make_satp_sv39(self.root_paddr)
        {
            unsafe {
                crate::csr::write_satp(0);
            }
            crate::csr::sfence_vma();
            DIRECT_MAP_ACTIVE.store(false, Ordering::Release);
        }
    }

    // AGENT: lazily allocate the real Sv39 root on the first mapping operation.
    pub fn ensure_root(&mut self, pool: &FramePool) -> Result<usize, &'static str> {
        if self.root_paddr != 0 {
            return Ok(self.root_paddr);
        }
        let frame = pool.alloc_pg_frame().ok_or("enomem")?;
        zero_page(frame.paddr());
        self.root_paddr = frame.paddr();
        self.root_frame = Some(frame);
        Ok(self.root_paddr)
    }

    // AGENT: walk this owned page table to a 4 KiB leaf slot, allocating and
    // retaining every intermediate page-table frame inside PageTable.
    fn walk_create(&mut self, va: usize, pool: &FramePool) -> Result<(usize, usize), &'static str> {
        let indices = vpn_indices(va);
        let mut table = self.root_paddr()?;
        for &index in &indices[..2] {
            let pte = read_pte(table, index);
            if pte_is_valid(pte) {
                if pte_is_leaf(pte) {
                    return Err("overlap");
                }
                if !pte_is_table(pte) {
                    return Err("efault");
                }
                table = pte_paddr(pte);
                continue;
            }

            let frame = pool.alloc_pg_frame().ok_or("enomem")?;
            let next_table = frame.paddr();
            zero_page(next_table);
            write_pte(table, index, make_table_pte(next_table));
            self.table_frames.push(frame);
            table = next_table;
        }
        Ok((table, indices[2]))
    }

    // AGENT: descend through this owned root only via valid next-level pointers
    // so malformed writable-without-readable entries are never dereferenced.
    fn walk_existing(&self, va: usize) -> Result<(usize, usize, usize), &'static str> {
        let indices = vpn_indices(va);
        let mut table = self.root_paddr()?;
        for &index in &indices[..2] {
            let pte = read_pte(table, index);
            if !pte_is_table(pte) {
                return Err("efault");
            }
            table = pte_paddr(pte);
        }
        let leaf = read_pte(table, indices[2]);
        Ok((table, indices[2], leaf))
    }

    // AGENT: recursively collect every valid 4 KiB leaf under an owned root so
    // callers can detect hardware mappings that have no resident-page owner.
    fn collect_leaf_mappings(
        table_paddr: usize,
        level: usize,
        vaddr_prefix: usize,
        leaves: &mut Vec<Sv39Leaf>,
    ) -> Result<(), &'static str> {
        for index in 0..PTE_COUNT {
            let pte = read_pte(table_paddr, index);
            if !pte_is_valid(pte) {
                continue;
            }

            let shift = 12 + level * 9;
            let vaddr = vaddr_prefix | (index << shift);
            if pte_is_leaf(pte) {
                if level != 0 {
                    return Err("unexpected huge leaf");
                }
                leaves.push(Sv39Leaf {
                    vaddr: canonicalize_sv39(vaddr),
                    paddr: pte_paddr(pte),
                    flags: pte & PTE_FLAG_MASK & !PTE_V,
                });
                continue;
            }

            if !pte_is_table(pte) {
                return Err("invalid Sv39 PTE");
            }
            if level == 0 {
                return Err("invalid Sv39 leaf table");
            }
            Self::collect_leaf_mappings(pte_paddr(pte), level - 1, vaddr, leaves)?;
        }
        Ok(())
    }

    // AGENT: create a hardware leaf mapping while keeping intermediate table
    // frame ownership inside this PageTable instead of a raw-root helper.
    pub fn map_leaf(
        &mut self,
        va: usize,
        pa: usize,
        flags: usize,
        pool: &FramePool,
    ) -> Result<(), &'static str> {
        if !leaf_args_are_valid(va, pa, flags) {
            return Err("einval");
        }
        self.ensure_root(pool)?;
        let (table, index) = self.walk_create(va, pool)?;
        let old = read_pte(table, index);
        if pte_is_valid(old) {
            return Err("overlap");
        }
        write_pte(table, index, make_leaf_pte(pa, flags));
        Ok(())
    }

    // AGENT: map a contiguous physical range at an equally contiguous virtual range.
    pub fn map_linear(
        &mut self,
        va_start: usize,
        pa_start: usize,
        len: usize,
        flags: usize,
        pool: &FramePool,
    ) -> Result<(), &'static str> {
        if va_start % PAGE_SZ != 0 || pa_start % PAGE_SZ != 0 {
            return Err("einval");
        }
        let pages = len.checked_add(PAGE_SZ - 1).ok_or("einval")? / PAGE_SZ;
        for page in 0..pages {
            let offset = page.checked_mul(PAGE_SZ).ok_or("einval")?;
            let va = va_start.checked_add(offset).ok_or("einval")?;
            let pa = pa_start.checked_add(offset).ok_or("einval")?;
            self.map_leaf(va, pa, flags, pool)?;
        }
        Ok(())
    }

    // AGENT: update an existing hardware leaf directly through this PageTable's
    // owned root, keeping the mutation implementation inside the owner type.
    pub fn update_leaf(&mut self, va: usize, pa: usize, flags: usize) -> Result<(), &'static str> {
        if !leaf_args_are_valid(va, pa, flags) {
            return Err("einval");
        }
        let (table, index, old) = self.walk_existing(va)?;
        if !pte_is_leaf(old) {
            return Err("efault");
        }
        write_pte(table, index, make_leaf_pte(pa, flags));
        Ok(())
    }

    // AGENT: return physical identity and normalized flags for one resident
    // consistency check without a parallel raw-root lookup function.
    pub(super) fn leaf_mapping(&self, va: usize) -> Result<Sv39Leaf, &'static str> {
        if !leaf_vaddr_is_valid(va) {
            return Err("einval");
        }
        let (_, _, pte) = self.walk_existing(va)?;
        if !pte_is_leaf(pte) {
            return Err("efault");
        }
        Ok(Sv39Leaf {
            vaddr: va,
            paddr: pte_paddr(pte),
            flags: pte & PTE_FLAG_MASK & !PTE_V,
        })
    }

    // AGENT: snapshot every owned hardware leaf so AddrSpace can enforce the
    // reverse invariant that no Sv39 user mapping exists without resident data.
    pub(super) fn leaf_mappings(&self) -> Result<Vec<Sv39Leaf>, &'static str> {
        if self.root_paddr == 0 {
            return Ok(Vec::new());
        }
        let mut leaves = Vec::new();
        Self::collect_leaf_mappings(self.root_paddr()?, 2, 0, &mut leaves)?;
        Ok(leaves)
    }

    // AGENT: require a live root and leaf when removing a mapping so callers
    // cannot commit resident metadata against a missing owned Sv39 table.
    pub fn unmap_leaf(&mut self, va: usize) -> Result<(), &'static str> {
        if !leaf_vaddr_is_valid(va) {
            return Err("einval");
        }
        let (table, index, old) = self.walk_existing(va)?;
        if !pte_is_leaf(old) {
            return Err("efault");
        }
        write_pte(table, index, 0);
        Ok(())
    }

    // AGENT: translate user memory directly through this owned Sv39 tree and
    // enforce the leaf's user and requested-access permission bits.
    pub fn translate(&self, va: usize, access: PageAccess) -> Result<usize, &'static str> {
        if !va_is_sv39_canonical(va) {
            return Err("efault");
        }
        let (_, _, pte) = self.walk_existing(va)?;
        if !pte_is_leaf(pte) || pte & PTE_U == 0 {
            return Err("efault");
        }
        match access {
            PageAccess::Read if pte & PTE_R == 0 => return Err("efault"),
            PageAccess::Write if pte & PTE_W == 0 => return Err("efault"),
            PageAccess::Execute if pte & PTE_X == 0 => return Err("efault"),
            _ => {}
        }
        Ok(pte_paddr(pte) + (va & (PAGE_SZ - 1)))
    }

    // AGENT: install this page table as the active Sv39 root and enable direct-map
    // physical-page access for later frame and PTE operations.
    pub fn activate_kernel_direct_map(&self) -> Result<(), &'static str> {
        let root = self.root_paddr()?;
        unsafe {
            crate::csr::write_satp(crate::csr::make_satp_sv39(root));
        }
        crate::csr::sfence_vma();
        DIRECT_MAP_ACTIVE.store(true, Ordering::Release);
        Ok(())
    }

    // AGENT: drop all hardware page-table frames during exec or process teardown.
    pub fn clear(&mut self) {
        self.table_frames.clear();
        self.root_frame = None;
        self.root_paddr = 0;
    }
}

// AGENT: create the first QEMU Sv39 root with both low identity mappings and a
// high-half direct map so the low-linked kernel can keep running after satp.
pub fn build_kernel_page_table(
    pool: &FramePool,
    ram_start: usize,
    ram_end: usize,
) -> Result<PageTable, &'static str> {
    if ram_start % PAGE_SZ != 0 || ram_end % PAGE_SZ != 0 || ram_end <= ram_start {
        return Err("einval");
    }

    let mut page_table = PageTable::new();
    let len = ram_end - ram_start;
    let flags = PTE_R | PTE_W | PTE_X | PTE_G | PTE_A | PTE_D;
    page_table.map_linear(ram_start, ram_start, len, flags, pool)?;
    page_table.map_linear(p2v(ram_start), ram_start, len, flags, pool)?;
    Ok(page_table)
}

// AGENT: keep pure PTE classification regressions available to both Rust tests
// and the optional QEMU MM boot selftest.
#[cfg(any(test, feature = "qemu-mm-selftest"))]
pub mod tests {
    use super::*;

    // AGENT: run every Sv39-specific MM regression from the QEMU boot hook.
    pub fn run_all() {
        leaf_classification_rejects_invalid_encodings();
        leaf_input_validation_rejects_aliases_and_truncation();
    }

    // AGENT: enforce the Sv39 distinction between next-level pointers, legal
    // leaves, invalid entries, and the reserved writable-without-readable form.
    #[cfg_attr(test, test)]
    fn leaf_classification_rejects_invalid_encodings() {
        assert!(!pte_is_leaf(0));
        assert!(!pte_is_table(0));
        assert!(!pte_is_leaf(PTE_R));
        assert!(!pte_is_leaf(PTE_V));
        assert!(pte_is_table(PTE_V));
        assert!(!pte_is_leaf(PTE_V | PTE_W));
        assert!(!pte_is_table(PTE_V | PTE_W));
        assert!(!pte_is_leaf(PTE_V | PTE_W | PTE_X));

        assert!(pte_is_leaf(PTE_V | PTE_R));
        assert!(pte_is_leaf(PTE_V | PTE_X));
        assert!(pte_is_leaf(PTE_V | PTE_R | PTE_W));
        assert!(pte_is_leaf(PTE_V | PTE_R | PTE_W | PTE_X));
    }

    // AGENT: prove that leaf input validation rejects addresses and flags that
    // the raw PTE encoder or VPN extraction would otherwise silently truncate.
    #[cfg_attr(test, test)]
    fn leaf_input_validation_rejects_aliases_and_truncation() {
        let low_va = PAGE_SZ;
        let noncanonical_alias = (1usize << SV39_VADDR_BITS) | low_va;
        let high_va = canonicalize_sv39((1usize << (SV39_VADDR_BITS - 1)) | low_va);

        assert!(va_is_sv39_canonical(low_va));
        assert!(va_is_sv39_canonical(high_va));
        assert!(!va_is_sv39_canonical(noncanonical_alias));
        assert_eq!(vpn_indices(noncanonical_alias), vpn_indices(low_va));

        assert!(page_paddr_is_valid(0));
        assert!(page_paddr_is_valid(0x8000_0000));
        assert!(!page_paddr_is_valid(1usize << SV39_PADDR_BITS));

        assert!(leaf_flags_are_valid(PTE_R | PTE_U | PTE_A));
        assert!(leaf_flags_are_valid(PTE_R | (0b11 << 8)));
        assert!(!leaf_flags_are_valid(PTE_W));
        assert!(!leaf_flags_are_valid(PTE_V | PTE_R));
        assert!(!leaf_flags_are_valid(PTE_R | (1usize << 10)));

        assert!(leaf_args_are_valid(low_va, 0, PTE_R));
        assert!(!leaf_args_are_valid(noncanonical_alias, 0, PTE_R));
    }
}
