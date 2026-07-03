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

    // AGENT: locate cache slots by block identity without returning an index
    // that callers must immediately unwrap back into a slot.
    fn slot_ref(
        items: &[Option<CacheSlot>; BLOCK_CACHE_CHAIN_SLOTS],
        key: BlockKey,
    ) -> Option<&CacheSlot> {
        items
            .iter()
            .filter_map(|entry| entry.as_ref())
            .find(|slot| slot.key == key)
    }

    // AGENT: mutate a resident slot directly after finding it by block identity.
    fn slot_mut(
        items: &mut [Option<CacheSlot>; BLOCK_CACHE_CHAIN_SLOTS],
        key: BlockKey,
    ) -> Option<&mut CacheSlot> {
        items
            .iter_mut()
            .filter_map(|entry| entry.as_mut())
            .find(|slot| slot.key == key)
    }

    // AGENT: prefer unused fixed slots before replacing an existing cache slot.
    fn empty_slot_index(items: &[Option<CacheSlot>; BLOCK_CACHE_CHAIN_SLOTS]) -> Option<usize> {
        items.iter().position(|entry| entry.is_none())
    }

    // AGENT: choose a replacement victim deterministically, preferring clean
    // slots so writeback work is only paid when all slots are dirty.
    fn victim_slot(
        items: &[Option<CacheSlot>; BLOCK_CACHE_CHAIN_SLOTS],
    ) -> Option<(usize, CacheSlot)> {
        items
            .iter()
            .enumerate()
            .find_map(|(idx, entry)| match entry {
                Some(slot) if !slot.modified => Some((idx, *slot)),
                _ => None,
            })
            .or_else(|| {
                items
                    .iter()
                    .enumerate()
                    .find_map(|(idx, entry)| (*entry).map(|slot| (idx, slot)))
            })
    }

    // AGENT: write back a dirty cache snapshot before it is marked clean or
    // reused for another block key.
    fn flush_slot<D: BlockDevice + ?Sized>(
        device: &D,
        slot: CacheSlot,
    ) -> Result<(), &'static str> {
        if slot.modified {
            device.write_block(slot.key.dev, slot.key.block, &slot.payload)?;
        }
        Ok(())
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
            if let Some(slot) = Self::slot_ref(&items, key) {
                return Ok(slot.payload.to_vec());
            }
        }

        let block_data = device.read_block(dev, block)?;
        let payload = block_payload_from_slice(&block_data, "eio")?;
        {
            let mut items = ch.items.lock().unwrap();
            if let Some(slot) = Self::slot_ref(&items, key) {
                return Ok(slot.payload.to_vec());
            }
            let new_slot = CacheSlot {
                key,
                payload,
                modified: false,
            };
            if let Some(empty_idx) = Self::empty_slot_index(&items) {
                items[empty_idx] = Some(new_slot);
                return Ok(block_data);
            }
            let Some((victim_idx, victim)) = Self::victim_slot(&items) else {
                return Err("enospc");
            };
            Self::flush_slot(device, victim)?;
            items[victim_idx] = Some(new_slot);
        }
        Ok(block_data)
    }

    // AGENT: write back through the cache by updating the resident slot first;
    // a full chain flushes one victim before reusing its slot.
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
        let mut items = ch.items.lock().unwrap();
        if let Some(slot) = Self::slot_mut(&mut items, key) {
            slot.payload = payload;
            slot.modified = true;
            return Ok(());
        }
        let new_slot = CacheSlot {
            key,
            payload,
            modified: true,
        };
        if let Some(empty_idx) = Self::empty_slot_index(&items) {
            items[empty_idx] = Some(new_slot);
            return Ok(());
        }
        let Some((victim_idx, victim)) = Self::victim_slot(&items) else {
            return Err("enospc");
        };
        Self::flush_slot(device, victim)?;
        items[victim_idx] = Some(new_slot);
        Ok(())
    }

    // AGENT: perform one snapshot writeback pass without treating the number
    // of cleared slots as proof that the cache is now clean.
    fn flush_dirty_once<D: BlockDevice + ?Sized>(&self, device: &D) -> Result<usize, &'static str> {
        let mut dirty = Vec::new();
        for chain_idx in 0..self.chains.len() {
            let ch = &self.chains[chain_idx];
            let items = ch.items.lock().unwrap();
            for slot in items.iter().filter_map(|entry| entry.as_ref()) {
                if slot.modified {
                    dirty.push(*slot);
                }
            }
        }

        let mut flushed = 0usize;
        for snapshot in dirty {
            Self::flush_slot(device, snapshot)?;
            let ch = &self.chains[self.idx(snapshot.key)];
            let mut items = ch.items.lock().unwrap();
            if let Some(slot) = Self::slot_mut(&mut items, snapshot.key) {
                if slot.modified && slot.payload == snapshot.payload {
                    slot.modified = false;
                    flushed += 1;
                }
            }
        }
        Ok(flushed)
    }

    // AGENT: keep writing dirty snapshots until the live cache reports no
    // dirty slots, since a writeback pass may race with a newer cached payload.
    pub fn flush_dirty<D: BlockDevice + ?Sized>(&self, device: &D) -> Result<usize, &'static str> {
        let mut flushed = 0usize;
        loop {
            flushed += self.flush_dirty_once(device)?;
            if self.dirty_count() == 0 {
                return Ok(flushed);
            }
        }
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
}

