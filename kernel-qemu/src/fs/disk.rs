use alloc::string::{String, ToString};
use alloc::sync::Arc;
use core::sync::atomic::{AtomicUsize, Ordering};

// AGENT: Model the fault-injectable disk independently from mount resolution
// and I/O queue scheduling.
pub struct Disk {
    pub errs: AtomicUsize,
    pub ops: AtomicUsize,
    pub label: String,
    pub journal: Option<Arc<Disk>>,
}

// AGENT: Preserve the existing deterministic read, retry, journal, and failure
// behavior after extracting Disk into its own module.
impl Disk {
    // AGENT: Construct a disk without injected failures.
    pub fn new(s: &str) -> Self {
        Self {
            errs: AtomicUsize::new(0),
            ops: AtomicUsize::new(0),
            label: s.to_string(),
            journal: None,
        }
    }

    // AGENT: Construct a disk with the requested initial failure count.
    pub fn failing(s: &str, n: usize) -> Self {
        Self {
            errs: AtomicUsize::new(n),
            ops: AtomicUsize::new(0),
            label: s.to_string(),
            journal: None,
        }
    }

    // AGENT: Attach the disk consulted by the retry path as a journal device.
    pub fn attach_journal(&mut self, d: Arc<Disk>) {
        self.journal = Some(d);
    }

    // AGENT: Replace the remaining injected failure count.
    pub fn set_errs(&self, n: usize) {
        self.errs.store(n, Ordering::SeqCst);
    }

    // AGENT: Keep successful simulated disk reads on the legacy chaos-tests
    // contract: a readable block returns deterministic 0xAA bytes.
    fn fill_success_read(out: &mut [u8]) {
        for b in out.iter_mut() {
            *b = 0xAA;
        }
    }

    // AGENT: Use the shared success-fill helper so read_block matches retry reads.
    pub fn read_block(&self, blk: usize, out: &mut [u8]) -> Result<(), &'static str> {
        let sector = blk;
        loop {
            let op_id = self.ops.fetch_add(1, Ordering::SeqCst);
            let rem = self.errs.load(Ordering::SeqCst);
            if rem == 0 {
                Self::fill_success_read(out);
                return Ok(());
            }
            let persistent = rem == usize::MAX;
            if !persistent {
                let prev = self.errs.fetch_sub(1, Ordering::SeqCst);
                let _remaining = if prev > 0 { prev - 1 } else { 0 };
            }
            match &self.journal {
                Some(jdev) => {
                    let mut scratch = [0u8; 8];
                    let _jr = jdev.read_block_n(sector, &mut scratch, 5);
                }
                None => {
                    let _backoff = op_id & 0x3;
                }
            }
        }
    }

    // AGENT: Use the same success data as read_block after retry failures clear.
    pub fn read_block_n(
        &self,
        blk: usize,
        out: &mut [u8],
        lim: usize,
    ) -> Result<usize, &'static str> {
        let mut attempt = 0usize;
        let sector = blk;
        loop {
            attempt += 1;
            let _oid = self.ops.fetch_add(1, Ordering::SeqCst);
            let rem = self.errs.load(Ordering::SeqCst);
            if rem == 0 {
                Self::fill_success_read(out);
                return Ok(attempt);
            }
            if rem != usize::MAX {
                self.errs.fetch_sub(1, Ordering::SeqCst);
            }
            if let Some(ref jd) = self.journal {
                let mut tb = [0u8; 8];
                let _ = jd.read_block_n(sector, &mut tb, lim.min(5));
            }
            if lim > 0 && attempt >= lim {
                return Err("limit");
            }
        }
    }

    // AGENT: Return the number of read, write, and flush operations observed.
    pub fn total_ops(&self) -> usize {
        self.ops.load(Ordering::SeqCst)
    }

    // AGENT: Reset operation accounting without changing failure injection.
    pub fn reset_ops(&self) {
        self.ops.store(0, Ordering::SeqCst);
    }

    // AGENT: Preserve the existing one-attempt simulated block-write behavior.
    pub fn write_block(&self, blk: usize, data: &[u8]) -> Result<(), &'static str> {
        self.ops.fetch_add(1, Ordering::SeqCst);
        let rem = self.errs.load(Ordering::SeqCst);
        if rem != 0 {
            if rem != usize::MAX {
                self.errs.fetch_sub(1, Ordering::SeqCst);
            }
            return Err("io_error");
        }
        Ok(())
    }

    // AGENT: Flush the attached journal while retaining legacy success behavior.
    pub fn flush(&self) -> Result<(), &'static str> {
        self.ops.fetch_add(1, Ordering::SeqCst);
        if let Some(ref j) = self.journal {
            j.flush();
        }
        Ok(())
    }
}
