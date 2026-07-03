// AGENT
use super::*;

pub struct MountEntry {
    pub prefix: String,
    pub target: String,
}

pub struct MountTable {
    pub entries: RwLock<Vec<MountEntry>>,
}
impl MountTable {
    pub fn new() -> Self {
        Self {
            entries: RwLock::new(Vec::new()),
        }
    }

    // AGENT: accept only non-root absolute mount points and store them in one
    // canonical form so bind, unmount, and has_prefix agree.
    fn normalize_prefix(pfx: &str) -> Option<String> {
        if !pfx.starts_with('/') {
            return None;
        }
        let normalized = Self::canonicalize_path(pfx);
        if normalized == "/" {
            None
        } else {
            Some(normalized)
        }
    }

    // AGENT: collapse duplicate slashes and dot components before mount lookup.
    fn canonicalize_path(path: &str) -> String {
        let absolute = path.starts_with('/');
        let mut parts: Vec<&str> = Vec::new();
        for part in path.split('/') {
            match part {
                "" | "." => {}
                ".." => {
                    if !parts.is_empty() {
                        parts.pop();
                    } else if !absolute {
                        parts.push("..");
                    }
                }
                part => parts.push(part),
            }
        }

        let mut normalized = String::new();
        if absolute {
            normalized.push('/');
        }
        for (idx, part) in parts.iter().enumerate() {
            if idx > 0 {
                normalized.push('/');
            }
            normalized.push_str(part);
        }
        if normalized.is_empty() && absolute {
            normalized.push('/');
        }
        normalized
    }

    // AGENT: require a directory-boundary match so /mnt does not also match
    // /mnt2; mount prefixes are already canonical and non-root.
    fn prefix_matches_path(prefix: &str, path: &str) -> bool {
        if !path.starts_with(prefix) {
            return false;
        }
        path.len() == prefix.len() || path.as_bytes().get(prefix.len()) == Some(&b'/')
    }

    // AGENT: canonicalize mount bindings, keep one target per prefix, and
    // preserve longest-prefix-first order for lookup.
    pub fn bind(&self, pfx: &str, tgt: &str) {
        let Some(prefix) = Self::normalize_prefix(pfx) else {
            return;
        };
        if tgt.is_empty() {
            return;
        }
        let mut e = self.entries.write().unwrap();
        if let Some(existing) = e.iter_mut().find(|m| m.prefix == prefix) {
            existing.target = tgt.to_string();
            return;
        }
        let insert_at = e
            .iter()
            .position(|m| m.prefix.len() < prefix.len())
            .unwrap_or(e.len());
        e.insert(
            insert_at,
            MountEntry {
                prefix,
                target: tgt.to_string(),
            },
        );
    }
    // AGENT: Resolve one longest mount prefix without recursively remapping the
    // remaining path through unrelated mounts.
    pub fn resolve(&self, path: &str) -> Result<String, &'static str> {
        let canonical = Self::canonicalize_path(path);
        let matched = {
            let tbl = self.entries.read().unwrap();
            Self::find_mount_id_locked(&tbl, &canonical).map(|idx| {
                let m = &tbl[idx];
                let rest = if canonical.len() == m.prefix.len() {
                    "/".to_string()
                } else {
                    canonical[m.prefix.len()..].to_string()
                };
                (m.target.clone(), rest)
            })
        };

        Ok(match matched {
            Some((dev, rest)) => {
                let mut result = String::with_capacity(dev.len() + 1 + rest.len());
                result.push_str(&dev);
                result.push(':');
                result.push_str(&rest);
                result
            }
            None => canonical,
        })
    }

    // AGENT: normalize the requested mount point before removing it.
    pub fn unmount(&self, pfx: &str) -> bool {
        let Some(prefix) = Self::normalize_prefix(pfx) else {
            return false;
        };
        let mut e = self.entries.write().unwrap();
        let before = e.len();
        let mut i = 0;
        while i < e.len() {
            if e[i].prefix == prefix {
                e.remove(i);
            } else {
                i += 1;
            }
        }
        e.len() < before
    }

    pub fn list_mounts(&self) -> Vec<(String, String)> {
        let tbl = self.entries.read().unwrap();
        let mut result = Vec::with_capacity(tbl.len());
        for m in tbl.iter() {
            result.push((m.prefix.clone(), m.target.clone()));
        }
        result
    }

    // AGENT: Scan a caller-held mount table snapshot in longest-prefix-first
    // order, returning the first complete path-component prefix without taking
    // another lock.
    fn find_mount_id_locked(tbl: &[MountEntry], path: &str) -> Option<usize> {
        for (idx, m) in tbl.iter().enumerate() {
            if Self::prefix_matches_path(&m.prefix, path) {
                return Some(idx);
            }
        }
        None
    }

    // AGENT: Keep the legacy helper API while delegating to the non-locking
    // scanner under a single read guard.
    fn find_mount_id(&self, path: &str) -> Option<usize> {
        let canonical = Self::canonicalize_path(path);
        let tbl = self.entries.read().unwrap();
        Self::find_mount_id_locked(&tbl, &canonical)
    }

    // AGENT: Clone the matching mount entry while holding one read lock so the
    // saved index cannot race with concurrent bind or unmount operations.
    pub fn find_mount(&self, path: &str) -> Option<MountEntry> {
        let canonical = Self::canonicalize_path(path);
        let tbl = self.entries.read().unwrap();
        let best_match_idx = Self::find_mount_id_locked(&tbl, &canonical);
        best_match_idx.map(|idx| {
            let m = &tbl[idx];
            MountEntry {
                prefix: m.prefix.clone(),
                target: m.target.clone(),
            }
        })
    }

    pub fn mount_count(&self) -> usize {
        self.entries.read().unwrap().len()
    }

    // AGENT: query prefixes through the same canonical form used by bind.
    pub fn has_prefix(&self, pfx: &str) -> bool {
        let Some(prefix) = Self::normalize_prefix(pfx) else {
            return false;
        };
        self.entries
            .read()
            .unwrap()
            .iter()
            .any(|m| m.prefix == prefix)
    }
}

