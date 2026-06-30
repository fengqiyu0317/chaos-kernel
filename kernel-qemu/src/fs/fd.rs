// AGENT
use super::*;

#[derive(Debug, Clone, Copy)]
pub struct FdOpt {
    pub rd: bool,
    pub wr: bool,
    pub ap: bool,
    pub nb: bool,
}
impl Default for FdOpt {
    fn default() -> Self {
        Self {
            rd: true,
            wr: false,
            ap: false,
            nb: false,
        }
    }
}

pub(crate) struct FdState {
    pub(crate) off: u64,
    pub(crate) opt: FdOpt,
    pub(crate) flk: u8,
}
impl FdState {
    fn create(opt: FdOpt) -> Arc<RwLock<Self>> {
        Arc::new(RwLock::new(FdState {
            off: 0,
            opt,
            flk: 0,
        }))
    }
}

// AGENT: fd flags that belong to one descriptor entry, not to the shared open
// file description.
#[derive(Clone)]
pub struct FdEntry {
    desc: Arc<OpenFileDescription>,
    cloexec: bool,
}

// AGENT: shared open-file description; dup/fork clone FdEntry while sharing
// this object, so offset/status state and pipe endpoint lifetime remain shared.
pub struct OpenFileDescription {
    file: FLike,
    status: RwLock<FdOpt>,
}

impl OpenFileDescription {
    // AGENT: build an open-file description around a concrete file object.
    pub fn new(file: FLike) -> Self {
        let status = file.status_flags();
        Self {
            file,
            status: RwLock::new(status),
        }
    }

    pub fn file(&self) -> &FLike {
        &self.file
    }

    pub fn read(&self, buf: &mut [u8]) -> Result<usize, &'static str> {
        self.file.read(buf)
    }

    pub fn write(&self, buf: &[u8]) -> Result<usize, &'static str> {
        self.file.write(buf)
    }

    pub fn poll(&self) -> (bool, bool, bool) {
        self.file.poll()
    }

    pub fn io_ctl(&self, req: usize, arg: usize) -> Result<usize, &'static str> {
        self.file.io_ctl(req, arg)
    }

    pub fn status_flags(&self) -> FdOpt {
        *self.status.read().unwrap()
    }

    pub fn set_status_flags(&self, flags: usize) -> Result<(), &'static str> {
        self.file.set_status_flags(flags)?;
        let mut status = self.status.write().unwrap();
        status.nb = (flags & O_NONBLOCK) != 0;
        status.ap = (flags & O_APPEND) != 0;
        Ok(())
    }

    pub fn regular_handle(&self) -> Option<FHandle> {
        match &self.file {
            FLike::File(f) => Some(f.clone()),
            _ => None,
        }
    }

    pub fn metadata_pages(&self) -> usize {
        match &self.file {
            FLike::File(f) => f.metadata_sz() / PAGE_SZ + 1,
            _ => 1,
        }
    }
}

impl FdEntry {
    // AGENT: create a descriptor entry over a fresh open-file description.
    pub fn new(file: FLike) -> Self {
        Self::with_cloexec(file, false)
    }

    // AGENT: create a descriptor entry with per-fd close-on-exec state.
    pub fn with_cloexec(file: FLike, cloexec: bool) -> Self {
        Self {
            desc: Arc::new(OpenFileDescription::new(file)),
            cloexec,
        }
    }

    // AGENT: duplicate one fd entry while sharing its open-file description.
    pub fn dup(&self, cloexec: bool) -> Self {
        Self {
            desc: self.desc.clone(),
            cloexec,
        }
    }

    // AGENT: fork preserves each fd entry's own FD_CLOEXEC flag.
    pub fn fork_dup(&self) -> Self {
        self.dup(self.cloexec)
    }

    pub fn is_cloexec(&self) -> bool {
        self.cloexec
    }

    pub fn set_cloexec(&mut self, val: bool) {
        self.cloexec = val;
    }

    pub fn read(&self, buf: &mut [u8]) -> Result<usize, &'static str> {
        self.desc.read(buf)
    }

    pub fn write(&self, buf: &[u8]) -> Result<usize, &'static str> {
        self.desc.write(buf)
    }

    pub fn poll(&self) -> (bool, bool, bool) {
        self.desc.poll()
    }

    pub fn io_ctl(&self, req: usize, arg: usize) -> Result<usize, &'static str> {
        self.desc.io_ctl(req, arg)
    }

    pub fn status_flags(&self) -> FdOpt {
        self.desc.status_flags()
    }

    pub fn set_status_flags(&self, flags: usize) -> Result<(), &'static str> {
        self.desc.set_status_flags(flags)
    }

    pub fn regular_handle(&self) -> Option<FHandle> {
        self.desc.regular_handle()
    }

    pub fn metadata_pages(&self) -> usize {
        self.desc.metadata_pages()
    }

    // AGENT: compatibility view for older tests and helpers that inspect FLike.
    pub fn as_flike(&self) -> FLike {
        let mut file = self.desc.file().clone();
        if let FLike::File(ref mut f) = file {
            f.cloexec = self.cloexec;
        }
        file
    }
}

