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

// AGENT: F_SETFL-style updates preserve access mode and only replace mutable
// status flags carried by the shared open-file description.
impl FdOpt {
    pub fn apply_status_flags(&mut self, flags: usize) {
        self.nb = (flags & O_NONBLOCK) != 0;
        self.ap = (flags & O_APPEND) != 0;
    }
}

// AGENT: regular-file descriptor state owns only the mutable offset; fd status
// flags live in OpenFileDescription so fcntl/ioctl updates have one authority.
pub(crate) struct FdState {
    pub(crate) off: u64,
}
impl FdState {
    fn create() -> Arc<RwLock<Self>> {
        Arc::new(RwLock::new(FdState { off: 0 }))
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

    // AGENT: enforce open-file-description access flags before dispatching to
    // the concrete file-like object.
    pub fn read(&self, buf: &mut [u8]) -> Result<usize, &'static str> {
        let status = self.status_flags();
        if !status.rd {
            return Err("ebadf");
        }
        match &self.file {
            FLike::File(f) => f.read_with_status(status, buf),
            FLike::Pipe(p) => p.read_at(buf),
            FLike::Ep(_) => Err("enosys"),
        }
    }

    // AGENT: keep append/write permission checks in the shared open-file
    // description instead of duplicating mutable status in FHandle.
    pub fn write(&self, buf: &[u8]) -> Result<usize, &'static str> {
        let status = self.status_flags();
        if !status.wr {
            return Err("ebadf");
        }
        match &self.file {
            FLike::File(f) => f.write_with_status(status, buf),
            FLike::Pipe(p) => p.write_at(buf),
            FLike::Ep(_) => Err("enosys"),
        }
    }

    // AGENT: return explicit poll status so epoll can preserve closed peer state.
    pub fn poll(&self) -> PollStatus {
        match &self.file {
            FLike::File(f) => f.poll_status_with_status(self.status_flags()),
            _ => self.file.poll(),
        }
    }

    pub fn io_ctl(&self, req: usize, arg: usize) -> Result<usize, &'static str> {
        self.file.io_ctl(req, arg)
    }

    pub fn status_flags(&self) -> FdOpt {
        *self.status.read().unwrap()
    }

    // AGENT: expose status flags in the same bit shape returned by F_GETFL so
    // checkpoint code does not duplicate fd option encoding.
    pub fn status_flags_bits(&self) -> usize {
        fdopt_to_open_flags(self.status_flags())
    }

    // AGENT: update only the mutable open-file status bits; access mode remains
    // fixed from open/pipe creation.
    pub fn set_status_flags(&self, flags: usize) -> Result<(), &'static str> {
        let mut status = self.status.write().unwrap();
        status.apply_status_flags(flags);
        Ok(())
    }

    pub fn regular_handle(&self) -> Option<FHandle> {
        match &self.file {
            FLike::File(f) => Some(f.clone()),
            _ => None,
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

    // AGENT: expose epoll instances to fd-table lifecycle cleanup without
    // leaking the open-file-description internals.
    pub fn epoll_instance(&self) -> Option<EpInst> {
        match self.desc.file() {
            FLike::Ep(inst) => Some(inst.clone()),
            _ => None,
        }
    }

    // AGENT: compare open-file-description identity so close can distinguish the
    // last fd-table reference from temporary cloned FdEntry handles.
    pub fn same_open_description(&self, other: &FdEntry) -> bool {
        Arc::ptr_eq(&self.desc, &other.desc)
    }

    // AGENT: remove a source-backed epoll subscription from this file object.
    pub fn unregister_epoll_source(&self, sub_id: usize) -> bool {
        self.desc.file().unregister_epoll(sub_id)
    }

    pub fn read(&self, buf: &mut [u8]) -> Result<usize, &'static str> {
        self.desc.read(buf)
    }

    pub fn write(&self, buf: &[u8]) -> Result<usize, &'static str> {
        self.desc.write(buf)
    }

    // AGENT: forward explicit poll status through the descriptor entry layer.
    pub fn poll(&self) -> PollStatus {
        self.desc.poll()
    }

    pub fn io_ctl(&self, req: usize, arg: usize) -> Result<usize, &'static str> {
        self.desc.io_ctl(req, arg)
    }

    pub fn status_flags(&self) -> FdOpt {
        self.desc.status_flags()
    }

    // AGENT: expose serialized status flags for checkpoint fd table snapshots.
    pub fn status_flags_bits(&self) -> usize {
        self.desc.status_flags_bits()
    }

    pub fn set_status_flags(&self, flags: usize) -> Result<(), &'static str> {
        self.desc.set_status_flags(flags)
    }

    pub fn regular_handle(&self) -> Option<FHandle> {
        self.desc.regular_handle()
    }

    // AGENT: compatibility view for older tests and helpers that inspect FLike.
    pub fn as_flike(&self) -> FLike {
        self.desc.file().clone()
    }
}

