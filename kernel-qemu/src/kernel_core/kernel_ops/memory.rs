use super::*;

impl Kernel {
    // AGENT: recover only migrated user COW store faults; other page faults
    // stay fatal until demand paging or stack growth semantics are migrated.
    pub fn handle_pgfault(
        &self,
        addr: usize,
        access: KernelPageFaultAccess,
    ) -> Result<(), &'static str> {
        if addr >= KERN_BASE {
            return Err("efault");
        }

        let task = self.cur_task(0).ok_or("esrch")?;
        match access {
            KernelPageFaultAccess::Store => {
                let addr_space = task.process.addr_space.lock().unwrap();
                addr_space.handle_cow_fault(addr, &self.pool).map(|_| ())
            }
            KernelPageFaultAccess::Instruction | KernelPageFaultAccess::Load => Err("segfault"),
        }
    }

    // AGENT: keep the legacy Kernel page allocation API as a thin wrapper over
    // FramePool, so physical-address conversion stays tied to the pool base.
    pub fn alloc_pages(&self, count: usize) -> Vec<usize> {
        let frames = self.pool.batch_alloc(count);
        let mut pages = Vec::with_capacity(frames.len());
        for frame_id in frames {
            match self.pool.frame_id_to_paddr(frame_id) {
                Some(paddr) => pages.push(paddr),
                None => self.pool.put(frame_id),
            }
        }
        pages
    }

    // AGENT: validate physical addresses through FramePool before returning
    // them to the shared frame bitmap.
    pub fn free_pages(&self, pages: &[usize]) {
        for &pa in pages {
            if let Some(frame_id) = self.pool.paddr_to_frame_id(pa) {
                self.pool.put(frame_id);
            }
        }
    }

    // AGENT: report pressure over the complete physical-memory span represented
    // by FramePool, including frames occupied before runtime allocation begins.
    pub fn memory_pressure(&self) -> usize {
        let total = self.pool.total_pages();
        let free = self.pool.free_count();
        if total == 0 {
            return 100;
        }
        let used = total.saturating_sub(free);
        used.saturating_mul(100) / total
    }
}
