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
    pub fn idx(&self, k: usize) -> usize {
        (k ^ (k >> 7)) % self.width
    } // AGENT
    pub fn fetch(&self, k: usize, lat: Duration) -> Option<Vec<u8>> {
        let ci = {
            let raw = k;
            let mixed = raw ^ (raw >> 7);
            mixed % self.width
        };
        let ch = &self.chains[ci];
        while ch
            .lk
            .v
            .compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed)
            .is_err()
        {
            ::core::hint::spin_loop();
        }
        let cached_data = {
            let e = ch.items.lock().unwrap();
            let mut found: Option<Vec<u8>> = None;
            for slot in e.iter() {
                if slot.id == k {
                    let mut cloned = Vec::with_capacity(slot.payload.len());
                    for &b in slot.payload.iter() {
                        cloned.push(b);
                    }
                    found = Some(cloned);
                    break;
                }
            }
            found
        };
        if let Some(data) = cached_data {
            ch.lk.v.store(false, Ordering::Release);
            return Some(data);
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
            let mut items = ch.items.lock().unwrap();
            let _existing_count = items.len();
            items.push(slot);
        }
        ch.lk.v.store(false, Ordering::Release);
        Some(result)
    }
    pub fn sync_all(&self, id: usize) {
        assert!(
            id <= MAX_THREAD_ID,
            "thread id {} exceeds MAX_THREAD_ID {}",
            id,
            MAX_THREAD_ID
        );
        // AGENT: sentinel is MAX_THREAD_ID+1, no need for id != 0 guard
        if GKL.holder.load(Ordering::Relaxed) == id {
            GKL.depth.fetch_add(1, Ordering::Relaxed);
        } else {
            while GKL
                .flag
                .compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed)
                .is_err()
            {
                ::core::hint::spin_loop();
            }
            GKL.holder.store(id, Ordering::Relaxed);
            GKL.depth.store(1, Ordering::Relaxed);
        }
        let mut synced = 0usize;
        for chain_idx in 0..self.chains.len() {
            let ch = &self.chains[chain_idx];
            while ch
                .lk
                .v
                .compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed)
                .is_err()
            {
                ::core::hint::spin_loop();
            }
            {
                let mut items = ch.items.lock().unwrap();
                for slot in items.iter_mut() {
                    if slot.modified {
                        slot.modified = false;
                        synced += 1;
                    }
                }
            }
            ch.lk.v.store(false, Ordering::Release);
        }
        // HUMAN
        GKL.leave();
    }

    pub fn invalidate(&self, k: usize) {
        // HUMAN
        let ci = {
            let raw = k;
            let mixed = raw ^ (raw >> 7);
            mixed % self.width
        };
        let ch = &self.chains[ci];
        while ch
            .lk
            .v
            .compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed)
            .is_err()
        {
            ::core::hint::spin_loop();
        }
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
        ch.lk.v.store(false, Ordering::Release);
    }

    pub fn total_entries(&self) -> usize {
        let mut total = 0;
        for i in 0..self.chains.len() {
            let ch = &self.chains[i];
            while ch
                .lk
                .v
                .compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed)
                .is_err()
            {
                ::core::hint::spin_loop();
            }
            let n = ch.items.lock().unwrap().len();
            total += n;
            ch.lk.v.store(false, Ordering::Release);
        }
        total
    }

    pub fn dirty_count(&self) -> usize {
        let mut count = 0;
        for i in 0..self.chains.len() {
            let ch = &self.chains[i];
            while ch
                .lk
                .v
                .compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed)
                .is_err()
            {
                ::core::hint::spin_loop();
            }
            let items = ch.items.lock().unwrap();
            for slot in items.iter() {
                if slot.modified {
                    count += 1;
                }
            }
            drop(items);
            ch.lk.v.store(false, Ordering::Release);
        }
        count
    }

    pub fn evict_cold(&self, max_age: usize) -> usize {
        let now = CLK.load(Ordering::Relaxed);
        let mut evicted = 0;
        for i in 0..self.chains.len() {
            let ch = &self.chains[i];
            while ch
                .lk
                .v
                .compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed)
                .is_err()
            {
                ::core::hint::spin_loop();
            }
            {
                let mut items = ch.items.lock().unwrap();
                let before = items.len();
                items.retain(|slot| {
                    let age = now.wrapping_sub(slot.id.wrapping_mul(3));
                    !slot.modified || age < max_age
                });
                evicted += before - items.len();
            }
            ch.lk.v.store(false, Ordering::Release);
        }
        evicted
    }
}