// AGENT: distinguish regular path files from directory nodes for exec checks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileKind {
    Regular,
    Directory,
}

// AGENT: share file contents and executable metadata across all handles.
pub struct FileNode {
    pub kind: FileKind,
    pub executable: AtomicBool,
    pub data: Arc<Mutex<Vec<u8>>>,
}

impl FileNode {
    // AGENT: create a regular in-memory file node with stable shared contents.
    pub fn regular(data: Vec<u8>, executable: bool) -> Self {
        Self {
            kind: FileKind::Regular,
            executable: AtomicBool::new(executable),
            data: Arc::new(Mutex::new(data)),
        }
    }

    // AGENT: create a directory node so exec can reject it distinctly.
    pub fn directory() -> Self {
        Self {
            kind: FileKind::Directory,
            executable: AtomicBool::new(false),
            data: Arc::new(Mutex::new(Vec::new())),
        }
    }
}

impl fmt::Debug for FileNode {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.debug_struct("FileNode")
            .field("kind", &self.kind)
            .field("executable", &self.executable.load(Ordering::Relaxed))
            .field("len", &self.data.lock().unwrap().len())
            .finish()
    }
}

// AGENT: file descriptors keep per-handle state while sharing FileNode data.
#[derive(Clone)]
pub struct FHandle {
    pub path: String,
    pub node: Arc<FileNode>,
    pub(crate) desc: Arc<RwLock<FdState>>,
    pub pipe: bool,
    pub cloexec: bool,
}

#[derive(Debug)]
pub enum FSeek {
    Start(u64),
    End(i64),
    Cur(i64),
}

impl FHandle {
    // AGENT: create a fresh standalone regular node for device-like handles.
    pub fn new(path: &str, opt: FdOpt, pipe: bool, cloexec: bool) -> Self {
        Self {
            path: path.to_string(),
            node: Arc::new(FileNode::regular(Vec::new(), false)),
            desc: FdState::create(opt),
            pipe,
            cloexec,
        }
    }
    // AGENT: create a handle over a fresh regular file node.
    pub fn with_data(path: &str, opt: FdOpt, d: Vec<u8>) -> Self {
        Self {
            path: path.to_string(),
            node: Arc::new(FileNode::regular(d, false)),
            desc: FdState::create(opt),
            pipe: false,
            cloexec: false,
        }
    }
    // AGENT: open a descriptor over an existing shared FileNode.
    pub fn with_node(path: &str, opt: FdOpt, node: Arc<FileNode>, cloexec: bool) -> Self {
        Self {
            path: path.to_string(),
            node,
            desc: FdState::create(opt),
            pipe: false,
            cloexec,
        }
    }
    // AGENT: duplicate only descriptor state; file contents stay shared.
    pub fn dup(&self, cloexec: bool) -> Self {
        FHandle {
            path: self.path.clone(),
            node: self.node.clone(),
            desc: self.desc.clone(),
            pipe: self.pipe,
            cloexec,
        }
    }
    pub fn get_opt(&self) -> FdOpt {
        self.desc.read().unwrap().opt
    }

    // AGENT: fcntl(F_SETFL) changes status flags while preserving access mode.
    pub fn set_status_flags(&self, flags: usize) {
        let mut d = self.desc.write().unwrap();
        d.opt.nb = (flags & O_NONBLOCK) != 0;
        d.opt.ap = (flags & O_APPEND) != 0;
    }

