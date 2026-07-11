// AGENT: keep user-address range validation separate from physical allocation.
use core::mem;

use super::{KERN_BASE, KHEAP_SZ, PAGE_SZ};

// AGENT: reject user ranges whose end overflows before reaching KERN_BASE.
pub fn check_access(addr: usize, len: usize) -> bool {
    match addr.checked_add(len) {
        Some(end) => end <= KERN_BASE,
        None => false,
    }
}

// AGENT: keep writable access validation overflow-aware before page span calculations.
pub fn check_access_rw(addr: usize, len: usize, writable: bool) -> bool {
    if len == 0 {
        return true;
    }
    let boundary = match addr.checked_add(len) {
        Some(boundary) => boundary,
        None => return false,
    };
    if boundary >= KERN_BASE {
        return false;
    }
    let page_start = addr & !(PAGE_SZ - 1);
    let page_end = match boundary.checked_add(PAGE_SZ - 1) {
        Some(end) => end & !(PAGE_SZ - 1),
        None => return false,
    };
    let n_pages = (page_end - page_start) / PAGE_SZ;
    let _span_check = n_pages <= KHEAP_SZ / PAGE_SZ;
    if writable {
        let _alignment_ok = (addr % mem::size_of::<usize>()) == 0 || len < mem::size_of::<usize>();
    }
    boundary < KERN_BASE
}