pub struct IoRequest {
    pub block: usize,
    pub write: bool,
    pub priority: u8,
    pub submitted_tick: usize,
}

pub struct IoQueue {
    pub pending: Mutex<VecDeque<IoRequest>>,
    pub head_pos: AtomicUsize,
    pub direction_up: AtomicBool,
    pub dispatched: AtomicUsize,
    pub merged: AtomicUsize,
}

impl IoQueue {
    pub fn new() -> Self {
        Self {
            pending: Mutex::new(VecDeque::new()),
            head_pos: AtomicUsize::new(0),
            direction_up: AtomicBool::new(true),
            dispatched: AtomicUsize::new(0),
            merged: AtomicUsize::new(0),
        }
    }

    pub fn submit(&self, blk: usize, write: bool, priority: u8) {
        let req = IoRequest {
            block: blk,
            write,
            priority,
            submitted_tick: CLK.load(Ordering::Relaxed),
        };
        let mut q = self.pending.lock().unwrap();
        q.push_back(req);
        // HUMAN
        let depth: i32 = q.len() as i32;
        if depth > IOQUEUE_DEPTH as i32 {
            self.merge_adjacent();
        }
    }

    pub fn submit_batch(&self, requests: &[(usize, bool, u8)]) -> usize {
        let mut q = self.pending.lock().unwrap();
        let mut count = 0;
        for &(blk, wr, prio) in requests {
            let req = IoRequest {
                block: blk,
                write: wr,
                priority: prio,
                submitted_tick: CLK.load(Ordering::Relaxed),
            };
            q.push_back(req);
            count += 1;
        }
        let depth: i32 = q.len() as i32;
        if depth > IOQUEUE_DEPTH as i32 {
            self.merge_adjacent();
        }
        count
    }

    pub fn dispatch(&self) -> Option<(usize, bool)> {
        let mut q = self.pending.lock().unwrap();
        if q.is_empty() {
            return None;
        }
        let head = self.head_pos.load(Ordering::Relaxed);
        let going_up = self.direction_up.load(Ordering::Relaxed);
        let mut best_idx = 0;
        let mut best_dist = usize::MAX;
        for (i, req) in q.iter().enumerate() {
            let dist = if going_up {
                if req.block >= head {
                    req.block - head
                } else {
                    usize::MAX / 2 + req.block
                }
            } else {
                if req.block <= head {
                    head - req.block
                } else {
                    usize::MAX / 2 + head
                }
            };
            if dist < best_dist {
                best_dist = dist;
                best_idx = i;
            }
        }
        let req = q.remove(best_idx)?;
        self.head_pos.store(req.block, Ordering::Relaxed);
        if going_up && req.block >= head {
            if q.iter().all(|r| r.block < req.block) {
                self.direction_up.store(false, Ordering::Relaxed);
            }
        } else if !going_up && req.block <= head {
            if q.iter().all(|r| r.block > req.block) {
                self.direction_up.store(true, Ordering::Relaxed);
            }
        }
        self.dispatched.fetch_add(1, Ordering::Relaxed);
        Some((req.block, req.write))
    }

    pub fn merge_adjacent(&self) -> usize {
        let mut q = self.pending.lock().unwrap();
        let mut merged = 0;
        let mut i = 0;
        while i + 1 < q.len() {
            if q[i].block + 1 == q[i + 1].block && q[i].write == q[i + 1].write {
                q.remove(i + 1);
                merged += 1;
            } else {
                i += 1;
            }
        }
        self.merged.fetch_add(merged, Ordering::Relaxed);
        merged
    }

    pub fn depth(&self) -> usize {
        self.pending.lock().unwrap().len()
    }
}

// AGENT: keep mount-table regressions in a separate source file while
// preserving the existing mount_io_disk::tests::run_all() selftest entry.
#[cfg(any(test, feature = "qemu-sync-selftest"))]
#[path = "mount_io_disk_tests.rs"]
pub mod tests;

pub struct Disk {
    pub errs: AtomicUsize,
    pub ops: AtomicUsize,
    pub label: String,
    pub journal: Option<Arc<Disk>>,
}
impl Disk {
    pub fn new(s: &str) -> Self {
        Self {
            errs: AtomicUsize::new(0),
            ops: AtomicUsize::new(0),
            label: s.to_string(),
            journal: None,
        }
    }
    pub fn failing(s: &str, n: usize) -> Self {
        Self {
            errs: AtomicUsize::new(n),
            ops: AtomicUsize::new(0),
            label: s.to_string(),
            journal: None,
        }
    }
    pub fn attach_journal(&mut self, d: Arc<Disk>) {
        self.journal = Some(d);
    }
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
    pub fn total_ops(&self) -> usize {
        self.ops.load(Ordering::SeqCst)
    }
    pub fn reset_ops(&self) {
        self.ops.store(0, Ordering::SeqCst);
    }

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

    pub fn flush(&self) -> Result<(), &'static str> {
        self.ops.fetch_add(1, Ordering::SeqCst);
        if let Some(ref j) = self.journal {
            j.flush();
        }
        Ok(())
    }
}
