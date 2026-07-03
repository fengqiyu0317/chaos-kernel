// AGENT
use super::block_device::{BlockDevice, BLOCK_CACHE_BLOCK_SIZE};
use super::*;

// AGENT: identify cached data by block-device namespace plus block number
// instead of overloading file-descriptor ids as cache keys.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BlockKey {
    pub dev: usize,
    pub block: usize,
}

impl BlockKey {
    pub const fn new(dev: usize, block: usize) -> Self {
        Self { dev, block }
    }

    fn hash(self) -> usize {
        let mut h = self.block ^ (self.block >> 7);
        h ^= self.dev.wrapping_mul(0x9E37_79B9);
        h ^ (h >> 11)
    }
}

// AGENT: let BlockCache own the data-vs-metadata dirty distinction needed by
// fsync/fdatasync-style callers instead of keeping a parallel FileNode flag.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CachedBlockKind {
    Data,
    Metadata,
}

pub struct CacheSlot {
    pub key: BlockKey,
    pub payload: Vec<u8>,
    pub kind: CachedBlockKind,
    pub modified: bool,
}

// AGENT: QEMU block-cache chains are usable during early boot, before the
// scheduler has installed a current task. Keep their locking independent from
// task-owned Spin and protect the slots with this mutex only.
pub struct CacheChain {
    pub items: Mutex<Vec<CacheSlot>>,
}
impl CacheChain {
    pub fn new() -> Self {
        Self {
            items: Mutex::new(Vec::new()),
        }
    }
}

pub struct BlockCache {
    pub chains: Vec<CacheChain>,
    pub width: usize,
}
impl BlockCache {
    // AGENT: BlockCache chains are sharded by block key and each chain owns its
    // slot mutex, so cache operations also work before current-task setup.
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
    pub fn idx(&self, key: BlockKey) -> usize {
        key.hash() % self.width
    }

    // AGENT: read cached blocks with only the chain mutex so boot-time block
    // reads work before the scheduler has installed a current task.
    pub fn read_block_cached<D: BlockDevice + ?Sized>(
        &self,
        device: &D,
        dev: usize,
        block: usize,
    ) -> Result<Vec<u8>, &'static str> {
        let key = BlockKey::new(dev, block);
        let ci = self.idx(key);
        let ch = &self.chains[ci];

        {
            let items = ch.items.lock().unwrap();
            if let Some(slot) = items.iter().find(|slot| slot.key == key) {
                return Ok(slot.payload.clone());
            }
        }

