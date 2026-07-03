// AGENT
use super::*;

pub const BLOCK_CACHE_BLOCK_SIZE: usize = 512;
pub const ROOT_BLOCK_DEVICE: usize = 0;
pub const DEFAULT_RAM_BLOCK_DEVICE_BYTES: usize = 16 * 1024 * 1024;

// AGENT: narrow block-device interface used by BlockCache; concrete QEMU
// drivers can later implement this over virtio-blk or another real device.
pub trait BlockDevice {
    fn read_block(&self, dev: usize, block: usize) -> Result<Vec<u8>, &'static str>;
    fn write_block(&self, dev: usize, block: usize, data: &[u8]) -> Result<(), &'static str>;
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

impl BlockDevice for RamBlockDevice {
    // AGENT: enforce the fixed RAM-device block range and return zeros for
    // sparse blocks that have never been written.
    fn read_block(&self, _dev: usize, block: usize) -> Result<Vec<u8>, &'static str> {
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
    fn write_block(&self, _dev: usize, block: usize, data: &[u8]) -> Result<(), &'static str> {
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
}

#[cfg(feature = "qemu-sync-selftest")]
pub mod tests {
    use super::*;

    // AGENT: expose RamBlockDevice capacity checks through the existing QEMU
    // sync/fs selftest route instead of relying on host std tests.
    pub fn run_all() {
        empty_device_starts_blank_with_default_capacity();
        empty_device_still_has_writable_blocks();
    }

    // AGENT: verify the fixed RAM-device capacity is exposed without
    // per-instance capacity fields.
    fn empty_device_starts_blank_with_default_capacity() {
        let dev = RamBlockDevice::empty();
        assert_eq!(dev.capacity_bytes(), DEFAULT_RAM_BLOCK_DEVICE_BYTES);
        assert_eq!(dev.capacity_bytes() % BLOCK_CACHE_BLOCK_SIZE, 0);
        let first_block = dev.read_block(0, 0).unwrap();
        assert!(first_block.iter().all(|&byte| byte == 0));
    }

    // AGENT: keep the last fixed-capacity block writable after removing the
    // historical valid-length tracking.
    fn empty_device_still_has_writable_blocks() {
        let dev = RamBlockDevice::empty();
        assert!(dev.block_count() > 1);

        let target_block = dev.block_count() - 1;
        let data = [0x5a; BLOCK_CACHE_BLOCK_SIZE];
        dev.write_block(0, target_block, &data).unwrap();
        assert_eq!(dev.read_block(0, target_block).unwrap().as_slice(), &data);
    }
}
