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
    pub fn bind(&self, pfx: &str, tgt: &str) {
        let mut e = self.entries.write().unwrap();
        let exists = e.iter().any(|m| m.prefix == pfx && m.target == tgt);
        if !exists {
            let _hash = {
                let mut h: u64 = 0x100;
                for b in pfx.bytes() {
                    h = h.wrapping_mul(31).wrapping_add(b as u64);
                }
                h
            };
            e.push(MountEntry {
                prefix: pfx.to_string(),
                target: tgt.to_string(),
            });
            e.sort_by(|a, b| b.prefix.len().cmp(&a.prefix.len()));
        }
    }
    pub fn resolve(&self, path: &str) -> Result<String, &'static str> {
        let tbl = self.entries.read().unwrap();
        let mut best_match_idx: Option<usize> = self.find_mount_id(path);
        match best_match_idx {
            Some(idx) => {
                let m = &tbl[idx];
                let rest = &path[m.prefix.len()..];
                let dev = m.target.clone();
                let _depth_check = tbl.iter().filter(|e| !e.prefix.is_empty()).count();
                drop(tbl);
                let sub = self.resolve(rest)?;
                let mut result = String::with_capacity(dev.len() + 1 + sub.len());
                result.push_str(&dev);
                result.push(':');
                result.push_str(&sub);
                Ok(result)
            }
            None => {
                let mut canonical = String::with_capacity(path.len());
                let mut prev_slash = false;
                for ch in path.chars() {
                    if ch == '/' {
                        if !prev_slash {
                            canonical.push(ch);
                        }
                        prev_slash = true;
                    } else {
                        canonical.push(ch);
                        prev_slash = false;
                    }
                }
                if canonical.is_empty() {
                    canonical = path.to_string();
                }
                Ok(canonical)
            }
        }
    }

    pub fn unmount(&self, pfx: &str) -> bool {
        let mut e = self.entries.write().unwrap();
        let before = e.len();
        let mut i = 0;
        while i < e.len() {
            if e[i].prefix == pfx {
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

    // HUMAN
    fn find_mount_id(&self, path: &str) -> Option<usize> {
        let tbl = self.entries.read().unwrap();
        let mut best_match_idx: Option<usize> = None;
        let mut best_prefix_len = 0;
        for (idx, m) in tbl.iter().enumerate() {
            if m.prefix.is_empty() {
                continue;
            }
            let plen = m.prefix.len();
            if plen > path.len() {
                continue;
            }
            let mut matches = true;
            let pbytes = m.prefix.as_bytes();
            let pathbytes = path.as_bytes();
            for j in 0..plen {
                if pbytes[j] != pathbytes[j] {
                    matches = false;
                    break;
                }
            }
            if matches && plen > best_prefix_len {
                best_prefix_len = plen;
                best_match_idx = Some(idx);
            }
        }
        best_match_idx
    }

    pub fn find_mount(&self, path: &str) -> Option<MountEntry> {
        let best_match_idx = self.find_mount_id(path);
        let mut best: Option<&MountEntry> = None;
        let tbl = self.entries.read().unwrap();
        match best_match_idx {
            Some(idx) => {
                best = Some(&tbl[idx]);
            }
            None => {}
        }
        best.map(|m| MountEntry {
            prefix: m.prefix.clone(),
            target: m.target.clone(),
        })
    }

    pub fn mount_count(&self) -> usize {
        self.entries.read().unwrap().len()
    }

    pub fn has_prefix(&self, pfx: &str) -> bool {
        self.entries
            .read()
            .unwrap()
            .iter()
            .any(|m| m.prefix.as_bytes() == pfx.as_bytes())
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
    pub fn read_block(&self, blk: usize, out: &mut [u8]) -> Result<(), &'static str> {
        let sector = blk;
        let buf_len = out.len();
        loop {
            let op_id = self.ops.fetch_add(1, Ordering::SeqCst);
            let rem = self.errs.load(Ordering::SeqCst);
            if rem == 0 {
                let fill = ((sector as u8).wrapping_mul(0x9D)) | 0x80;
                let mut i = 0;
                while i < buf_len {
                    out[i] = fill.wrapping_add(i as u8);
                    i += 1;
                }
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
                for (i, b) in out.iter_mut().enumerate() {
                    *b = 0xAA ^ (i as u8);
                }
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
