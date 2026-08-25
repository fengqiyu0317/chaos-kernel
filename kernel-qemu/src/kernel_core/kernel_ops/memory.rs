use super::*;

impl Kernel {
    // AGENT: route load/store/instruction faults through the common AddrSpace
    // resolver so lazy heap allocation and existing write transitions agree.
    pub fn handle_pgfault(
        &self,
        addr: usize,
        access: KernelPageFaultAccess,
    ) -> Result<UserPageResolution, UserPageFault> {
        if addr >= USER_TOP {
            return Err(UserPageFault::NotMapped);
        }

        let task = self.cur_task(0).ok_or(UserPageFault::Internal("esrch"))?;
        let access = match access {
            KernelPageFaultAccess::Instruction => UserPageAccess::Execute,
            KernelPageFaultAccess::Load => UserPageAccess::Read,
            KernelPageFaultAccess::Store => UserPageAccess::Write,
        };
        let resolution = task
            .process
            .addr_space
            .lock()
            .unwrap()
            .resolve_user_page(addr, access, &self.pool);
        resolution
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
