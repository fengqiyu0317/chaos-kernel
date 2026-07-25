// AGENT: keep QEMU-only device discovery and hardware adapters outside the
// migrated kernel semantic tree.
pub mod virtio_blk;
pub mod virtio_hal;

// AGENT: first polling milestone scans every virtio-mmio slot in QEMU virt's
// fixed platform window. Replace this with DTB-derived regions before adding
// another board; never assume that the block device occupies the first slot.
pub const QEMU_VIRTIO_MMIO_START: usize = 0x1000_1000;
pub const QEMU_VIRTIO_MMIO_SLOT_SIZE: usize = 0x1000;
pub const QEMU_VIRTIO_MMIO_SLOTS: usize = 8;

pub const QEMU_VIRTIO_MMIO_SIZE: usize = QEMU_VIRTIO_MMIO_SLOT_SIZE * QEMU_VIRTIO_MMIO_SLOTS;

// AGENT: validate the physical MMIO range accepted by the first QEMU-specific
// VirtIO HAL implementation without allowing arithmetic wraparound.
pub fn qemu_virtio_mmio_contains(paddr: usize, size: usize) -> bool {
    if size == 0 {
        return false;
    }
    let Some(range_end) = QEMU_VIRTIO_MMIO_START.checked_add(QEMU_VIRTIO_MMIO_SIZE) else {
        return false;
    };
    let Some(end) = paddr.checked_add(size) else {
        return false;
    };
    paddr >= QEMU_VIRTIO_MMIO_START && end <= range_end
}