        let block_data = device.read_block(dev, block)?;
        if block_data.len() != BLOCK_CACHE_BLOCK_SIZE {
            return Err("eio");
        }
        {
            let mut items = ch.items.lock().unwrap();
            if let Some(slot) = items.iter().find(|slot| slot.key == key) {
                return Ok(slot.payload.clone());
            }
            items.push(CacheSlot {
                key,
                payload: block_data.clone(),
                kind: CachedBlockKind::Data,
                modified: false,
            });
        }
        Ok(block_data)
    }

    // AGENT: update or insert one complete cached block and mark it dirty for a
    // later flush through the block-device interface; this path must be usable
    // before current-task setup just like read_block_cached().
    pub fn write_block_cached(
        &self,
        dev: usize,
        block: usize,
        data: &[u8],
    ) -> Result<(), &'static str> {
        self.write_block_cached_as(dev, block, data, CachedBlockKind::Data)
    }

    // AGENT: write one complete cached block and tag its dirty class so metadata
    // writeback policy can live in BlockCache.
    pub fn write_block_cached_as(
        &self,
        dev: usize,
        block: usize,
        data: &[u8],
        kind: CachedBlockKind,
    ) -> Result<(), &'static str> {
        if data.len() != BLOCK_CACHE_BLOCK_SIZE {
            return Err("einval");
        }
        let key = BlockKey::new(dev, block);
        let ci = self.idx(key);
        let ch = &self.chains[ci];
        let mut items = ch.items.lock().unwrap();
        if let Some(slot) = items.iter_mut().find(|slot| slot.key == key) {
            slot.payload = data.to_vec();
            slot.kind = kind;
            slot.modified = true;
            return Ok(());
        }
        items.push(CacheSlot {
            key,
            payload: data.to_vec(),
            kind,
            modified: true,
        });
        Ok(())
    }

    fn flush_dirty_where<D, F>(&self, device: &D, mut include: F) -> Result<usize, &'static str>
    where
        D: BlockDevice + ?Sized,
        F: FnMut(CachedBlockKind) -> bool,
    {
        let mut flushed = 0usize;
        for chain_idx in 0..self.chains.len() {
            let ch = &self.chains[chain_idx];
            let dirty = {
                let items = ch.items.lock().unwrap();
                items
                    .iter()
                    .filter(|slot| slot.modified && include(slot.kind))
                    .map(|slot| (slot.key, slot.kind, slot.payload.clone()))
                    .collect::<Vec<_>>()
            };

            for (key, kind, payload) in dirty {
                device.write_block(key.dev, key.block, &payload)?;
                let mut items = ch.items.lock().unwrap();
                if let Some(slot) = items.iter_mut().find(|slot| slot.key == key) {
                    if slot.modified && slot.kind == kind && slot.payload == payload {
                        slot.modified = false;
                        flushed += 1;
                    }
                }
            }
        }
        Ok(flushed)
    }

    // AGENT: clone dirty payloads under the chain mutex, write them outside the
    // mutex, then clear the dirty bit only if the cached payload is unchanged.
    pub fn flush_dirty<D: BlockDevice + ?Sized>(&self, device: &D) -> Result<usize, &'static str> {
        self.flush_dirty_where(device, |_| true)
    }

    // AGENT: fdatasync-style writeback flushes data blocks while leaving cached
    // metadata blocks dirty for a later full sync.
    pub fn flush_dirty_data<D: BlockDevice + ?Sized>(
        &self,
        device: &D,
    ) -> Result<usize, &'static str> {
        self.flush_dirty_where(device, |kind| kind == CachedBlockKind::Data)
    }

    // AGENT: no-device sync is only a GKL barrier; dirty cache entries must use
    // flush_dirty() or sync_all_with_device() so writeback has a BlockDevice.
    pub fn sync_all(&self, id: usize) {
        let _barrier = GKL.guard(id);
    }

    // AGENT: device-backed sync is the real dirty writeback path.
    pub fn sync_all_with_device<D: BlockDevice + ?Sized>(
        &self,
        id: usize,
        device: &D,
    ) -> Result<usize, &'static str> {
        let _barrier = GKL.guard(id);
        self.flush_dirty(device)
    }

    // AGENT: invalidate removes matching cached copies under the chain mutex.
    pub fn invalidate_block(&self, dev: usize, block: usize) {
        let key = BlockKey::new(dev, block);
        let ci = self.idx(key);
        let ch = &self.chains[ci];
        let mut items = ch.items.lock().unwrap();
        items.retain(|slot| slot.key != key);
    }

    // AGENT: total_entries observes each chain under its slot mutex.
    pub fn total_entries(&self) -> usize {
        let mut total = 0;
        for i in 0..self.chains.len() {
            let ch = &self.chains[i];
            let n = ch.items.lock().unwrap().len();
            total += n;
        }
        total
    }

    // AGENT: dirty_count observes each chain under its slot mutex.
    pub fn dirty_count(&self) -> usize {
        let mut count = 0;
        for i in 0..self.chains.len() {
            let ch = &self.chains[i];
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

    // AGENT: observe one dirty class for focused fsync/fdatasync regressions.
    pub fn dirty_count_by_kind(&self, kind: CachedBlockKind) -> usize {
        let mut count = 0;
        for i in 0..self.chains.len() {
            let ch = &self.chains[i];
            let items = ch.items.lock().unwrap();
            for slot in items.iter() {
                if slot.modified && slot.kind == kind {
                    count += 1;
                }
            }
        }
        count
    }

    // AGENT: eviction filters each chain under its slot mutex.
    pub fn evict_cold(&self, max_age: usize) -> usize {
        let now = CLK.load(Ordering::Relaxed);
        let mut evicted = 0;
        for i in 0..self.chains.len() {
            let ch = &self.chains[i];
            let mut items = ch.items.lock().unwrap();
            let before = items.len();
            items.retain(|slot| {
                let age = now.wrapping_sub(slot.key.block.wrapping_mul(3) ^ slot.key.dev);
                slot.modified || age < max_age
            });
            evicted += before - items.len();
        }
        evicted
    }
}
