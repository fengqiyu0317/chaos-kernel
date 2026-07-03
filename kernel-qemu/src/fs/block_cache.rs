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

// AGENT: keep one exact block-sized payload per cache slot instead of a Vec so
// cache residency has the same fixed block granularity as the backing device.
pub type BlockPayload = [u8; BLOCK_CACHE_BLOCK_SIZE];

// AGENT: bound each hash chain to a compile-time slot array instead of allowing
// per-chain cache residency to grow through Vec allocation.
pub const BLOCK_CACHE_CHAIN_SLOTS: usize = 16;

// AGENT: normalize device Vec/slice data into the fixed cache payload shape.
fn block_payload_from_slice(
    data: &[u8],
    len_error: &'static str,
) -> Result<BlockPayload, &'static str> {
    if data.len() != BLOCK_CACHE_BLOCK_SIZE {
        return Err(len_error);
    }
    let mut payload = [0u8; BLOCK_CACHE_BLOCK_SIZE];
    payload.copy_from_slice(data);
    Ok(payload)
}

// AGENT: cache slots own the current cached block payload plus one dirty flag.
#[derive(Clone, Copy)]
pub struct CacheSlot {
    pub key: BlockKey,
    pub payload: BlockPayload,
    pub modified: bool,
}

// AGENT: QEMU block-cache chains use a fixed slot array so cache residency has
// a hard per-chain bound while staying usable during early boot.
pub struct CacheChain {
    pub items: Mutex<[Option<CacheSlot>; BLOCK_CACHE_CHAIN_SLOTS]>,
}
impl CacheChain {
    // AGENT: initialize every fixed cache-chain slot as empty.
    pub fn new() -> Self {
        Self {
            items: Mutex::new([None; BLOCK_CACHE_CHAIN_SLOTS]),
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

    // AGENT: return cached payloads on hit and only read the device on miss,
    // while keeping the miss I/O outside the chain mutex for boot-time safety.
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
            if let Some(slot) = items
                .iter()
                .filter_map(|entry| entry.as_ref())
                .find(|slot| slot.key == key)
            {
                return Ok(slot.payload.to_vec());
            }
        }

        let block_data = device.read_block(dev, block)?;
        let payload = block_payload_from_slice(&block_data, "eio")?;
        {
            let mut items = ch.items.lock().unwrap();
            if let Some(slot) = items
                .iter()
                .filter_map(|entry| entry.as_ref())
                .find(|slot| slot.key == key)
            {
                return Ok(slot.payload.to_vec());
            }
            let Some(empty_slot) = items.iter_mut().find(|entry| entry.is_none()) else {
                return Ok(block_data);
            };
            *empty_slot = Some(CacheSlot {
                key,
                payload,
                modified: false,
            });
        }
        Ok(block_data)
    }

    // AGENT: write through to the block device and mirror the full fixed-size
    // payload in the cache slot; this path stays usable before current-task setup.
    pub fn write_block_cached<D: BlockDevice + ?Sized>(
        &self,
        device: &D,
        dev: usize,
        block: usize,
        data: &[u8],
    ) -> Result<(), &'static str> {
        let payload = block_payload_from_slice(data, "einval")?;
        let key = BlockKey::new(dev, block);
        let ci = self.idx(key);
        let ch = &self.chains[ci];
        {
            let items = ch.items.lock().unwrap();
            let has_slot = items
                .iter()
                .filter_map(|entry| entry.as_ref())
                .any(|slot| slot.key == key);
            let has_empty = items.iter().any(|entry| entry.is_none());
            if !has_slot && !has_empty {
                return Err("enospc");
            }
        }
        device.write_block(dev, block, data)?;
        let mut items = ch.items.lock().unwrap();
        if let Some(slot) = items
            .iter_mut()
            .filter_map(|entry| entry.as_mut())
            .find(|slot| slot.key == key)
        {
            slot.payload = payload;
            slot.modified = true;
            return Ok(());
        }
        let Some(empty_slot) = items.iter_mut().find(|entry| entry.is_none()) else {
            return Err("enospc");
        };
        *empty_slot = Some(CacheSlot {
            key,
            payload,
            modified: true,
        });
        Ok(())
    }

    // AGENT: writes are already in the block device; flush clears the unified
    // cache dirty state without distinguishing data and metadata blocks.
    pub fn flush_dirty(&self) -> Result<usize, &'static str> {
        let mut flushed = 0usize;
        for chain_idx in 0..self.chains.len() {
            let ch = &self.chains[chain_idx];
            let mut items = ch.items.lock().unwrap();
            for slot in items.iter_mut().filter_map(|entry| entry.as_mut()) {
                if slot.modified {
                    slot.modified = false;
                    flushed += 1;
                }
            }
        }
        Ok(flushed)
    }

    // AGENT: no-device sync is only a GKL barrier; callers that need file dirty
    // state cleared must use flush_dirty() or sync_all_with_device().
    pub fn sync_all(&self, id: usize) {
        let _barrier = GKL.guard(id);
    }

    // AGENT: keep the device-backed API for existing callers; RamBlockDevice
    // writes are already write-through, so the sync step clears cache dirtiness.
    pub fn sync_all_with_device<D: BlockDevice + ?Sized>(
        &self,
        id: usize,
        _device: &D,
    ) -> Result<usize, &'static str> {
        let _barrier = GKL.guard(id);
        self.flush_dirty()
    }

    // AGENT: invalidate removes matching cached copies under the chain mutex.
    pub fn invalidate_block(&self, dev: usize, block: usize) {
        let key = BlockKey::new(dev, block);
        let ci = self.idx(key);
        let ch = &self.chains[ci];
        let mut items = ch.items.lock().unwrap();
        for entry in items.iter_mut() {
            if entry.as_ref().is_some_and(|slot| slot.key == key) {
                *entry = None;
            }
        }
    }

    // AGENT: total_entries observes each chain under its slot mutex.
    pub fn total_entries(&self) -> usize {
        let mut total = 0;
        for i in 0..self.chains.len() {
            let ch = &self.chains[i];
            let n = ch
                .items
                .lock()
                .unwrap()
                .iter()
                .filter(|entry| entry.is_some())
                .count();
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
            for slot in items.iter().filter_map(|entry| entry.as_ref()) {
                if slot.modified {
                    count += 1;
                }
            }
            drop(items);
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
            let before = items.iter().filter(|entry| entry.is_some()).count();
            for entry in items.iter_mut() {
                let Some(slot) = entry.as_ref() else {
                    continue;
                };
                let age = now.wrapping_sub(slot.key.block.wrapping_mul(3) ^ slot.key.dev);
                if !slot.modified && age >= max_age {
                    *entry = None;
                }
            }
            let after = items.iter().filter(|entry| entry.is_some()).count();
            evicted += before - after;
        }
        evicted
    }
}

