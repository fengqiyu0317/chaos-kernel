// AGENT: Minimal Sv39 page-table helpers used by the migrated AddrSpace layer.
use super::*;

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

#[derive(Clone, Copy)]
pub enum PageAccess {
    Read,
    Write,
    Execute,
}

// AGENT: clear a freshly allocated physical page before using it as data or a
// lower-level page table.
pub fn zero_page(paddr: usize) {
    unsafe {
        ptr::write_bytes(paddr as *mut u8, 0, PAGE_SZ);
    }
}

// AGENT: copy one complete physical page during software COW resolution.
pub fn copy_page(dst_paddr: usize, src_paddr: usize) {
    unsafe {
        ptr::copy_nonoverlapping(src_paddr as *const u8, dst_paddr as *mut u8, PAGE_SZ);
    }
}

// AGENT: copy bytes from a kernel slice into translated physical user memory.
pub fn copy_to_phys(dst_paddr: usize, src: &[u8]) {
    unsafe {
        ptr::copy_nonoverlapping(src.as_ptr(), dst_paddr as *mut u8, src.len());
    }
}

// AGENT: copy bytes from translated physical user memory into a kernel slice.
pub fn copy_from_phys(src_paddr: usize, dst: &mut [u8]) {
    unsafe {
        ptr::copy_nonoverlapping(src_paddr as *const u8, dst.as_mut_ptr(), dst.len());
    }
}

// AGENT: borrow a physical page through the current identity/bare mapping.
pub fn phys_page_slice(paddr: usize) -> &'static [u8] {
    unsafe { core::slice::from_raw_parts(paddr as *const u8, PAGE_SZ) }
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
    unsafe { (table_paddr as *mut usize).add(index) }
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