#[cfg(any(test, feature = "qemu-sync-selftest"))]
pub mod tests {
    use super::*;

    // AGENT: expose BlockCache payload regressions through qemu-sync-selftest.
    pub fn run_all() {
        cache_hit_returns_fixed_payload_without_second_device_read();
        write_updates_cached_payload_before_flush_writes_back();
        flush_dirty_drains_until_dirty_count_is_zero();
        full_write_chain_flushes_victim_for_replacement();
        full_read_chain_flushes_victim_for_replacement();
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

    // AGENT: device wrapper that dirties the same cached block during the first
    // writeback pass so flush_dirty() must continue based on dirty_count().
    struct RewriteDuringFlushDevice<'a> {
        cache: &'a BlockCache,
        backing: RamBlockDevice,
        writes: AtomicUsize,
        rewrite_payload: BlockPayload,
    }

    impl<'a> RewriteDuringFlushDevice<'a> {
        // AGENT: keep the cache reference and replacement payload explicit for
        // the dirty-count drain regression.
        fn new(cache: &'a BlockCache, rewrite_payload: BlockPayload) -> Self {
            Self {
                cache,
                backing: RamBlockDevice::empty(),
                writes: AtomicUsize::new(0),
                rewrite_payload,
            }
        }
    }

    impl BlockDevice for RewriteDuringFlushDevice<'_> {
        // AGENT: delegate reads to the real RAM backend so the final persisted
        // payload can be checked after flush_dirty() returns.
        fn read_block(&self, dev: usize, block: usize) -> Result<Vec<u8>, &'static str> {
            self.backing.read_block(dev, block)
        }

        // AGENT: mutate the cached slot once during writeback to make the first
        // pass clear zero slots while dirty_count() still reports pending work.
        fn write_block(&self, dev: usize, block: usize, data: &[u8]) -> Result<(), &'static str> {
            self.backing.write_block(dev, block, data)?;
            if self.writes.fetch_add(1, Ordering::Relaxed) == 0 {
                self.cache
                    .write_block_cached(self, dev, block, &self.rewrite_payload)?;
            }
            Ok(())
        }
    }

    // AGENT: verify read hits return the slot-owned fixed payload without a
    // second device read that could replace cached contents.
    #[cfg_attr(test, test)]
    fn cache_hit_returns_fixed_payload_without_second_device_read() {
        let cache = BlockCache::new(1);
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

    // AGENT: keep writes resident in the cache until flush_dirty() writes the
    // latest full fixed-size payload to the backing device.
    #[cfg_attr(test, test)]
    fn write_updates_cached_payload_before_flush_writes_back() {
        let cache = BlockCache::new(1);
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
        assert_eq!(
            device.read_block(ROOT_BLOCK_DEVICE, 5).unwrap().as_slice(),
            &[0u8; BLOCK_CACHE_BLOCK_SIZE][..]
        );

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
        assert_eq!(
            device.read_block(ROOT_BLOCK_DEVICE, 5).unwrap().as_slice(),
            &[0u8; BLOCK_CACHE_BLOCK_SIZE][..]
        );
        assert_eq!(cache.flush_dirty(&device).unwrap(), 1);
        assert_eq!(cache.dirty_count(), 0);
        assert_eq!(
            device.read_block(ROOT_BLOCK_DEVICE, 5).unwrap().as_slice(),
            &second[..]
        );
    }

    // AGENT: prove flush_dirty() drains until dirty_count() is zero, not until a
    // single writeback pass reports that it cleared zero slots.
    #[cfg_attr(test, test)]
    fn flush_dirty_drains_until_dirty_count_is_zero() {
        let cache = BlockCache::new(1);
        let rewrite = [0x44u8; BLOCK_CACHE_BLOCK_SIZE];
        let device = RewriteDuringFlushDevice::new(&cache, rewrite);
        let first = [0x33u8; BLOCK_CACHE_BLOCK_SIZE];

        cache
            .write_block_cached(&device, ROOT_BLOCK_DEVICE, 8, &first)
            .unwrap();
        assert_eq!(cache.dirty_count(), 1);

        assert_eq!(cache.flush_dirty(&device).unwrap(), 1);
        assert_eq!(cache.dirty_count(), 0);
        assert_eq!(device.writes.load(Ordering::Relaxed), 2);
        assert_eq!(
            device.read_block(ROOT_BLOCK_DEVICE, 8).unwrap().as_slice(),
            &rewrite[..]
        );
    }

    // AGENT: keep the fixed-size chain bound while replacing a dirty victim
    // through writeback instead of rejecting the new write.
    #[cfg_attr(test, test)]
    fn full_write_chain_flushes_victim_for_replacement() {
        let cache = BlockCache::new(1);
        let device = RamBlockDevice::empty();

        for block in 0..BLOCK_CACHE_CHAIN_SLOTS {
            let payload = [block.wrapping_add(1) as u8; BLOCK_CACHE_BLOCK_SIZE];
            cache
                .write_block_cached(&device, ROOT_BLOCK_DEVICE, block, &payload)
                .unwrap();
        }

        assert_eq!(cache.dirty_count(), BLOCK_CACHE_CHAIN_SLOTS);

        let replacement = [0x77u8; BLOCK_CACHE_BLOCK_SIZE];
        cache
            .write_block_cached(
                &device,
                ROOT_BLOCK_DEVICE,
                BLOCK_CACHE_CHAIN_SLOTS,
                &replacement,
            )
            .unwrap();

        assert_eq!(cache.dirty_count(), BLOCK_CACHE_CHAIN_SLOTS);
        assert_eq!(
            device.read_block(ROOT_BLOCK_DEVICE, 0).unwrap().as_slice(),
            &[1u8; BLOCK_CACHE_BLOCK_SIZE][..]
        );
        assert_eq!(
            cache
                .read_block_cached(&device, ROOT_BLOCK_DEVICE, BLOCK_CACHE_CHAIN_SLOTS)
                .unwrap()
                .as_slice(),
            &replacement[..]
        );
        assert_eq!(cache.flush_dirty(&device).unwrap(), BLOCK_CACHE_CHAIN_SLOTS);
        assert_eq!(cache.dirty_count(), 0);
        assert_eq!(
            device
                .read_block(ROOT_BLOCK_DEVICE, BLOCK_CACHE_CHAIN_SLOTS)
                .unwrap()
                .as_slice(),
            &replacement[..]
        );
    }

    // AGENT: a full read miss also writes back one dirty victim so the newly
    // read block can occupy a fixed cache slot.
    #[cfg_attr(test, test)]
    fn full_read_chain_flushes_victim_for_replacement() {
        let cache = BlockCache::new(1);
        let device = RamBlockDevice::empty();

        for block in 0..BLOCK_CACHE_CHAIN_SLOTS {
            let payload = [block.wrapping_add(1) as u8; BLOCK_CACHE_BLOCK_SIZE];
            cache
                .write_block_cached(&device, ROOT_BLOCK_DEVICE, block, &payload)
                .unwrap();
        }
        let incoming = [0xeeu8; BLOCK_CACHE_BLOCK_SIZE];
        device
            .write_block(ROOT_BLOCK_DEVICE, BLOCK_CACHE_CHAIN_SLOTS, &incoming)
            .unwrap();

        assert_eq!(
            cache
                .read_block_cached(&device, ROOT_BLOCK_DEVICE, BLOCK_CACHE_CHAIN_SLOTS)
                .unwrap()
                .as_slice(),
            &incoming[..]
        );

        assert_eq!(cache.dirty_count(), BLOCK_CACHE_CHAIN_SLOTS - 1);
        assert_eq!(
            device.read_block(ROOT_BLOCK_DEVICE, 0).unwrap().as_slice(),
            &[1u8; BLOCK_CACHE_BLOCK_SIZE][..]
        );
    }
}