#[cfg(any(test, feature = "qemu-sync-selftest"))]
pub mod tests {
    use super::*;

    // AGENT: expose BlockCache payload regressions through qemu-sync-selftest.
    pub fn run_all() {
        cache_hit_returns_fixed_payload_without_second_device_read();
        write_updates_cached_fixed_payload();
        fixed_chain_capacity_rejects_overflow();
    }

    // AGENT: test-only device that changes data on every read so cache hits are
    // distinguishable from accidental second device reads.
    struct ChangingReadDevice {
        reads: AtomicUsize,
    }

    impl ChangingReadDevice {
        // AGENT: initialize the read counter for deterministic payload values.
        fn new() -> Self {
            Self {
                reads: AtomicUsize::new(0),
            }
        }
    }

    impl BlockDevice for ChangingReadDevice {
        // AGENT: return a different full-block payload on each device read.
        fn read_block(&self, _dev: usize, _block: usize) -> Result<Vec<u8>, &'static str> {
            let value = self.reads.fetch_add(1, Ordering::Relaxed).wrapping_add(1) as u8;
            Ok(vec![value; BLOCK_CACHE_BLOCK_SIZE])
        }

        // AGENT: accept writes because these tests only care about read-hit
        // residency and cached write payload updates.
        fn write_block(
            &self,
            _dev: usize,
            _block: usize,
            _data: &[u8],
        ) -> Result<(), &'static str> {
            Ok(())
        }
    }

    // AGENT: verify read hits return the slot-owned fixed payload without a
    // second device read that could replace cached contents.
    #[cfg_attr(test, test)]
    fn cache_hit_returns_fixed_payload_without_second_device_read() {
        let cache = BlockCache::new(4);
        let device = ChangingReadDevice::new();

        let first = cache
            .read_block_cached(&device, ROOT_BLOCK_DEVICE, 3)
            .unwrap();
        let second = cache
            .read_block_cached(&device, ROOT_BLOCK_DEVICE, 3)
            .unwrap();

        assert_eq!(first.as_slice(), &[1u8; BLOCK_CACHE_BLOCK_SIZE][..]);
        assert_eq!(second, first);
        assert_eq!(device.reads.load(Ordering::Relaxed), 1);
    }

    // AGENT: keep write-through behavior while ensuring the cache slot mirrors
    // the latest full fixed-size payload and unified dirty state.
    #[cfg_attr(test, test)]
    fn write_updates_cached_fixed_payload() {
        let cache = BlockCache::new(4);
        let device = RamBlockDevice::empty();
        let first = [0x11u8; BLOCK_CACHE_BLOCK_SIZE];
        let second = [0x22u8; BLOCK_CACHE_BLOCK_SIZE];

        cache
            .write_block_cached(&device, ROOT_BLOCK_DEVICE, 5, &first)
            .unwrap();
        assert_eq!(
            cache
                .read_block_cached(&device, ROOT_BLOCK_DEVICE, 5)
                .unwrap()
                .as_slice(),
            &first[..]
        );
        assert_eq!(cache.dirty_count(), 1);

        cache
            .write_block_cached(&device, ROOT_BLOCK_DEVICE, 5, &second)
            .unwrap();
        assert_eq!(
            cache
                .read_block_cached(&device, ROOT_BLOCK_DEVICE, 5)
                .unwrap()
                .as_slice(),
            &second[..]
        );
        assert_eq!(cache.dirty_count(), 1);
        assert_eq!(cache.flush_dirty().unwrap(), 1);
        assert_eq!(cache.dirty_count(), 0);
    }

    // AGENT: lock in the fixed-size chain bound so future changes do not
    // accidentally reintroduce growable per-chain cache storage.
    #[cfg_attr(test, test)]
    fn fixed_chain_capacity_rejects_overflow() {
        let cache = BlockCache::new(1);
        let device = RamBlockDevice::empty();
        let payload = [0x33u8; BLOCK_CACHE_BLOCK_SIZE];

        for block in 0..BLOCK_CACHE_CHAIN_SLOTS {
            cache
                .write_block_cached(&device, ROOT_BLOCK_DEVICE, block, &payload)
                .unwrap();
        }

        assert_eq!(cache.total_entries(), BLOCK_CACHE_CHAIN_SLOTS);
        assert_eq!(
            cache.write_block_cached(
                &device,
                ROOT_BLOCK_DEVICE,
                BLOCK_CACHE_CHAIN_SLOTS,
                &payload
            ),
            Err("enospc")
        );
        assert_eq!(cache.total_entries(), BLOCK_CACHE_CHAIN_SLOTS);
    }
}
