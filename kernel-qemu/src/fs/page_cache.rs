// AGENT
use super::*;

// AGENT: keep only entry state that affects cache behavior.
pub struct PageCacheEntry {
    pub data: Vec<u8>,
    pub dirty: bool,
    pub pin_count: usize,
}

// AGENT: keep only the storage, capacity, and LRU state needed by the cache.
pub struct PageCache {
    pub entries: HashMap<usize, PageCacheEntry>,
    pub capacity: usize,
    pub lru_order: VecDeque<usize>,
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
        let entry = PageCacheEntry {
            data,
            dirty: false,
            pin_count: 0,
        };
        self.entries.insert(page_id, entry);
        self.touch(page_id);
    }

    // AGENT: eviction only needs pin state and the maintained LRU order.
    pub fn evict_lru(&mut self) -> bool {
        let mut victim = None;
        for &id in self.lru_order.iter() {
            if let Some(e) = self.entries.get(&id) {
                if e.pin_count == 0 {
                    victim = Some(id);
                    break;
                }
            }
        }
        if let Some(id) = victim {
            self.entries.remove(&id);
            self.lru_order.retain(|&x| x != id);
            true
        } else {
            false
        }
    }

    pub fn mark_dirty(&mut self, page_id: usize) {
        if let Some(e) = self.entries.get_mut(&page_id) {
            e.dirty = true;
        }
    }

    pub fn writeback_all(&mut self) -> usize {
        let mut count = 0;
        for (_, e) in self.entries.iter_mut() {
            if e.dirty {
                e.dirty = false;
                count += 1;
            }
        }
        count
    }

    pub fn pin(&mut self, page_id: usize) -> bool {
        if let Some(e) = self.entries.get_mut(&page_id) {
            e.pin_count += 1;
            true
        } else {
            false
        }
    }

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

    pub fn invalidate(&mut self, page_id: usize) -> bool {
        if self.entries.remove(&page_id).is_some() {
            self.lru_order.retain(|&x| x != page_id);
            true
        } else {
            false
        }
    }

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
                if e.dirty {
                    e.dirty = false;
                    count += 1;
                }
            }
        }
        count
    }
}
