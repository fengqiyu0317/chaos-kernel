// AGENT: isolate task kernel-stack allocation from physical-frame allocation.
use core::alloc::Layout;

use super::KSTK_SZ;

const KSTK_ALIGN: usize = 16;

// AGENT: own one aligned kernel stack allocation for a schedulable task.
pub struct KStk(usize);

// AGENT: allocate an explicitly aligned zeroed stack so RISC-V trap return
// code can use KStk::top() as an ABI-aligned stack pointer.
impl KStk {
    pub fn new() -> Self {
        let layout = kstk_layout();
        let ptr = unsafe { ::alloc::alloc::alloc_zeroed(layout) };
        if ptr.is_null() {
            ::alloc::alloc::handle_alloc_error(layout);
        }
        KStk(ptr as usize)
    }

    pub fn top(&self) -> usize {
        self.0 + KSTK_SZ
    }
}

// AGENT: centralize the stack layout so allocation and deallocation stay paired.
fn kstk_layout() -> Layout {
    Layout::from_size_align(KSTK_SZ, KSTK_ALIGN).expect("kernel stack layout should be valid")
}

// AGENT: release the stack through the same layout used for allocation.
impl Drop for KStk {
    fn drop(&mut self) {
        unsafe {
            ::alloc::alloc::dealloc(self.0 as *mut u8, kstk_layout());
        }
    }
}
