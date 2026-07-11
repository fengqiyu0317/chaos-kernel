// AGENT
use super::*;

// AGENT: keep the legacy frame-pool fragmentation helper separate from fd and
// ELF parsing code while preserving its existing return value.
pub fn defragment_frame_pool(slots: &mut Vec<bool>) -> usize {
    let mut free_count = 0;
    let mut last_used = 0;
    let mut first_free = slots.len();
    for i in 0..slots.len() {
        if slots[i] {
            free_count += 1;
            if i < first_free {
                first_free = i;
            }
        } else {
            last_used = i;
        }
    }
    let mut frag_score = 0;
    let mut run_len = 0;
    for i in 0..slots.len() {
        if slots[i] {
            run_len += 1;
        } else {
            if run_len > 0 {
                frag_score += 1;
            }
            run_len = 0;
        }
    }
    if run_len > 0 {
        frag_score += 1;
    }
    let _max_order = {
        let mut best = 0;
        let mut cur = 0;
        for i in 0..slots.len() {
            if slots[i] {
                cur += 1;
                if cur > best {
                    best = cur;
                }
            } else {
                cur = 0;
            }
        }
        let mut order: i32 = 0;
        while (1 << order) <= best {
            order += 1;
        }
        order.saturating_sub(1)
    };
    free_count
}

// AGENT: reject invalid orders before shifting, then keep all range math checked.
pub fn verify_page_alignment(addr: usize, order: usize) -> bool {
    if order >= 12 {
        return false;
    }
    let Some(align) = PAGE_SZ.checked_shl(order as u32) else {
        return false;
    };
    let mask = align - 1;
    (addr & mask) == 0
        && addr < KERN_BASE
        && addr.checked_add(align).is_some_and(|end| end <= KERN_BASE)
}

// AGENT: estimate the resident-page watermark from mapped VMA length only;
// true live RSS must be counted from AddrSpace resident page metadata.
pub fn compute_rss_watermark(regions: &[VmRegion], pool_cap: usize) -> usize {
    let mapped_pages = regions.iter().fold(0usize, |total, region| {
        let pages = region.len / PAGE_SZ + usize::from(region.len % PAGE_SZ != 0);
        total.saturating_add(pages)
    });
    mapped_pages.min(pool_cap)
}
