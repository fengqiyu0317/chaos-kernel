use super::*;

impl Kernel {
    // AGENT: recover private COW and first shared-file writes through the common
    // address-space transition; lazy load/execute faults remain unsupported.
    pub fn handle_pgfault(
        &self,
        addr: usize,
        access: KernelPageFaultAccess,
    ) -> Result<(), &'static str> {
        if addr >= USER_TOP {
            return Err("efault");
        }

        let task = self.cur_task(0).ok_or("esrch")?;
        match access {
            KernelPageFaultAccess::Store => {
                let mut addr_space = task.process.addr_space.lock().unwrap();
                addr_space.handle_write_fault(addr, &self.pool).map(|_| ())
            }
            KernelPageFaultAccess::Instruction | KernelPageFaultAccess::Load => Err("segfault"),
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
