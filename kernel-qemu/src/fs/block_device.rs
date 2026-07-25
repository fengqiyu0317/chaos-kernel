// AGENT
use super::*;

pub const BLOCK_CACHE_BLOCK_SIZE: usize = 512;
pub const ROOT_BLOCK_DEVICE: usize = 0;
pub const DEFAULT_RAM_BLOCK_DEVICE_BYTES: usize = 16 * 1024 * 1024;

// AGENT: keep cache namespacing outside concrete devices while exposing the
// capacity and stable-write operation required by persistent QEMU backends.
pub trait BlockDevice: Send + Sync {
    // AGENT: report the number of addressable fixed-size cache blocks.
    fn block_count(&self) -> usize;
    // AGENT: read one complete cache block from this concrete device.
    fn read_block(&self, block: usize) -> Result<Vec<u8>, &'static str>;
    // AGENT: write one complete cache block to this concrete device.
    fn write_block(&self, block: usize, data: &[u8]) -> Result<(), &'static str>;
    // AGENT: make every previously completed write stable when supported.
    fn flush(&self) -> Result<(), &'static str>;
}

// AGENT: sparse fixed-size RAM block device used as the first QEMU-side
// writable backing store before virtio-blk is introduced.
pub struct RamBlockDevice {
    blocks: Mutex<BTreeMap<usize, Vec<u8>>>,
}

impl RamBlockDevice {
    // AGENT: keep the default kernel backend explicit while virtio-blk or a
    // real filesystem image is not part of the QEMU carrier.
    pub fn empty() -> Self {
        Self {
            blocks: Mutex::new(BTreeMap::new()),
        }
    }

    // AGENT: RamBlockDevice is a fixed-size temporary backend; the capacity is
    // a compile-time policy rather than per-instance state.
    pub fn capacity_bytes(&self) -> usize {
        DEFAULT_RAM_BLOCK_DEVICE_BYTES
    }

    // AGENT: derive the block count from the fixed RAM-device byte capacity.
    pub fn block_count(&self) -> usize {
        self.capacity_bytes() / BLOCK_CACHE_BLOCK_SIZE
    }
}

// AGENT: implement the persistent-backend contract with immediate in-memory
// visibility and a no-op stable-write operation.
impl BlockDevice for RamBlockDevice {
    // AGENT: publish the fixed RAM-disk capacity through the common backend
    // interface used by FileBlockAllocator.
    fn block_count(&self) -> usize {
        RamBlockDevice::block_count(self)
    }

    // AGENT: enforce the fixed RAM-device block range and return zeros for
    // sparse blocks that have never been written.
    fn read_block(&self, block: usize) -> Result<Vec<u8>, &'static str> {
        if block >= self.block_count() {
            return Err("eio");
        }
        let blocks = self.blocks.lock().unwrap();
        if let Some(payload) = blocks.get(&block) {
            Ok(payload.clone())
        } else {
            Ok(vec![0; BLOCK_CACHE_BLOCK_SIZE])
        }
    }

    // AGENT: store only in-range non-zero blocks in the sparse RAM map;
    // all-zero writes remove the block.
    fn write_block(&self, block: usize, data: &[u8]) -> Result<(), &'static str> {
        if data.len() != BLOCK_CACHE_BLOCK_SIZE {
            return Err("einval");
        }
        if block >= self.block_count() {
            return Err("eio");
        }
        let mut blocks = self.blocks.lock().unwrap();
        if data.iter().any(|&byte| byte != 0) {
            blocks.insert(block, data.to_vec());
        } else {
            blocks.remove(&block);
        }
        Ok(())
    }

    // AGENT: RAM writes are immediately visible to later reads, so the
    // in-memory fallback has no second persistence layer to flush.
    fn flush(&self) -> Result<(), &'static str> {
        Ok(())
    }
}

#[cfg(feature = "qemu-sync-selftest")]
pub mod tests {
    use super::*;