// AGENT: keep fd status flag encoding shared by fcntl-style reporting and the
// checkpoint image snapshot path.
pub fn fdopt_to_open_flags(opt: FdOpt) -> usize {
    let mut flags = match (opt.rd, opt.wr) {
        (true, true) => 2,
        (false, true) => 1,
        _ => 0,
    };
    if opt.nb {
        flags |= O_NONBLOCK;
    }
    if opt.ap {
        flags |= O_APPEND;
    }
    flags
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

// AGENT: regular file handles keep the offset state plus the open-time status
// snapshot; fd-table I/O reads mutable status from OpenFileDescription.
#[derive(Clone)]
pub struct FHandle {
    pub path: String,
    pub node: Arc<FileNode>,
    initial_status: FdOpt,
    pub(crate) desc: Arc<RwLock<FdState>>,
}

#[derive(Debug)]
pub enum FSeek {
    Start(u64),
    End(i64),
    Cur(i64),
}

impl FHandle {
    // AGENT: create a fresh standalone regular node for device-like handles.
    pub fn new(path: &str, opt: FdOpt) -> Self {
        Self {
            path: path.to_string(),
            node: Arc::new(FileNode::regular(Vec::new(), false)),
            initial_status: opt,
            desc: FdState::create(),
        }
    }
    // AGENT: create a handle over a fresh regular file node.
    pub fn with_data(path: &str, opt: FdOpt, d: Vec<u8>) -> Self {
        Self {
            path: path.to_string(),
            node: Arc::new(FileNode::regular(d, false)),
            initial_status: opt,
            desc: FdState::create(),
        }
    }
    // AGENT: open a descriptor over an existing shared FileNode.
    pub fn with_node(path: &str, opt: FdOpt, node: Arc<FileNode>) -> Self {
        Self {
            path: path.to_string(),
            node,
            initial_status: opt,
            desc: FdState::create(),
        }
    }
    // AGENT: duplicate only descriptor state; file contents stay shared.
    pub fn dup(&self) -> Self {
        FHandle {
            path: self.path.clone(),
            node: self.node.clone(),
            initial_status: self.initial_status,
            desc: self.desc.clone(),
        }
    }
    // AGENT: expose the immutable open-time access mode used to seed a new
    // OpenFileDescription; mutable status changes are read from FdEntry instead.
    pub fn get_opt(&self) -> FdOpt {
        self.initial_status
    }

    // AGENT: expose the shared open-file-description offset for checkpoint fd
    // snapshots without leaking the descriptor lock itself.
    pub fn offset(&self) -> u64 {
        self.desc.read().unwrap().off
    }

    // AGENT: direct handle reads use the open-time status snapshot; normal
    // fd-table reads pass the current OpenFileDescription status below.
    pub fn read(&self, buf: &mut [u8]) -> Result<usize, &'static str> {
        self.read_with_status(self.get_opt(), buf)
    }

    // AGENT: copy from a regular file node at an explicit offset without
    // touching descriptor state.
    fn copy_from_node_at(&self, off: usize, buf: &mut [u8]) -> usize {
        let d = self.node.data.lock().unwrap();
        if off >= d.len() {
            return 0;
        }
        let n = min(buf.len(), d.len() - off);
        buf[..n].copy_from_slice(&d[off..off + n]);
        n
    }

    // AGENT: read using status supplied by the owning OpenFileDescription and
    // advance the shared regular-file offset under one descriptor-state lock.
    fn read_with_status(&self, status: FdOpt, buf: &mut [u8]) -> Result<usize, &'static str> {
        if !status.rd {
            return Err("ebadf");
        }
        let mut desc = self.desc.write().unwrap();
        let off = desc.off as usize;
        let n = self.copy_from_node_at(off, buf);
        desc.off = (off + n) as u64;
        Ok(n)
    }

    // AGENT: positioned reads use only the immutable read access mode and do
    // not advance the shared descriptor offset.
    pub fn read_at(&self, off: usize, buf: &mut [u8]) -> Result<usize, &'static str> {
        if !self.initial_status.rd {
            return Err("ebadf");
        }
        Ok(self.copy_from_node_at(off, buf))
    }
    // AGENT: append/offset selection and offset advancement happen under one
    // shared descriptor state write lock.
    pub fn write(&self, buf: &[u8]) -> Result<usize, &'static str> {
        self.write_with_status(self.get_opt(), buf)
    }

    // AGENT: write using open-file-description status so F_SETFL changes are
    // visible without copying mutable flags back into FHandle.
    fn write_with_status(&self, status: FdOpt, buf: &[u8]) -> Result<usize, &'static str> {
        let mut desc = self.desc.write().unwrap();
        if !status.wr {
            return Err("ebadf");
        }
        let mut d = self.node.data.lock().unwrap();
        let off = if status.ap {
            d.len()
        } else {
            desc.off as usize
        };
        let end = Self::write_data_at(&mut d, off, buf)?;
        desc.off = end as u64;
        Ok(buf.len())
    }

    // AGENT: grow and copy one regular-file byte range with overflow checked in
    // one place so offset writes and append writes share the same bounds logic.
    fn write_data_at(d: &mut Vec<u8>, off: usize, buf: &[u8]) -> Result<usize, &'static str> {
        if buf.is_empty() {
            return Ok(off);
        }
        let end = off.checked_add(buf.len()).ok_or("efbig")?;
        if end > d.len() {
            d.resize(end, 0);
        }
        d[off..end].copy_from_slice(buf);
        Ok(end)
    }

    // AGENT: explicit-offset writes do not advance the shared file offset, but
    // they still use the same checked range write as ordinary writes.
    pub fn write_at(&self, off: usize, buf: &[u8]) -> Result<usize, &'static str> {
        if !self.initial_status.wr {
            return Err("ebadf");
        }
        let mut d = self.node.data.lock().unwrap();
        Self::write_data_at(&mut d, off, buf)?;
        Ok(buf.len())
    }
    // AGENT: compute seek targets with checked signed deltas so invalid offsets
    // fail instead of wrapping into huge u64 values.
    pub fn seek(&self, pos: FSeek) -> Result<u64, &'static str> {
        let mut d = self.desc.write().unwrap();
        let next = match pos {
            FSeek::Start(off) => off,
            FSeek::End(delta) => {
                let end = self.node.data.lock().unwrap().len() as u64;
                end.checked_add_signed(delta).ok_or("einval")?
            }
            FSeek::Cur(delta) => d.off.checked_add_signed(delta).ok_or("einval")?,
        };
        d.off = next;
        Ok(next)
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

    // AGENT: direct truncation uses the handle's open-time write permission.
    pub fn set_len(&self, len: u64) -> Result<(), &'static str> {
        if !self.initial_status.wr {
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
    pub fn lookup(&self, _path: &str, _depth: usize) -> Result<(), &'static str> {
        Ok(())
    }
    // AGENT: directory-style iteration uses the handle's open-time read
    // permission and advances only the regular handle offset.
    pub fn read_entry(&self) -> Result<String, &'static str> {
        let mut d = self.desc.write().unwrap();
        if !self.initial_status.rd {
            return Err("ebadf");
        }
        let off = d.off;
        d.off += 1;
        Ok(format!("entry_{}", off))
    }
    // AGENT: regular files do not carry pipe-style closed-peer state.
    pub fn poll_status(&self) -> PollStatus {
        self.poll_status_with_status(self.get_opt())
    }

    // AGENT: let OpenFileDescription provide the visible access mode when fd
    // polling goes through the fd table.
    fn poll_status_with_status(&self, status: FdOpt) -> PollStatus {
        let error = self.path.is_empty() && self.node.data.lock().unwrap().is_empty();
        PollStatus {
            readable: status.rd,
            writable: status.wr,
            error,
            closed: false,
        }
    }
    pub fn io_ctl(&self, _cmd: u32, _arg: usize) -> Result<usize, &'static str> {
        Ok(0)
    }
    pub fn advise_readahead(&self, offset: usize, len: usize) -> Result<(), &'static str> {
        let d = self.node.data.lock().unwrap();
        let requested_end = offset.saturating_add(len);
        let actual_end = min(requested_end, d.len());
        let _readahead_pages = (actual_end.saturating_sub(offset) + PAGE_SZ - 1) / PAGE_SZ;
        Ok(())
    }

    // AGENT: direct allocation uses checked ranges and the handle's open-time
    // write permission.
    pub fn fallocate(&self, offset: usize, len: usize) -> Result<(), &'static str> {
        if !self.initial_status.wr {
            return Err("ebadf");
        }
        let mut d = self.node.data.lock().unwrap();
        let needed = offset.checked_add(len).ok_or("efbig")?;
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
