// AGENT: own one direct-mapped physical-frame run as a task kernel stack.
use super::{p2v, zero_page, FramePool, KSTK_SZ, PAGE_SZ};

const KSTK_PAGES: usize = KSTK_SZ / PAGE_SZ;

// AGENT: retain the shared frame owner needed to return this stack's contiguous
// physical run without routing task stacks through the general kernel heap.
pub struct KStk {
    paddr: usize,
    pool: FramePool,
}

// AGENT: allocate and zero a page-aligned physical run whose direct-map alias
// can serve as the downward-growing, ABI-aligned RISC-V kernel stack.
impl KStk {
    pub fn new(pool: &FramePool) -> Result<Self, &'static str> {
        assert!(
            KSTK_SZ != 0 && KSTK_SZ % PAGE_SZ == 0,
            "kernel stack size must contain whole pages"
        );
        let paddr = pool.alloc_contiguous_pages(KSTK_PAGES, 1).ok_or("enomem")?;
        for page in 0..KSTK_PAGES {
            zero_page(paddr + page * PAGE_SZ);
        }
        Ok(Self {
            paddr,
            pool: pool.clone(),
        })
    }

    pub fn top(&self) -> usize {
        p2v(self.paddr) + KSTK_SZ
    }
}

// AGENT: return the complete stack run to the same shared physical-frame state.
impl Drop for KStk {
    fn drop(&mut self) {
        assert!(
            self.pool.release_contiguous_pages(self.paddr, KSTK_PAGES),
            "kernel stack frames were released twice or lost FramePool ownership"
        );
    }
}
