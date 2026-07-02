// AGENT
use super::*;

pub const BLOCK_CACHE_BLOCK_SIZE: usize = 512;
pub const ROOT_BLOCK_DEVICE: usize = 0;

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

// AGENT: narrow block-device interface used by BlockCache; concrete QEMU
// drivers can later implement this over virtio-blk or another real device.
pub trait BlockDevice {
    fn read_block(&self, dev: usize, block: usize) -> Result<Vec<u8>, &'static str>;
    fn write_block(&self, dev: usize, block: usize, data: &[u8]) -> Result<(), &'static str>;
}

// AGENT: fixed-size in-memory block device used as the first QEMU-side backend
// for initramfs or embedded init images before virtio-blk is introduced.
pub struct RamBlockDevice {
    valid_len: AtomicUsize,
    data: Mutex<Vec<u8>>,
}

impl RamBlockDevice {
    // AGENT: build a RAM block device from bytes and pad the backing store to a
    // complete block while preserving the original payload length.
    pub fn from_bytes(bytes: &[u8]) -> Self {
        let mut data = bytes.to_vec();
        let valid_len = data.len();
        if valid_len > 0 {
            let rem = valid_len % BLOCK_CACHE_BLOCK_SIZE;
            if rem != 0 {
                data.resize(valid_len + (BLOCK_CACHE_BLOCK_SIZE - rem), 0);
            }
        }
        Self {
            valid_len: AtomicUsize::new(valid_len),
            data: Mutex::new(data),
        }
    }

    // AGENT: keep the default kernel backend explicit when no boot image has
    // been linked yet.
    pub fn empty() -> Self {
        Self::from_bytes(&[])
    }

    pub fn byte_len(&self) -> usize {
        self.valid_len.load(Ordering::Relaxed)
    }
}

impl BlockDevice for RamBlockDevice {
    fn read_block(&self, _dev: usize, block: usize) -> Result<Vec<u8>, &'static str> {
        let start = block.checked_mul(BLOCK_CACHE_BLOCK_SIZE).ok_or("eio")?;
        let end = start.checked_add(BLOCK_CACHE_BLOCK_SIZE).ok_or("eio")?;
        let data = self.data.lock().unwrap();
        if end > data.len() {
            return Err("eio");
        }
        Ok(data[start..end].to_vec())
    }

    fn write_block(&self, _dev: usize, block: usize, data: &[u8]) -> Result<(), &'static str> {
        if data.len() != BLOCK_CACHE_BLOCK_SIZE {
            return Err("einval");
        }
        let start = block.checked_mul(BLOCK_CACHE_BLOCK_SIZE).ok_or("eio")?;
        let end = start.checked_add(BLOCK_CACHE_BLOCK_SIZE).ok_or("eio")?;
        let mut backing = self.data.lock().unwrap();
        if end > backing.len() {
            return Err("eio");
        }
        backing[start..end].copy_from_slice(data);
        let mut old_len = self.valid_len.load(Ordering::Relaxed);
        while end > old_len {
            match self.valid_len.compare_exchange(
                old_len,
                end,
                Ordering::Relaxed,
                Ordering::Relaxed,
            ) {
                Ok(_) => break,
                Err(current) => old_len = current,
            }
        }
        Ok(())
    }
}

pub struct CacheSlot {
    pub key: BlockKey,
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
    pub fn idx(&self, key: BlockKey) -> usize {
        key.hash() % self.width
    }

    // AGENT: read cached blocks through an explicit block device; miss I/O is
    // outside the chain SpinGuard and insertion double-checks for races.
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
            let _guard = ch.lk.guard();
            let e = ch.items.lock().unwrap();
            if let Some(slot) = e.iter().find(|slot| slot.key == key) {
                return Ok(slot.payload.clone());
            }
        }

        let block_data = device.read_block(dev, block)?;
        if block_data.len() != BLOCK_CACHE_BLOCK_SIZE {
            return Err("eio");
        }
        let result = block_data.clone();
        let slot = CacheSlot {
            key,
            payload: block_data,
            modified: false,
        };
        {
            let _guard = ch.lk.guard();
            let mut items = ch.items.lock().unwrap();
            if let Some(slot) = items.iter().find(|slot| slot.key == key) {
                return Ok(slot.payload.clone());
            }
            items.push(slot);
        }
        Ok(result)
    }

    // AGENT: update or insert one complete cached block and mark it dirty for a
    // later flush through the block-device interface.
    pub fn write_block_cached(
        &self,
        dev: usize,
        block: usize,
        data: &[u8],
    ) -> Result<(), &'static str> {
        if data.len() != BLOCK_CACHE_BLOCK_SIZE {
            return Err("einval");
        }
        let key = BlockKey::new(dev, block);
        let ci = self.idx(key);
        let ch = &self.chains[ci];
        let _guard = ch.lk.guard();
        let mut items = ch.items.lock().unwrap();
        if let Some(slot) = items.iter_mut().find(|slot| slot.key == key) {
            slot.payload.clear();
            slot.payload.extend_from_slice(data);
            slot.modified = true;
            return Ok(());
        }
        items.push(CacheSlot {
            key,
            payload: data.to_vec(),
            modified: true,
        });
        Ok(())
    }

    // AGENT: write dirty blocks outside cache-chain SpinGuards, then clear the
    // dirty bit only if the cached payload did not change during writeback.
    pub fn flush_dirty<D: BlockDevice + ?Sized>(&self, device: &D) -> Result<usize, &'static str> {
        let mut flushed = 0usize;
        for chain_idx in 0..self.chains.len() {
            let ch = &self.chains[chain_idx];
            let dirty = {
                let _guard = ch.lk.guard();
                let items = ch.items.lock().unwrap();
                items
                    .iter()
                    .filter(|slot| slot.modified)
                    .map(|slot| (slot.key, slot.payload.clone()))
                    .collect::<Vec<_>>()
            };

            for (key, payload) in dirty {
                device.write_block(key.dev, key.block, &payload)?;
                let _guard = ch.lk.guard();
                let mut items = ch.items.lock().unwrap();
                if let Some(slot) = items.iter_mut().find(|slot| slot.key == key) {
                    if slot.modified && slot.payload == payload {
                        slot.modified = false;
                        flushed += 1;
                    }
                }
            }
        }
        Ok(flushed)
    }

    // AGENT: keep the legacy no-device sync entry as a GKL-only barrier; dirty
    // cache entries must be flushed through flush_dirty() or sync_all_with_device().
    pub fn sync_all(&self, id: usize) {
        let _gkl = GKL.guard(id);
    }

    // AGENT: sync with a device performs real dirty writeback instead of
    // clearing cache state without I/O.
    pub fn sync_all_with_device<D: BlockDevice + ?Sized>(
        &self,
        id: usize,
        device: &D,
    ) -> Result<usize, &'static str> {
        // AGENT: route GKL through the guard so Drop performs owner-checked release.
        let _gkl = GKL.guard(id);
        self.flush_dirty(device)
    }

    // AGENT: invalidate uses SpinGuard so early exits cannot leak the chain lock.
    pub fn invalidate_block(&self, dev: usize, block: usize) {
        let key = BlockKey::new(dev, block);
        let ci = self.idx(key);
        let ch = &self.chains[ci];
        let _guard = ch.lk.guard();
        {
            let mut items = ch.items.lock().unwrap();
            let mut idx = 0;
            while idx < items.len() {
                if items[idx].key == key {
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
                    let age = now.wrapping_sub(slot.key.block.wrapping_mul(3) ^ slot.key.dev);
                    slot.modified || age < max_age
                });
                evicted += before - items.len();
            }
        }
        evicted
    }
}