    // AGENT: expose RamBlockDevice capacity checks through the existing QEMU
    // sync/fs selftest route instead of relying on host std tests.
    pub fn run_all() {
        empty_device_starts_blank_with_default_capacity();
        empty_device_still_has_writable_blocks();
        file_storage_flushes_device_after_cache_writeback();
    }

    // AGENT: verify the fixed RAM-device capacity is exposed without
    // per-instance capacity fields.
    fn empty_device_starts_blank_with_default_capacity() {
        let dev = RamBlockDevice::empty();
        assert_eq!(dev.capacity_bytes(), DEFAULT_RAM_BLOCK_DEVICE_BYTES);
        assert_eq!(dev.capacity_bytes() % BLOCK_CACHE_BLOCK_SIZE, 0);
        let first_block = dev.read_block(0).unwrap();
        assert!(first_block.iter().all(|&byte| byte == 0));
    }

    // AGENT: keep the last fixed-capacity block writable after removing the
    // historical valid-length tracking.
    fn empty_device_still_has_writable_blocks() {
        let dev = RamBlockDevice::empty();
        assert!(dev.block_count() > 1);

        let target_block = dev.block_count() - 1;
        let data = [0x5a; BLOCK_CACHE_BLOCK_SIZE];
        dev.write_block(target_block, &data).unwrap();
        dev.flush().unwrap();
        assert_eq!(dev.read_block(target_block).unwrap().as_slice(), &data);
    }

    // AGENT: record device-write progress at flush time so the regression
    // proves FileStorage drains BlockCache before issuing the stable-write
    // operation required by a persistent backend.
    struct FlushTrackingDevice {
        backing: RamBlockDevice,
        writes: AtomicUsize,
        writes_seen_at_flush: AtomicUsize,
    }

    impl FlushTrackingDevice {
        // AGENT: initialize an empty fixed-capacity backend and zero counters.
        fn new() -> Self {
            Self {
                backing: RamBlockDevice::empty(),
                writes: AtomicUsize::new(0),
                writes_seen_at_flush: AtomicUsize::new(0),
            }
        }
    }

    // AGENT: wrap the RAM device to observe cache-writeback and flush ordering.
    impl BlockDevice for FlushTrackingDevice {
        // AGENT: keep the wrapper capacity identical to its RAM backing store.
        fn block_count(&self) -> usize {
            self.backing.block_count()
        }

        // AGENT: delegate reads without changing the flush-order counters.
        fn read_block(&self, block: usize) -> Result<Vec<u8>, &'static str> {
            self.backing.read_block(block)
        }

        // AGENT: count only writes that have completed in the backing device.
        fn write_block(&self, block: usize, data: &[u8]) -> Result<(), &'static str> {
            self.backing.write_block(block, data)?;
            self.writes.fetch_add(1, Ordering::Relaxed);
            Ok(())
        }

        // AGENT: snapshot how many completed writes were visible when the
        // stable-write operation was issued.
        fn flush(&self) -> Result<(), &'static str> {
            self.writes_seen_at_flush
                .store(self.writes.load(Ordering::Relaxed), Ordering::Relaxed);
            Ok(())
        }
    }

    // AGENT: route an initial FileNode payload through the real FileStorage
    // flush path and require device flush to observe completed cache writeback.
    fn file_storage_flushes_device_after_cache_writeback() {
        let device = Arc::new(FlushTrackingDevice::new());
        let storage = FileStorage::new(
            Arc::new(BlockCache::new(1)),
            device.clone(),
            Arc::new(FileBlockAllocator::new(device.block_count())),
        );
        let node = FileNode::regular(false);

        node.write_initial_bytes(&storage, b"stable").unwrap();

        let writes = device.writes.load(Ordering::Relaxed);
        assert!(writes > 0);
        assert_eq!(device.writes_seen_at_flush.load(Ordering::Relaxed), writes);
    }
}
