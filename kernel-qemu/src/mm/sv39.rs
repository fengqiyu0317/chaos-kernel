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

fn pte_paddr(pte: usize) -> usize {
    (pte >> 10) << 12
}

fn pte_is_valid(pte: usize) -> bool {
    pte & PTE_V != 0
}

fn pte_is_leaf(pte: usize) -> bool {
    pte & (PTE_R | PTE_W | PTE_X) != 0
}

fn vpn_indices(va: usize) -> [usize; 3] {
    [(va >> 30) & 0x1ff, (va >> 21) & 0x1ff, (va >> 12) & 0x1ff]
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

// AGENT: walk to the 4 KiB leaf slot, allocating intermediate page-table pages
// from the same FramePool used by migrated AddrSpace mappings.
fn walk_create(
    root_paddr: usize,
    va: usize,
    pool: &FramePool,
    page_table_frames: &mut Vec<PgFrame>,
) -> Result<(usize, usize), &'static str> {
    let indices = vpn_indices(va);
    let mut table = root_paddr;
    for &index in &indices[..2] {
        let pte = read_pte(table, index);
        if pte_is_valid(pte) {
            if pte_is_leaf(pte) {
                return Err("overlap");
            }
            table = pte_paddr(pte);
            continue;
        }

        let frame = pool.alloc_pg_frame().ok_or("enomem")?;
        let next_table = frame.paddr();
        zero_page(next_table);
        write_pte(table, index, make_table_pte(next_table));
        page_table_frames.push(frame);
        table = next_table;
    }
    Ok((table, indices[2]))
}

fn walk_existing(root_paddr: usize, va: usize) -> Result<(usize, usize, usize), &'static str> {
    let indices = vpn_indices(va);
    let mut table = root_paddr;
    for &index in &indices[..2] {
        let pte = read_pte(table, index);
        if !pte_is_valid(pte) || pte_is_leaf(pte) {
            return Err("efault");
        }
        table = pte_paddr(pte);
    }
    let leaf = read_pte(table, indices[2]);
    Ok((table, indices[2], leaf))
}

pub fn map(
    root_paddr: usize,
    va: usize,
    pa: usize,
    flags: usize,
    pool: &FramePool,
    page_table_frames: &mut Vec<PgFrame>,
) -> Result<(), &'static str> {
    if root_paddr == 0 || va % PAGE_SZ != 0 || pa % PAGE_SZ != 0 {
        return Err("einval");
    }
    let (table, index) = walk_create(root_paddr, va, pool, page_table_frames)?;
    let old = read_pte(table, index);
    if pte_is_valid(old) {
        return Err("overlap");
    }
    write_pte(table, index, make_leaf_pte(pa, flags));
    Ok(())
}

pub fn update_leaf(
    root_paddr: usize,
    va: usize,
    pa: usize,
    flags: usize,
) -> Result<(), &'static str> {
    if root_paddr == 0 || va % PAGE_SZ != 0 || pa % PAGE_SZ != 0 {
        return Err("einval");
    }
    let (table, index, old) = walk_existing(root_paddr, va)?;
    if !pte_is_valid(old) || !pte_is_leaf(old) {
        return Err("efault");
    }
    write_pte(table, index, make_leaf_pte(pa, flags));
    Ok(())
}

// AGENT: expose leaf lookup without permissions checks so higher-level MM code
// can validate metadata/table coherence before changing resident page state.
pub fn leaf_paddr(root_paddr: usize, va: usize) -> Result<usize, &'static str> {
    if root_paddr == 0 || va % PAGE_SZ != 0 {
        return Err("einval");
    }
    let (_, _, pte) = walk_existing(root_paddr, va)?;
    if !pte_is_valid(pte) || !pte_is_leaf(pte) {
        return Err("efault");
    }
    Ok(pte_paddr(pte))
}

pub fn unmap(root_paddr: usize, va: usize) -> Result<usize, &'static str> {
    if root_paddr == 0 || va % PAGE_SZ != 0 {
        return Err("einval");
    }
    let (table, index, old) = walk_existing(root_paddr, va)?;
    if !pte_is_valid(old) || !pte_is_leaf(old) {
        return Err("efault");
    }
    write_pte(table, index, 0);
    Ok(pte_paddr(old))
}

pub fn translate(root_paddr: usize, va: usize, access: PageAccess) -> Result<usize, &'static str> {
    if root_paddr == 0 {
        return Err("efault");
    }
    let (_, _, pte) = walk_existing(root_paddr, va)?;
    if !pte_is_valid(pte) || !pte_is_leaf(pte) {
        return Err("efault");
    }
    if pte & PTE_U == 0 {
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

// AGENT: own a real Sv39 root and the intermediate page-table frames allocated
// while mapping either a process address space or the early kernel direct map.
pub struct PageTable {
    root_paddr: usize,
    root_frame: Option<PgFrame>,
    table_frames: Vec<PgFrame>,
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

    // AGENT: expose the live root only after the first mapping allocates it.
    pub fn root_paddr(&self) -> Result<usize, &'static str> {
        if self.root_paddr == 0 {
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

    // AGENT: create a hardware leaf mapping while keeping intermediate table
    // frame ownership inside this PageTable.
    pub fn map_leaf(
        &mut self,
        va: usize,
        pa: usize,
        flags: usize,
        pool: &FramePool,
    ) -> Result<(), &'static str> {
        let root = self.ensure_root(pool)?;
        map(root, va, pa, flags, pool, &mut self.table_frames)
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

    // AGENT: update an existing hardware leaf through this owned Sv39 root.
    pub fn update_leaf(&self, va: usize, pa: usize, flags: usize) -> Result<(), &'static str> {
        update_leaf(self.root_paddr()?, va, pa, flags)
    }

    // AGENT: validate that resident metadata still has a matching hardware leaf
    // before mutating COW ownership.
    pub fn leaf_paddr(&self, va: usize) -> Result<usize, &'static str> {
        leaf_paddr(self.root_paddr()?, va)
    }

    // AGENT: keep callers simple when a not-yet-mapped address space has no root.
    pub fn update_leaf_if_present(
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
    // a real Sv39 root and report page-table inconsistencies to the caller.
    pub fn unmap_leaf_if_present(&self, va: usize) -> Result<(), &'static str> {
        if self.root_paddr == 0 {
            Ok(())
        } else {
            unmap(self.root_paddr, va).map(|_| ())
        }
    }

    // AGENT: route user memory translation through the owned Sv39 tree.
    pub fn translate(&self, va: usize, access: PageAccess) -> Result<usize, &'static str> {
        translate(self.root_paddr()?, va, access)
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
