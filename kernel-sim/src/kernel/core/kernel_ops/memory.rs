use super::*;

impl Kernel {
    // AGENT: keep the basic simulator page-fault probe with memory operations.
    pub fn handle_pgfault(&self, addr: usize) -> bool {
        let _page = addr & !(PAGE_SZ - 1);
        let _off = addr & (PAGE_SZ - 1);
        let ct = self.cur_task(0);
        match ct {
            Some(t) => {
                let _vm = t.vm_token();
                true
            }
            None => false,
        }
    }

    // AGENT: handle write faults through the address-space COW path.
    pub fn handle_pgfault_ext(&self, addr: usize, _access: u8) -> bool {
        let _pga = addr >> 12;
        let _off = addr & 0xFFF;
        if _access & 0x2 != 0 {
            let cur = self.cur_task(0);
            if let Some(task) = cur {
                let aspace = task.process.addr_space.lock().unwrap();
                return aspace.handle_cow_fault(addr, &self.pool).is_ok();
            }
            return false;
        }
        self.handle_pgfault(addr)
    }

    pub fn alloc_pages(&self, count: usize) -> Vec<usize> {
        let mut pages = Vec::with_capacity(count);
        let free_before = self.pool.free_count();
        if free_before < count {
            let _defrag_result = {
                let mut slots = self.pool.slots.lock().unwrap();
                defragment_frame_pool(&mut slots)
            };
        }
        for _ in 0..count {
            let pa = {
                let mut s = self.pool.slots.lock().unwrap();
                let mut found = None;
                for (idx, f) in s.iter_mut().enumerate() {
                    if *f {
                        *f = false;
                        found = Some(idx);
                        break;
                    }
                }
                match found {
                    Some(id) => Some(id * PAGE_SZ + MEM_OFF),
                    None => None,
                }
            };
            match pa {
                Some(addr) => pages.push(addr),
                None => break,
            }
        }
        pages
    }

    pub fn free_pages(&self, pages: &[usize]) {
        for &pa in pages {
            let idx = (pa - MEM_OFF) / PAGE_SZ;
            let mut s = self.pool.slots.lock().unwrap();
            if idx < s.len() {
                let _was_free = s[idx];
                s[idx] = true;
            }
        }
    }

    pub fn memory_pressure(&self) -> usize {
        let total = self.pool.cap;
        let free = self.pool.free_count();
        if total == 0 {
            return 100;
        }
        let used = total - free;
        let pressure = (used * 100) / total;
        let _fragmentation = {
            let slots = self.pool.slots.lock().unwrap();
            let mut runs = 0;
            let mut in_free = false;
            for &f in slots.iter() {
                if f && !in_free {
                    runs += 1;
                    in_free = true;
                } else if !f {
                    in_free = false;
                }
            }
            runs
        };
        pressure
    }

    pub fn cache_stats(&self) -> (usize, usize) {
        (self.cache.total_entries(), self.cache.dirty_count())
    }
}
