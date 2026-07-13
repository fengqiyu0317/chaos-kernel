// AGENT: keep the original BTreeSet-backed recycled-id policy for upper-layer
// resources now that physical frames use independent preallocated metadata.
use alloc::collections::BTreeSet;

// AGENT: keep the exclusive resource limit beside the bump cursor and returned
// ids so one allocator cannot be called with inconsistent bounds.
pub(crate) struct AllocatorState {
    next: usize,
    limit: usize,
    free: BTreeSet<usize>,
}

// AGENT: restore the bounded BTreeSet allocator used by upper-layer ids such as
// file-backed blocks; FramePool no longer depends on this implementation.
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
    fn allocate_id(&mut self, id: usize) -> Option<usize> {
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

    // AGENT: return an allocated id once and report invalid or duplicate frees.
    pub(crate) fn release(&mut self, id: usize) -> bool {
        id < self.next && self.free.insert(id)
    }

    // AGENT: query availability across recycled and never-used resources.
    fn is_free(&self, id: usize) -> bool {
        id < self.limit && (self.free.contains(&id) || id >= self.next)
    }

    // AGENT: expose compact allocator statistics only to focused regression
    // builds without leaking the free set into the normal runtime surface.
    #[cfg(any(test, feature = "qemu-sync-selftest"))]
    pub(crate) fn stats(&self) -> (usize, usize) {
        (self.next, self.free.len())
    }
}
