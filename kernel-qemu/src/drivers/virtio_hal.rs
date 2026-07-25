// AGENT: adapt virtio-drivers DMA ownership to the kernel's one shared
// FramePool and active QEMU Sv39 direct map.
use core::ptr::NonNull;

use virtio_drivers::{BufferDirection, Hal, PhysAddr};

use crate::drivers::qemu_virtio_mmio_contains;
use crate::irq_lock::IrqOnceCell;
use crate::kernel::{p2v, v2p, zero_page, FramePool, KERN_BASE, PAGE_SZ};

static DMA_FRAME_POOL: IrqOnceCell<FramePool> = IrqOnceCell::new();

// AGENT: install a clone of the boot FramePool handle so VirtIO's static HAL
// callbacks allocate from the same physical-page ownership bitmap as the heap,
// page tables, user pages, and kernel stacks.
pub fn init(frame_pool: FramePool) -> Result<(), &'static str> {
    DMA_FRAME_POOL.init(frame_pool).map_err(|_| "ebusy")
}

// AGENT: require HAL setup to precede device construction and every virtqueue
// allocation.
fn frame_pool() -> &'static FramePool {
    DMA_FRAME_POOL
        .get()
        .expect("VirtIO HAL used before FramePool initialization")
}

// AGENT: translate dynamic high-half buffers through the direct map while
// retaining identity addresses for the linked kernel, boot stack, and locals.
fn kernel_buffer_paddr(vaddr: usize) -> usize {
    if vaddr >= KERN_BASE {
        v2p(vaddr)
    } else {
        vaddr
    }
}

// AGENT: make virtio-drivers' fixed-width DMA address ABI explicit at the
// kernel's usize-based physical-memory boundary.
fn phys_addr_from_usize(paddr: usize) -> PhysAddr {
    PhysAddr::try_from(paddr).expect("kernel physical address does not fit VirtIO ABI")
}

// AGENT: reject device addresses that cannot be represented by this RV64
// kernel before using them in FramePool or page-table helpers.
fn phys_addr_to_usize(paddr: PhysAddr) -> usize {
    usize::try_from(paddr).expect("VirtIO physical address does not fit kernel usize")
}

// AGENT: reject shared buffers outside FramePool-owned QEMU RAM before the
// device receives their physical address.
fn validate_shared_range(paddr: usize, len: usize) {
    assert!(len > 0, "VirtIO cannot share an empty buffer");
    let last = paddr
        .checked_add(len - 1)
        .expect("VirtIO shared buffer range overflow");
    let first_page = paddr & !(PAGE_SZ - 1);
    let last_page = last & !(PAGE_SZ - 1);
    assert!(
        frame_pool().paddr_to_frame_id(first_page).is_some()
            && frame_pool().paddr_to_frame_id(last_page).is_some(),
        "VirtIO shared buffer is outside FramePool RAM"
    );
}

// AGENT: marker type used for static dispatch by virtio-drivers.
pub struct VirtioHal;

// SAFETY: DMA allocations are exclusive contiguous FramePool runs, returned
// through the active direct map, and share/unshare only expose QEMU RAM physical
// addresses for the duration controlled by virtio-drivers.
// AGENT: satisfy virtio-drivers' static HAL contract without creating a second
// allocator or address-space ownership source.
unsafe impl Hal for VirtioHal {
    // AGENT: allocate and zero a page-aligned physically contiguous DMA run
    // from the kernel's sole physical-page allocator.
    fn dma_alloc(pages: usize, _direction: BufferDirection) -> (PhysAddr, NonNull<u8>) {
        let paddr = frame_pool()
            .alloc_contiguous_pages(pages, 1)
            .expect("VirtIO DMA allocation exhausted FramePool");
        for page in 0..pages {
            zero_page(paddr + page * PAGE_SZ);
        }
        let vaddr = NonNull::new(p2v(paddr) as *mut u8)
            .expect("direct-mapped VirtIO DMA address should be non-null");
        (phys_addr_from_usize(paddr), vaddr)
    }

    // AGENT: return exactly the DMA run originally allocated by dma_alloc to
    // the same shared FramePool.
    unsafe fn dma_dealloc(paddr: PhysAddr, vaddr: NonNull<u8>, pages: usize) -> i32 {
        let paddr = phys_addr_to_usize(paddr);
        if vaddr.as_ptr() as usize != p2v(paddr) {
            return -1;
        }
        if frame_pool().release_contiguous_pages(paddr, pages) {
            0
        } else {
            -1
        }
    }

    // AGENT: this first platform adapter identity-maps the fixed QEMU virt MMIO
    // window into the kernel page table; the callback is primarily for PCI BARs.
    unsafe fn mmio_phys_to_virt(paddr: PhysAddr, size: usize) -> NonNull<u8> {
        let paddr = phys_addr_to_usize(paddr);
        assert!(
            qemu_virtio_mmio_contains(paddr, size),
            "VirtIO MMIO address is outside mapped QEMU window"
        );
        NonNull::new(paddr as *mut u8).expect("QEMU VirtIO MMIO address should be non-null")
    }

    // AGENT: expose the physical address underlying one non-empty kernel buffer
    // to the identity-addressed QEMU VirtIO device.
    unsafe fn share(buffer: NonNull<[u8]>, _direction: BufferDirection) -> PhysAddr {
        let paddr = kernel_buffer_paddr(buffer.as_ptr() as *mut u8 as usize);
        validate_shared_range(paddr, buffer.len());
        phys_addr_from_usize(paddr)
    }

    // AGENT: QEMU virt has coherent guest RAM and no IOMMU in this milestone,
    // so unshare only verifies that it matches the original direct translation.
    unsafe fn unshare(paddr: PhysAddr, buffer: NonNull<[u8]>, _direction: BufferDirection) {
        let expected = kernel_buffer_paddr(buffer.as_ptr() as *mut u8 as usize);
        let paddr = phys_addr_to_usize(paddr);
        assert_eq!(paddr, expected, "VirtIO unshare address mismatch");
        validate_shared_range(paddr, buffer.len());
    }
}
