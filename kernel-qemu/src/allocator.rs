// AGENT: keep the original BTreeSet-backed recycled-id policy for upper-layer
// resources now that physical frames use independent preallocated metadata.
use alloc::collections::BTreeSet;

// AGENT: keep the exclusive resource limit beside the bump cursor and returned
// ids so one allocator cannot be called with inconsistent bounds.
#[derive(Clone)]
pub(crate) struct AllocatorState {
    next: usize,
    limit: usize,
    free: BTreeSet<usize>,
}

// AGENT: restore the bounded BTreeSet allocator used by upper-layer ids such as
// file-backed blocks; FramePool no longer depends on this implementation.
impl AllocatorState {
    // AGENT: begin sequential allocation at zero within one exclusive limit.
    pub(crate) fn new(limit: usize) -> Self {
        Self {
            next: 0,
            limit,
            free: BTreeSet::new(),
        }
    }

    // AGENT: allocate the lowest available id at or above a caller-supplied
    // lower bound, as required by bounded fd allocators such as F_DUPFD.
    pub(crate) fn allocate_from(&mut self, start: usize) -> Option<usize> {
        let recycled = self.free.range(start..).next().copied();
        let fresh = self.next.max(start);
        let fresh = (fresh < self.limit).then_some(fresh);
        let id = match (recycled, fresh) {
            (Some(recycled), Some(fresh)) => recycled.min(fresh),
            (Some(recycled), None) => recycled,
            (None, Some(fresh)) => fresh,
            _ => return None,
        };
        self.reserve(id)
    }

    // AGENT: reserve one exact id and preserve skipped never-used ids as
    // immediately reusable resources for dup2-style fixed-id allocation.
    pub(crate) fn reserve(&mut self, id: usize) -> Option<usize> {
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
