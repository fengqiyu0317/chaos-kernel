// AGENT: share the bump-pointer plus recycled-id allocation policy between
// physical frames and file-backed RAM blocks without coupling their locks.
use alloc::{collections::BTreeSet, vec::Vec};

// AGENT: keep the exclusive resource limit beside the bump cursor and returned
// ids so one allocator cannot be called with inconsistent bounds.
pub(crate) struct AllocatorState {
    next: usize,
    limit: usize,
    free: BTreeSet<usize>,
}

// AGENT: centralize bounded single, specific, contiguous, and batch id
// allocation for both physical frames and file-backed blocks.
impl AllocatorState {
    // AGENT: begin sequential allocation across one validated half-open range.
    pub(crate) fn new(next: usize, limit: usize) -> Self {
        debug_assert!(next <= limit);
        Self {
            next,
            limit,
            free: BTreeSet::new(),
        }
    }

    // AGENT: prefer the lowest recycled id, then advance into never-used ids.
    pub(crate) fn allocate(&mut self) -> Option<usize> {
        let recycled = self.free.iter().next().copied();
        let fresh = (self.next < self.limit).then_some(self.next);
        let id = match (recycled, fresh) {
            (Some(recycled), Some(fresh)) => recycled.min(fresh),
            (Some(recycled), None) => recycled,
            (None, Some(fresh)) => fresh,
            _ => return None,
        };
        self.allocate_id(id)
    }

    // AGENT: reserve a requested id and preserve skipped never-used ids as
    // immediately reusable resources.
    pub(crate) fn allocate_id(&mut self, id: usize) -> Option<usize> {
        if !self.is_free(id) {
            return None;
        }
        if id < self.next {
            self.free.remove(&id);
        } else {
            for skipped in self.next..id {
                self.free.insert(skipped);
            }
            self.next = id + 1;
        }
        Some(id)
    }

    // AGENT: reserve the first aligned contiguous free run in one state update.
    pub(crate) fn allocate_contiguous(
        &mut self,
        first: usize,
        count: usize,
        align: usize,
    ) -> Option<usize> {
        if count == 0 || align == 0 {
            return None;
        }
        for start in (first..self.limit).step_by(align) {
            let Some(end) = start.checked_add(count) else {
                break;
            };
            if end > self.limit {
                break;
            }
            if (start..end).all(|id| self.is_free(id)) {
                for id in start..end {
                    self.allocate_id(id)?;
                }
                return Some(start);
            }
        }
        None
    }

    // AGENT: return an allocated id once and report invalid or duplicate frees.
    pub(crate) fn release(&mut self, id: usize) -> bool {
        id < self.next && self.free.insert(id)
    }

    // AGENT: query availability across recycled and never-used resources.
    pub(crate) fn is_free(&self, id: usize) -> bool {
        id < self.limit && (self.free.contains(&id) || id >= self.next)
    }

    // AGENT: count recycled ids plus the never-used suffix below the limit.
    pub(crate) fn free_count(&self) -> usize {
        self.free.len() + self.limit.saturating_sub(self.next)
    }

    // AGENT: reserve up to `count` ids using the same deterministic policy.
    pub(crate) fn allocate_batch(&mut self, count: usize) -> Vec<usize> {
        let mut ids = Vec::with_capacity(count);
        while ids.len() < count {
            let Some(id) = self.allocate() else {
                break;
            };
            ids.push(id);
        }
        ids
    }

    // AGENT: expose compact allocator statistics only to focused regression
    // builds without leaking the free set into the normal runtime surface.
    #[cfg(any(test, feature = "qemu-sync-selftest"))]
    pub(crate) fn stats(&self) -> (usize, usize) {
        (self.next, self.free.len())
    }
}
