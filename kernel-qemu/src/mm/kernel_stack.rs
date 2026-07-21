// AGENT: own one direct-mapped PgFrame run as a task kernel stack.
use alloc::vec::Vec;

use super::{p2v, zero_page, FramePool, PgFrame, KSTK_SZ, PAGE_SZ};

const KSTK_PAGES: usize = KSTK_SZ / PAGE_SZ;

// AGENT: retain ordinary PgFrame owners for every page in the contiguous stack
// run so the shared frame RAII path, rather than KStk, performs reclamation.
pub struct KStk {
    frames: Vec<PgFrame>,
}

// AGENT: allocate a contiguous run as ordinary PgFrame handles and zero each
// owned page before exposing its direct-map alias as a RISC-V kernel stack.
impl KStk {
    pub fn new(pool: &FramePool) -> Result<Self, &'static str> {
        assert!(
            KSTK_SZ != 0 && KSTK_SZ % PAGE_SZ == 0,
            "kernel stack size must contain whole pages"
        );
        let frames = pool
            .alloc_contiguous_pg_frames(KSTK_PAGES, 1)
            .ok_or("enomem")?;
        for frame in &frames {
            zero_page(frame.paddr());
        }
        Ok(Self { frames })
    }

    // AGENT: derive the direct-map stack top from the first frame in the
    // contiguous RAII-owned run.
    pub fn top(&self) -> usize {
        p2v(self
            .frames
            .first()
            .expect("kernel stack must own at least one frame")
            .paddr())
            + KSTK_SZ
    }

    // AGENT: expose the final owned PgFrame containing the fixed top-of-stack
    // TrapFrame so the user page table can install its supervisor-only alias.
    pub fn top_page_paddr(&self) -> usize {
        self.frames
            .last()
            .expect("kernel stack must own at least one frame")
            .paddr()
    }
}
