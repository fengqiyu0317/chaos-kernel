// AGENT
use super::*;

// AGENT: keep only entry state that affects cache behavior.
struct PageCacheEntry {
    data: Vec<u8>,
    dirty: bool,
    pin_count: usize,
}

impl PageCacheEntry {
    // AGENT: new cache entries start clean because insert() represents data
    // already loaded from backing storage.
    fn clean(data: Vec<u8>) -> Self {
        Self {
            data,
            dirty: false,
            pin_count: 0,
        }
    }

    // AGENT: dirty pages must survive LRU pressure until an explicit writeback
    // path marks them clean.
    fn can_evict(&self) -> bool {
        self.pin_count == 0 && !self.dirty
    }

    // AGENT: centralize dirty-bit clearing so writeback helpers count exactly
    // the pages they changed.
    fn mark_clean(&mut self) -> bool {
        if !self.dirty {
            return false;
        }
        self.dirty = false;
        true
    }
}

// AGENT: keep only the storage, capacity, and LRU state needed by the cache.
pub struct PageCache {
    entries: HashMap<usize, PageCacheEntry>,
    capacity: usize,
    lru_order: VecDeque<usize>,
}

impl PageCache {
    // AGENT: initialize the minimal page-cache state.
    pub fn new(capacity: usize) -> Self {
        Self {
            entries: HashMap::new(),
            capacity,
            lru_order: VecDeque::new(),
        }
    }

    // AGENT: keep the LRU update in one place so lookup and insert cannot
    // drift into different recency rules.
    fn touch(&mut self, page_id: usize) {
        self.lru_order.retain(|&id| id != page_id);
        self.lru_order.push_back(page_id);
    }

    // AGENT: choose the oldest page that can be discarded without writeback.
    fn lru_victim(&self) -> Option<usize> {
        self.lru_order.iter().copied().find(|id| {
            self.entries
                .get(id)
                .map(PageCacheEntry::can_evict)
                .unwrap_or(false)
        })
    }

    // AGENT: keep entry removal and LRU cleanup together so callers cannot
    // forget one half of the cache state.
    fn remove_entry(&mut self, page_id: usize) -> bool {
        if self.entries.remove(&page_id).is_none() {
            return false;
        }
        self.lru_order.retain(|&id| id != page_id);
        true
    }

    // AGENT: use lru_order as the single source of recency state.
    pub fn lookup(&mut self, page_id: usize) -> Option<&[u8]> {
        self.entries.get(&page_id)?;
        self.touch(page_id);
        self.entries.get(&page_id).map(|e| e.data.as_slice())
    }

    // AGENT: replace an existing page in place so lru_order never keeps
    // duplicate entries for the same page id.
    pub fn insert(&mut self, page_id: usize, data: Vec<u8>) {
        if let Some(entry) = self.entries.get_mut(&page_id) {
            entry.data = data;
            entry.dirty = false;
            self.touch(page_id);
            return;
        }
        if self.capacity == 0 {
            return;
        }
        if self.entries.len() >= self.capacity && !self.evict_lru() {
            return;
        }
        self.entries.insert(page_id, PageCacheEntry::clean(data));
        self.touch(page_id);
    }

    // AGENT: evict only clean, unpinned pages; dirty pages need an explicit
    // writeback path before they can be discarded.
    pub fn evict_lru(&mut self) -> bool {
        self.lru_victim()
            .map(|page_id| self.remove_entry(page_id))
            .unwrap_or(false)
    }

    // AGENT: record that a cached page now has data not yet safe to evict.
    pub fn mark_dirty(&mut self, page_id: usize) {
        if let Some(e) = self.entries.get_mut(&page_id) {
            e.dirty = true;
        }
    }

    // AGENT: mark all dirty pages clean after the caller's writeback boundary.
    pub fn writeback_all(&mut self) -> usize {
        let mut count = 0;
        for e in self.entries.values_mut() {
            if e.mark_clean() {
                count += 1;
            }
        }
        count
    }

    // AGENT: pinned pages are protected from LRU eviction.
    pub fn pin(&mut self, page_id: usize) -> bool {
        if let Some(e) = self.entries.get_mut(&page_id) {
            e.pin_count += 1;
            true
        } else {
            false
        }
    }

    // AGENT: unpin saturates at zero so repeated cleanup calls do not underflow.
    pub fn unpin(&mut self, page_id: usize) -> bool {
        if let Some(e) = self.entries.get_mut(&page_id) {
            if e.pin_count > 0 {
                e.pin_count -= 1;
            }
            true
        } else {
            false
        }
    }

    // AGENT: invalidation is an explicit discard and keeps the LRU queue in sync.
    pub fn invalidate(&mut self, page_id: usize) -> bool {
        self.remove_entry(page_id)
    }

    // AGENT: mark dirty pages clean after the caller flushes this page range.
    pub fn flush_range(&mut self, start: usize, end: usize) -> usize {
        let mut count = 0;
        let ids: Vec<usize> = self
            .entries
            .keys()
            .filter(|&&id| id >= start && id < end)
            .copied()
            .collect();
        for id in ids {
            if let Some(e) = self.entries.get_mut(&id) {
                if e.mark_clean() {
                    count += 1;
                }
            }
        }
        count
    }
}
