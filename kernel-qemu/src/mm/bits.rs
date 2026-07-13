// AGENT: keep migrated bit and buddy helpers explicit no_std/alloc code.
extern crate alloc;

use alloc::vec::Vec;
use core::cmp::min;
use core::mem::size_of;
use core::sync::atomic::{AtomicUsize, Ordering};

const PAGE_SIZE: usize = 4096;

pub fn bitwise_merge(a: u64, b: u64, mask: u64) -> u64 {
    (a & !mask) | (b & mask)
}

// AGENT: keep zero-distance rotations masked to the requested bit width.
pub fn rotate_bits(value: u64, amount: u32, width: u32) -> u64 {
    if width == 0 || width > 64 {
        return value;
    }
    let mask = if width == 64 {
        !0u64
    } else {
        (1u64 << width) - 1
    };
    let v = value & mask;
    let actual = amount % width;
    if actual == 0 {
        return v;
    }
    ((v << actual) | (v >> (width - actual))) & mask
}

// AGENT: report invalid alignments and arithmetic overflow to callers instead
// of returning the unaligned input as if alignment had succeeded.
pub fn checked_align_up(addr: usize, align: usize) -> Option<usize> {
    if !align.is_power_of_two() {
        return None;
    }
    addr.checked_add(align - 1)
        .map(|value| value & !(align - 1))
}

// AGENT: centralize power-of-two address flooring while treating a bad
// alignment as an internal caller error rather than silently accepting it.
pub fn align_down(addr: usize, align: usize) -> usize {
    assert!(align.is_power_of_two(), "alignment must be a power of two");
    addr & !(align - 1)
}

pub fn log2_floor(v: usize) -> usize {
    if v == 0 {
        return 0;
    }
    (size_of::<usize>() * 8) - 1 - (v.leading_zeros() as usize)
}

pub struct BuddyAllocator {
    pub free_lists: Vec<Vec<usize>>,
    pub max_order: usize,
    pub base_addr: usize,
    pub total_pages: usize,
    pub allocated: AtomicUsize,
}

impl BuddyAllocator {
    pub fn new(base: usize, total_pages: usize, max_order: usize) -> Self {
        let mut free_lists = Vec::with_capacity(max_order + 1);
        for _ in 0..=max_order {
            free_lists.push(Vec::new());
        }
        let order = log2_floor(total_pages);
        let usable_order = min(order, max_order);
        let block_pages = pages_for_order(usable_order).unwrap_or(0);
        let mut addr = base;
        let mut remaining = total_pages;
        while block_pages != 0 && remaining >= block_pages {
            free_lists[usable_order].push(addr);
            let Some(next_addr) =
                block_bytes(usable_order).and_then(|bytes| addr.checked_add(bytes))
            else {
                break;
            };
            addr = next_addr;
            remaining -= block_pages;
        }
        for o in (0..usable_order).rev() {
            let pages = pages_for_order(o).unwrap_or(0);
            while remaining >= pages {
                free_lists[o].push(addr);
                let Some(next_addr) = block_bytes(o).and_then(|bytes| addr.checked_add(bytes))
                else {
                    break;
                };
                addr = next_addr;
                remaining -= pages;
            }
        }
        Self {
            free_lists,
            max_order,
            base_addr: base,
            total_pages,
            allocated: AtomicUsize::new(0),
        }
    }

    pub fn alloc_order(&mut self, order: usize) -> Option<usize> {
        if order > self.max_order {
            return None;
        }
        let allocated_pages = pages_for_order(order)?;
        for o in order..=self.max_order {
            if let Some(block) = self.free_lists[o].pop() {
                let mut current_order = o;
                let addr = block;
                while current_order > order {
                    current_order -= 1;
                    let buddy = addr.checked_add(block_bytes(current_order)?)?;
                    self.free_lists[current_order].push(buddy);
                }
                self.allocated.fetch_add(allocated_pages, Ordering::Relaxed);
                return Some(addr);
            }
        }
        None
    }

