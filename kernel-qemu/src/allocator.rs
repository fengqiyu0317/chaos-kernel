// AGENT: share the bump-pointer plus recycled-id allocation policy between
// physical frames and file-backed RAM blocks without coupling their locks.
use alloc::{vec, vec::Vec};

const WORD_BITS: usize = usize::BITS as usize;

// AGENT: keep the exclusive resource limit beside the bump cursor and returned
// ids so one allocator cannot be called with inconsistent bounds.
pub(crate) struct AllocatorState {
    next: usize,
    limit: usize,
    free_bits: Vec<usize>,
    recycled: usize,
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
            free_bits: vec![0; limit.saturating_add(WORD_BITS - 1) / WORD_BITS],
            recycled: 0,
        }
    }

    // AGENT: prefer the lowest recycled id, then advance into never-used ids.
    pub(crate) fn allocate(&mut self) -> Option<usize> {
        let recycled = self.first_recycled();
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
            self.clear_recycled(id);
        } else {
            for skipped in self.next..id {
                self.set_recycled(skipped);
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
        if id >= self.next || self.is_recycled(id) {
            return false;
        }
        self.set_recycled(id);
        true
    }

    // AGENT: validate the complete run before publishing any returned id so
    // page-backed heap teardown cannot leave a partially released span.
    pub(crate) fn release_contiguous(&mut self, first: usize, count: usize) -> bool {
        let Some(end) = first.checked_add(count) else {
            return false;
        };
        if count == 0 || end > self.next || (first..end).any(|id| self.is_recycled(id)) {
            return false;
        }
        for id in first..end {
            self.set_recycled(id);
        }
        true
    }

    // AGENT: query availability across recycled and never-used resources.
    pub(crate) fn is_free(&self, id: usize) -> bool {
        id < self.limit && (self.is_recycled(id) || id >= self.next)
    }

    // AGENT: count recycled ids plus the never-used suffix below the limit.
    pub(crate) fn free_count(&self) -> usize {
        self.recycled + self.limit.saturating_sub(self.next)
    }

    // AGENT: fill caller-owned storage so holding a FramePool lock never needs
    // to recurse into the global allocator.
    pub(crate) fn allocate_batch_into(&mut self, count: usize, ids: &mut Vec<usize>) {
        while ids.len() < count {
            let Some(id) = self.allocate() else {
                break;
            };
            ids.push(id);
        }
    }

    // AGENT: expose compact allocator statistics only to focused regression
    // builds without leaking the free set into the normal runtime surface.
    #[cfg(any(test, feature = "qemu-sync-selftest"))]
    pub(crate) fn stats(&self) -> (usize, usize) {
        (self.next, self.recycled)
    }

    // AGENT: find returned ids without allocating while the allocator lock is
    // held; this keeps FramePool usable as the global heap's page provider.
    fn first_recycled(&self) -> Option<usize> {
        for (word_index, &word) in self.free_bits.iter().enumerate() {
            if word == 0 {
                continue;
            }
            let bit = word.trailing_zeros() as usize;
            let id = word_index.checked_mul(WORD_BITS)?.checked_add(bit)?;
            if id < self.next && id < self.limit {
                return Some(id);
            }
        }
        None
    }

    fn is_recycled(&self, id: usize) -> bool {
        if id >= self.limit {
            return false;
        }
        let word = id / WORD_BITS;
        let bit = id % WORD_BITS;
        self.free_bits[word] & (1usize << bit) != 0
    }

    fn set_recycled(&mut self, id: usize) {
        debug_assert!(id < self.limit);
        let word = id / WORD_BITS;
        let bit = id % WORD_BITS;
        let mask = 1usize << bit;
        debug_assert_eq!(self.free_bits[word] & mask, 0);
        self.free_bits[word] |= mask;
        self.recycled += 1;
    }

    fn clear_recycled(&mut self, id: usize) {
        debug_assert!(id < self.limit);
        let word = id / WORD_BITS;
        let bit = id % WORD_BITS;
        let mask = 1usize << bit;
        debug_assert_ne!(self.free_bits[word] & mask, 0);
        self.free_bits[word] &= !mask;
        self.recycled -= 1;
    }
}
