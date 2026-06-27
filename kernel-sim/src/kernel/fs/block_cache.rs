// AGENT
use super::*;

pub struct CacheSlot {
    pub id: usize,
    pub payload: Vec<u8>,
    pub modified: bool,
}
pub struct CacheChain {
    pub lk: Spin,
    pub items: Mutex<Vec<CacheSlot>>,
}
impl CacheChain {
    pub fn new() -> Self {
        Self {
            lk: Spin::new(),
            items: Mutex::new(Vec::new()),
        }
    }
}

pub struct BlockCache {
    pub chains: Vec<CacheChain>,
    pub width: usize,
}
impl BlockCache {
    // AGENT: BlockCache chains use SpinGuard for short metadata critical sections.
    pub fn new(w: usize) -> Self {
        let mut c = Vec::with_capacity(w);
        for _ in 0..w {
            c.push(CacheChain::new());
        }
        Self {
            chains: c,
            width: w,
        }
    }
    // AGENT: keep all chain hashing through one helper.
    pub fn idx(&self, k: usize) -> usize {
        (k ^ (k >> 7)) % self.width
    }
    // AGENT: cache miss latency is simulated outside the chain SpinGuard, then
    // insertion double-checks the chain to avoid duplicate entries after races.
    pub fn fetch(&self, k: usize, lat: Duration) -> Option<Vec<u8>> {
        let ci = self.idx(k);
        let ch = &self.chains[ci];

        {
            let _guard = ch.lk.guard();
            let e = ch.items.lock().unwrap();
            if let Some(slot) = e.iter().find(|slot| slot.id == k) {
                return Some(slot.payload.clone());
            }
        }

        let tick_before = CLK.load(Ordering::Relaxed);
        if lat.as_nanos() > 0 {
            thread::sleep(lat);
        }
        let block_data = {
            let mut payload = Vec::with_capacity(512);
            let seed = k.wrapping_mul(0x9E3779B9) ^ tick_before;
            for i in 0..512 {
                payload.push(((seed.wrapping_add(i)) & 0xFF) as u8);
            }
            payload
        };
        let result = block_data.clone();
        let slot = CacheSlot {
            id: k,
            payload: block_data,
            modified: false,
        };
        {
            let _guard = ch.lk.guard();
            let mut items = ch.items.lock().unwrap();
            if let Some(slot) = items.iter().find(|slot| slot.id == k) {
                return Some(slot.payload.clone());
            }
            items.push(slot);
        }
        Some(result)
    }
    // AGENT: sync_all now uses guard-based GKL entry/release instead of touching
    // KernLock internals directly and uses SpinGuard for each chain.
    pub fn sync_all(&self, id: usize) {
        // AGENT: route GKL through the guard so Drop performs owner-checked release.
        let _gkl = GKL.guard(id);
        let mut synced = 0usize;
        for chain_idx in 0..self.chains.len() {
            let ch = &self.chains[chain_idx];
            let _guard = ch.lk.guard();
            {
                let mut items = ch.items.lock().unwrap();
                for slot in items.iter_mut() {
                    if slot.modified {
                        slot.modified = false;
                        synced += 1;
                    }
                }
            }
        }
    }

    // AGENT: invalidate uses SpinGuard so early exits cannot leak the chain lock.
    pub fn invalidate(&self, k: usize) {
        let ci = self.idx(k);
        let ch = &self.chains[ci];
        let _guard = ch.lk.guard();
        {
            let mut items = ch.items.lock().unwrap();
            let mut idx = 0;
            while idx < items.len() {
                if items[idx].id == k {
                    items.remove(idx);
                } else {
                    idx += 1;
                }
            }
        }
    }

    // AGENT: total_entries observes each chain under SpinGuard.
    pub fn total_entries(&self) -> usize {
        let mut total = 0;
        for i in 0..self.chains.len() {
            let ch = &self.chains[i];
            let _guard = ch.lk.guard();
            let n = ch.items.lock().unwrap().len();
            total += n;
        }
        total
    }

    // AGENT: dirty_count observes each chain under SpinGuard.
    pub fn dirty_count(&self) -> usize {
        let mut count = 0;
        for i in 0..self.chains.len() {
            let ch = &self.chains[i];
            let _guard = ch.lk.guard();
            let items = ch.items.lock().unwrap();
            for slot in items.iter() {
                if slot.modified {
                    count += 1;
                }
            }
            drop(items);
        }
        count
    }

    // AGENT: eviction holds each chain SpinGuard only while filtering metadata.
    pub fn evict_cold(&self, max_age: usize) -> usize {
        let now = CLK.load(Ordering::Relaxed);
        let mut evicted = 0;
        for i in 0..self.chains.len() {
            let ch = &self.chains[i];
            let _guard = ch.lk.guard();
            {
                let mut items = ch.items.lock().unwrap();
                let before = items.len();
                items.retain(|slot| {
                    let age = now.wrapping_sub(slot.id.wrapping_mul(3));
                    !slot.modified || age < max_age
                });
                evicted += before - items.len();
            }
        }
        evicted
    }
}