    // AGENT: validate frees before mutating the free lists so a bad address or
    // duplicate free cannot silently corrupt later frame allocation.
    pub fn free_order(&mut self, addr: usize, order: usize) -> Result<(), &'static str> {
        self.validate_free_block(addr, order)?;
        let released_pages = pages_for_order(order).ok_or("bad order")?;
        if self.allocated.load(Ordering::Relaxed) < released_pages {
            return Err("free exceeds allocated pages");
        }
        let mut current_addr = addr;
        let mut current_order = order;
        while current_order < self.max_order {
            let block_size = block_bytes(current_order).ok_or("bad order")?;
            let rel = current_addr
                .checked_sub(self.base_addr)
                .ok_or("address below base")?;
            let buddy_addr = self
                .base_addr
                .checked_add(rel ^ block_size)
                .ok_or("address overflow")?;
            if let Some(pos) = self.free_lists[current_order]
                .iter()
                .position(|&a| a == buddy_addr)
            {
                self.free_lists[current_order].remove(pos);
                current_addr = min(current_addr, buddy_addr);
                current_order += 1;
            } else {
                break;
            }
        }
        if self.overlaps_free_block(current_addr, current_order) {
            return Err("free block overlaps existing block");
        }
        self.free_lists[current_order].push(current_addr);
        self.allocated.fetch_sub(released_pages, Ordering::Relaxed);
        Ok(())
    }

    pub fn free_pages_count(&self) -> usize {
        let mut count = 0usize;
        for (order, list) in self.free_lists.iter().enumerate() {
            let pages = pages_for_order(order).unwrap_or(0);
            count = count.saturating_add(list.len().saturating_mul(pages));
        }
        count
    }

    pub fn largest_free_order(&self) -> Option<usize> {
        // AGENT
        for o in (0..=self.max_order).rev() {
            if !self.free_lists[o].is_empty() {
                return Some(o);
            }
        }
        None
    }

    pub fn fragmentation_score(&self) -> usize {
        // AGENT
        let total_free = self.free_pages_count();
        let largest = match self.largest_free_order() {
            Some(order) => pages_for_order(order).unwrap_or(0),
            None => return 0,
        };
        if total_free <= largest {
            return 0;
        }
        ((total_free - largest) * 100) / total_free
    }

    pub fn snapshot(&self) -> BuddyAllocator {
        BuddyAllocator {
            free_lists: self.free_lists.clone(),
            max_order: self.max_order,
            base_addr: self.base_addr,
            total_pages: self.total_pages,
            allocated: AtomicUsize::new(self.allocated.load(Ordering::Relaxed)),
        }
    }

    fn validate_free_block(&self, addr: usize, order: usize) -> Result<(), &'static str> {
        if order > self.max_order {
            return Err("bad order");
        }
        if addr % PAGE_SIZE != 0 {
            return Err("unaligned address");
        }
        let block_size = block_bytes(order).ok_or("bad order")?;
        let rel = addr
            .checked_sub(self.base_addr)
            .ok_or("address below base")?;
        if rel % block_size != 0 {
            return Err("address not order-aligned");
        }
        let range_end = self.managed_end().ok_or("managed range overflow")?;
        let block_end = addr.checked_add(block_size).ok_or("address overflow")?;
        if addr < self.base_addr || block_end > range_end {
            return Err("address outside managed range");
        }
        if self.overlaps_free_block(addr, order) {
            return Err("double free");
        }
        Ok(())
    }

    fn managed_end(&self) -> Option<usize> {
        self.total_pages
            .checked_mul(PAGE_SIZE)
            .and_then(|bytes| self.base_addr.checked_add(bytes))
    }

    fn overlaps_free_block(&self, addr: usize, order: usize) -> bool {
        self.free_lists
            .iter()
            .enumerate()
            .any(|(free_order, blocks)| {
                blocks.iter().any(|&free_addr| {
                    blocks_overlap(addr, order, free_addr, free_order).unwrap_or(true)
                })
            })
    }
}

fn pages_for_order(order: usize) -> Option<usize> {
    if order >= usize::BITS as usize {
        return None;
    }
    Some(1usize << order)
}

fn block_bytes(order: usize) -> Option<usize> {
    pages_for_order(order)?.checked_mul(PAGE_SIZE)
}

fn blocks_overlap(a: usize, a_order: usize, b: usize, b_order: usize) -> Option<bool> {
    let a_end = a.checked_add(block_bytes(a_order)?)?;
    let b_end = b.checked_add(block_bytes(b_order)?)?;
    Some(a < b_end && b < a_end)
}

// AGENT: keep the MM selftest module in src/mm/tests.rs even when bits.rs is
// compiled through the standalone mm_bits path in kernel-qemu/src/main.rs.
#[cfg(any(test, feature = "qemu-mm-selftest"))]
#[path = "tests.rs"]
pub mod tests;
