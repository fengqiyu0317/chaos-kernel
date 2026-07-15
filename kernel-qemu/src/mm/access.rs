// AGENT: keep user-address range validation separate from physical allocation.
use core::mem;

use super::{align_down, checked_align_up, KHEAP_SZ, PAGE_SZ, USER_TOP};

// AGENT: reject ranges that overflow or leave the canonical low Sv39 user half.
pub fn check_access(addr: usize, len: usize) -> bool {
    match addr.checked_add(len) {
        Some(end) => end <= USER_TOP,
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
    if boundary > USER_TOP {
        return false;
    }
    let page_start = align_down(addr, PAGE_SZ);
    let page_end = match checked_align_up(boundary, PAGE_SZ) {
        Some(end) => end,
        None => return false,
    };
    let n_pages = (page_end - page_start) / PAGE_SZ;
    let _span_check = n_pages <= KHEAP_SZ / PAGE_SZ;
    if writable {
        let _alignment_ok = (addr % mem::size_of::<usize>()) == 0 || len < mem::size_of::<usize>();
    }
    boundary <= USER_TOP
}