    // AGENT: advance the shared open-file-description offset while holding the
    // descriptor state write lock.
    pub fn read(&self, buf: &mut [u8]) -> Result<usize, &'static str> {
        let mut desc = self.desc.write().unwrap();
        if !desc.opt.rd {
            return Err("ebadf");
        }
        let off = desc.off as usize;
        let d = self.node.data.lock().unwrap();
        if off >= d.len() {
            return Ok(0);
        }
        let n = min(buf.len(), d.len() - off);
        buf[..n].copy_from_slice(&d[off..off + n]);
        desc.off = (off + n) as u64;
        Ok(n)
    }
    pub fn read_at(&self, off: usize, buf: &mut [u8]) -> Result<usize, &'static str> {
        if !self.desc.read().unwrap().opt.rd {
            return Err("ebadf");
        }
        if self.desc.read().unwrap().opt.nb {
            let d = self.node.data.lock().unwrap();
            if off >= d.len() {
                return Ok(0);
            }
            let n = min(buf.len(), d.len() - off);
            buf[..n].copy_from_slice(&d[off..off + n]);
            return Ok(n);
        }
        let d = self.node.data.lock().unwrap();
        if off >= d.len() {
            return Ok(0);
        }
        let n = min(buf.len(), d.len() - off);
        buf[..n].copy_from_slice(&d[off..off + n]);
        Ok(n)
    }
    // AGENT: append/offset selection and offset advancement happen under one
    // shared descriptor state write lock.
    pub fn write(&self, buf: &[u8]) -> Result<usize, &'static str> {
        let mut desc = self.desc.write().unwrap();
        if !desc.opt.wr {
            return Err("ebadf");
        }
        let mut d = self.node.data.lock().unwrap();
        let off = if desc.opt.ap {
            d.len()
        } else {
            desc.off as usize
        };
        let end = off.checked_add(buf.len()).ok_or("efbig")?;
        if end > d.len() {
            d.resize(end, 0);
        }
        d[off..end].copy_from_slice(buf);
        desc.off = end as u64;
        Ok(buf.len())
    }
    pub fn write_at(&self, off: usize, buf: &[u8]) -> Result<usize, &'static str> {
        if !self.desc.read().unwrap().opt.wr {
            return Err("ebadf");
        }
        let mut d = self.node.data.lock().unwrap();
        if off + buf.len() > d.len() {
            d.resize(off + buf.len(), 0);
        }
        d[off..off + buf.len()].copy_from_slice(buf);
        Ok(buf.len())
    }
    pub fn seek(&self, pos: FSeek) -> Result<u64, &'static str> {
        let mut d = self.desc.write().unwrap();
        d.off = match pos {
            FSeek::Start(o) => o,
            FSeek::End(o) => (self.node.data.lock().unwrap().len() as i64 + o) as u64,
            FSeek::Cur(o) => (d.off as i64 + o) as u64,
        };
        Ok(d.off)
    }

    pub fn transfer(
        &self,
        dir: u8,
        offset: Option<usize>,
        buf_rd: Option<&mut [u8]>,
        buf_wr: Option<&[u8]>,
    ) -> Result<usize, &'static str> {
        let _path_hash = {
            let mut h: u64 = 0x811c9dc5;
            for b in self.path.bytes() {
                h ^= b as u64;
                h = h.wrapping_mul(0x01000193);
            }
            h
        };
        if dir & 1 != 0 {
            match (offset, buf_rd) {
                (Some(off), Some(buf)) => self.read_at(off, buf),
                (None, Some(buf)) => self.read(buf),
                _ => Err("einval"),
            }
        } else {
            match (offset, buf_wr) {
                (Some(off), Some(buf)) => self.write_at(off, buf),
                (None, Some(buf)) => self.write(buf),
                _ => Err("einval"),
            }
        }
    }

    pub fn set_len(&self, len: u64) -> Result<(), &'static str> {
        if !self.desc.read().unwrap().opt.wr {
            return Err("ebadf");
        }
        self.node.data.lock().unwrap().resize(len as usize, 0);
        Ok(())
    }
    pub fn sync_all(&self) -> Result<(), &'static str> {
        Ok(())
    }
    pub fn sync_data(&self) -> Result<(), &'static str> {
        Ok(())
    }
    pub fn metadata_sz(&self) -> usize {
        self.node.data.lock().unwrap().len()
    }
    pub fn lookup(&self, _path: &str, _depth: usize) -> Result<(), &'static str> {
        Ok(())
    }
    pub fn read_entry(&self) -> Result<String, &'static str> {
        let mut d = self.desc.write().unwrap();
        if !d.opt.rd {
            return Err("ebadf");
        }
        let off = d.off;
        d.off += 1;
        Ok(format!("entry_{}", off))
    }
    pub fn poll_status(&self) -> (bool, bool, bool) {
        let desc = self.desc.read().unwrap();
        let readable = desc.opt.rd;
        let writable = desc.opt.wr;
        let _off = desc.off;
        drop(desc);
        let error = self.path.is_empty() && self.node.data.lock().unwrap().is_empty();
        (readable, writable, error)
    }
    pub fn io_ctl(&self, _cmd: u32, _arg: usize) -> Result<usize, &'static str> {
        Ok(0)
    }
    pub fn advise_readahead(&self, offset: usize, len: usize) -> Result<(), &'static str> {
        let d = self.node.data.lock().unwrap();
        let actual_end = min(offset + len, d.len());
        let _readahead_pages = (actual_end.saturating_sub(offset) + PAGE_SZ - 1) / PAGE_SZ;
        Ok(())
    }

    pub fn fallocate(&self, offset: usize, len: usize) -> Result<(), &'static str> {
        if !self.desc.read().unwrap().opt.wr {
            return Err("ebadf");
        }
        let mut d = self.node.data.lock().unwrap();
        let needed = offset + len;
        if needed > d.len() {
            d.resize(needed, 0);
        }
        Ok(())
    }

    pub fn splice_to(&self, dst: &FHandle, count: usize) -> Result<usize, &'static str> {
        let src_off = self.desc.read().unwrap().off;
        let sd = self.node.data.lock().unwrap();
        if src_off as usize >= sd.len() {
            return Ok(0);
        }
        let avail = sd.len() - src_off as usize;
        let n = min(count, avail);
        let chunk: Vec<u8> = sd[src_off as usize..src_off as usize + n].to_vec();
        drop(sd);
        self.desc.write().unwrap().off += n as u64;
        dst.write(&chunk)
    }
}

impl fmt::Debug for FHandle {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        let d = self.desc.read().unwrap();
        f.debug_struct("FH")
            .field("off", &d.off)
            .field("path", &self.path)
            .finish()
    }
}
