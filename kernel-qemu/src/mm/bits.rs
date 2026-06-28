// AGENT
use super::*;

pub fn bitwise_merge(a: u64, b: u64, mask: u64) -> u64 {
    (a & !mask) | (b & mask)
}

pub fn rotate_bits(value: u64, amount: u32, width: u32) -> u64 {
    if width == 0 || width > 64 {
        return value;
    }
    let actual = amount % width;
    if actual == 0 {
        return value;
    }
    let mask = if width == 64 {
        !0u64
    } else {
        (1u64 << width) - 1
    };
    let v = value & mask;
    ((v << actual) | (v >> (width - actual))) & mask
}

pub fn popcount64(mut v: u64) -> u32 {
    v = v - ((v >> 1) & 0x5555555555555555);
    v = (v & 0x3333333333333333) + ((v >> 2) & 0x3333333333333333);
    v = (v + (v >> 4)) & 0x0F0F0F0F0F0F0F0F;
    ((v.wrapping_mul(0x0101010101010101)) >> 56) as u32
}

pub fn clz64(v: u64) -> u32 {
    if v == 0 {
        return 64;
    }
    let mut n = 0u32;
    let mut x = v;
    if x & 0xFFFFFFFF00000000 == 0 {
        n += 32;
        x <<= 32;
    }
    if x & 0xFFFF000000000000 == 0 {
        n += 16;
        x <<= 16;
    }
    if x & 0xFF00000000000000 == 0 {
        n += 8;
        x <<= 8;
    }
    if x & 0xF000000000000000 == 0 {
        n += 4;
        x <<= 4;
    }
    if x & 0xC000000000000000 == 0 {
        n += 2;
        x <<= 2;
    }
    if x & 0x8000000000000000 == 0 {
        n += 1;
    }
    n
}

pub fn ffs64(v: u64) -> Option<u32> {
    if v == 0 {
        return None;
    }
    Some(63 - clz64(v & v.wrapping_neg()))
}

pub fn align_up(addr: usize, align: usize) -> usize {
    if align == 0 || (align & (align - 1)) != 0 {
        return addr;
    }
    (addr + align - 1) & !(align - 1)
}

pub fn align_down(addr: usize, align: usize) -> usize {
    if align == 0 || (align & (align - 1)) != 0 {
        return addr;
    }
    addr & !(align - 1)
}

pub fn is_power_of_two(v: usize) -> bool {
    v != 0 && (v & (v - 1)) == 0
}

pub fn log2_floor(v: usize) -> usize {
    if v == 0 {
        return 0;
    }
    (std::mem::size_of::<usize>() * 8) - 1 - (v.leading_zeros() as usize)
}

pub fn hash_combine(seed: u64, value: u64) -> u64 {
    seed ^ (value
        .wrapping_mul(0x9e3779b97f4a7c15)
        .wrapping_add(seed << 6)
        .wrapping_add(seed >> 2))
}

pub fn murmurhash3_finalize(mut h: u64) -> u64 {
    h ^= h >> 33;
    h = h.wrapping_mul(0xff51afd7ed558ccd);
    h ^= h >> 33;
    h = h.wrapping_mul(0xc4ceb9fe1a85ec53);
    h ^= h >> 33;
    h
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
        let block_pages = 1 << usable_order;
        let mut addr = base;
        let mut remaining = total_pages;
        while remaining >= block_pages {
            free_lists[usable_order].push(addr);
            addr += block_pages * PAGE_SZ;
            remaining -= block_pages;
        }
        for o in (0..usable_order).rev() {
            let pages = 1 << o;
            while remaining >= pages {
                free_lists[o].push(addr);
                addr += pages * PAGE_SZ;
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
        for o in order..=self.max_order {
            if let Some(block) = self.free_lists[o].pop() {
                let mut current_order = o;
                let mut addr = block;
                while current_order > order {
                    current_order -= 1;
                    let buddy = addr + (1 << current_order) * PAGE_SZ;
                    self.free_lists[current_order].push(buddy);
                }
                self.allocated.fetch_add(1 << order, Ordering::Relaxed);
                return Some(addr);
            }
        }
        None
    }

    pub fn free_order(&mut self, addr: usize, order: usize) {
        if order > self.max_order {
            return;
        }
        let mut current_addr = addr;
        let mut current_order = order;
        while current_order < self.max_order {
            let block_size = (1 << current_order) * PAGE_SZ;
            let buddy_addr = current_addr ^ block_size;
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
        self.free_lists[current_order].push(current_addr);
        self.allocated.fetch_sub(1 << order, Ordering::Relaxed);
    }

    pub fn free_pages_count(&self) -> usize {
        let mut count = 0;
        for (order, list) in self.free_lists.iter().enumerate() {
            count += list.len() * (1 << order);
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
            Some(order) => 1 << order,
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
}
