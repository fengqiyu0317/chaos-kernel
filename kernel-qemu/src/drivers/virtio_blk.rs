// AGENT: polling VirtIO block backend for the first real-sector QEMU storage
// milestone; interrupt-driven completion is intentionally deferred.
use alloc::sync::Arc;
use alloc::vec;
use alloc::vec::Vec;
use core::ptr::NonNull;

use virtio_drivers::device::blk::{VirtIOBlk, SECTOR_SIZE};
use virtio_drivers::transport::mmio::{MmioTransport, VirtIOHeader};
use virtio_drivers::transport::{DeviceType, Transport};

use super::virtio_hal::{self, VirtioHal};
use super::{QEMU_VIRTIO_MMIO_SLOTS, QEMU_VIRTIO_MMIO_SLOT_SIZE, QEMU_VIRTIO_MMIO_START};
use crate::irq_lock::Mutex;
use crate::kernel::{BlockDevice, FramePool, BLOCK_CACHE_BLOCK_SIZE};
use crate::println;

type PollingVirtioBlk = VirtIOBlk<VirtioHal, MmioTransport<'static>>;

// AGENT: serialize polling queue use while exposing a dyn BlockDevice backend
// to FileStorage and the raw-sector smoke path.
pub struct VirtioBlockDevice {
    inner: Mutex<PollingVirtioBlk>,
    block_count: usize,
}

impl VirtioBlockDevice {
    // AGENT: finish feature negotiation once and snapshot the 512-byte sector
    // capacity used for every later bounds check.
    fn new(transport: MmioTransport<'static>) -> Result<Self, &'static str> {
        let inner = VirtIOBlk::<VirtioHal, _>::new(transport).map_err(|_| "eio")?;
        let block_count = usize::try_from(inner.capacity()).map_err(|_| "efbig")?;
        if block_count == 0 || SECTOR_SIZE != BLOCK_CACHE_BLOCK_SIZE {
            return Err("eio");
        }
        Ok(Self {
            inner: Mutex::new(inner),
            block_count,
        })
    }
}

// AGENT: expose the polling VirtIO driver through the storage semantic layer's
// device-independent block interface.
impl BlockDevice for VirtioBlockDevice {
    // AGENT: expose the 512-byte sector capacity read during device setup.
    fn block_count(&self) -> usize {
        self.block_count
    }

    // AGENT: perform one synchronous virtqueue transaction for one 512-byte
    // cache block and report device failures as the storage-layer eio.
    fn read_block(&self, block: usize) -> Result<Vec<u8>, &'static str> {
        if block >= self.block_count {
            return Err("eio");
        }
        let mut data = vec![0; BLOCK_CACHE_BLOCK_SIZE];
        self.inner
            .lock()
            .read_blocks(block, &mut data)
            .map_err(|_| "eio")?;
        Ok(data)
    }

    // AGENT: require exactly one cache block and submit it synchronously to the
    // VirtIO block request queue.
    fn write_block(&self, block: usize, data: &[u8]) -> Result<(), &'static str> {
        if data.len() != BLOCK_CACHE_BLOCK_SIZE {
            return Err("einval");
        }
        if block >= self.block_count {
            return Err("eio");
        }
        self.inner
            .lock()
            .write_blocks(block, data)
            .map_err(|_| "eio")
    }

    // AGENT: issue VIRTIO_BLK_T_FLUSH after completed writes so QEMU commits
    // them to the persistent raw-image backend before reporting success.
    fn flush(&self) -> Result<(), &'static str> {
        self.inner.lock().flush().map_err(|_| "eio")
    }
}

// AGENT: scan all eight fixed QEMU virt MMIO slots, select device ID 2 through
// MmioTransport's typed DeviceType, and never silently fall back to RAM.
pub fn probe_root_block(frame_pool: FramePool) -> Result<Arc<dyn BlockDevice>, &'static str> {
    virtio_hal::init(frame_pool)?;
    for slot in 0..QEMU_VIRTIO_MMIO_SLOTS {
        let mmio_paddr = QEMU_VIRTIO_MMIO_START + slot * QEMU_VIRTIO_MMIO_SLOT_SIZE;
        let header = NonNull::new(mmio_paddr as *mut VirtIOHeader).ok_or("enodev")?;
        // SAFETY: the fixed QEMU virt window is page-mapped supervisor RW for
        // the whole boot lifetime, and each slot is one 4 KiB MMIO region.
        let Ok(transport) = (unsafe { MmioTransport::new(header, QEMU_VIRTIO_MMIO_SLOT_SIZE) })
        else {
            continue;
        };
        if transport.device_type() != DeviceType::Block {
            continue;
        }
        let device = VirtioBlockDevice::new(transport)?;
        println!(
            "[kernel-qemu] virtio-blk ready mmio={:#x} blocks={} discovery=fixed-qemu-virt",
            mmio_paddr,
            device.block_count()
        );
        return Ok(Arc::new(device));
    }
    Err("enodev")
}
