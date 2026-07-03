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

// AGENT: keep one exact block-sized payload per cache slot instead of a Vec so
// cache residency has the same fixed block granularity as the backing device.
pub type BlockPayload = [u8; BLOCK_CACHE_BLOCK_SIZE];

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

// AGENT: cache slots own the current cached block payload plus dirty metadata.
pub struct CacheSlot {
    pub key: BlockKey,
    pub payload: BlockPayload,
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
            if let Some(slot) = items.iter().find(|slot| slot.key == key) {
                return Ok(slot.payload.to_vec());
            }
        }

        let block_data = device.read_block(dev, block)?;
        let payload = block_payload_from_slice(&block_data, "eio")?;
        {
            let mut items = ch.items.lock().unwrap();
            if let Some(slot) = items.iter().find(|slot| slot.key == key) {
                return Ok(slot.payload.to_vec());
            }
            items.push(CacheSlot {
                key,
                payload,
                kind: CachedBlockKind::Data,
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
        self.write_block_cached_as(device, dev, block, data, CachedBlockKind::Data)
    }

    // AGENT: write one complete block into the device, update the cached
    // payload, and tag its dirty class for fdatasync/fsync distinction.
    pub fn write_block_cached_as<D: BlockDevice + ?Sized>(
        &self,
        device: &D,
        dev: usize,
        block: usize,
        data: &[u8],
        kind: CachedBlockKind,
    ) -> Result<(), &'static str> {
        let payload = block_payload_from_slice(data, "einval")?;
        device.write_block(dev, block, data)?;
        let key = BlockKey::new(dev, block);
        let ci = self.idx(key);
        let ch = &self.chains[ci];
        let mut items = ch.items.lock().unwrap();
        if let Some(slot) = items.iter_mut().find(|slot| slot.key == key) {
            slot.payload = payload;
            slot.kind = kind;
            slot.modified = true;
            return Ok(());
        }
        items.push(CacheSlot {
            key,
            payload,
            kind,
            modified: true,
        });
        Ok(())
    }

    fn flush_dirty_where<F>(&self, mut include: F) -> Result<usize, &'static str>
    where
        F: FnMut(CachedBlockKind) -> bool,
    {
        let mut flushed = 0usize;
        for chain_idx in 0..self.chains.len() {
            let ch = &self.chains[chain_idx];
            let mut items = ch.items.lock().unwrap();
            for slot in items.iter_mut() {
                if slot.modified && include(slot.kind) {
                    slot.modified = false;
                    flushed += 1;
                }
            }
        }
        Ok(flushed)
    }

    // AGENT: writes are already in the block device; flush clears dirty state
    // for slots whose data/metadata class still matches the sync request.
    pub fn flush_dirty(&self) -> Result<usize, &'static str> {
        self.flush_dirty_where(|_| true)
    }

    // AGENT: fdatasync-style sync clears data dirty bits while leaving cached
    // metadata slots dirty for a later full sync.
    pub fn flush_dirty_data(&self) -> Result<usize, &'static str> {
        self.flush_dirty_where(|kind| kind == CachedBlockKind::Data)
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

#[cfg(any(test, feature = "qemu-sync-selftest"))]
pub mod tests {
    use super::*;

    // AGENT: expose BlockCache payload regressions through qemu-sync-selftest.
    pub fn run_all() {
        cache_hit_returns_fixed_payload_without_second_device_read();
        write_updates_cached_fixed_payload();
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
    // the latest full fixed-size payload and dirty class.
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
        assert_eq!(cache.dirty_count_by_kind(CachedBlockKind::Data), 1);

        cache
            .write_block_cached_as(
                &device,
                ROOT_BLOCK_DEVICE,
                5,
                &second,
                CachedBlockKind::Metadata,
            )
            .unwrap();
        assert_eq!(
            cache
                .read_block_cached(&device, ROOT_BLOCK_DEVICE, 5)
                .unwrap()
                .as_slice(),
            &second[..]
        );
        assert_eq!(cache.dirty_count_by_kind(CachedBlockKind::Data), 0);
        assert_eq!(cache.dirty_count_by_kind(CachedBlockKind::Metadata), 1);

        assert_eq!(cache.flush_dirty_data().unwrap(), 0);
        assert_eq!(cache.dirty_count_by_kind(CachedBlockKind::Metadata), 1);
        assert_eq!(cache.flush_dirty().unwrap(), 1);
        assert_eq!(cache.dirty_count(), 0);
    }
}
