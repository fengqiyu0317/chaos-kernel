// AGENT
use super::*;

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

// AGENT: regular file handles keep the offset state plus the open-time status
// snapshot; fd-table I/O reads mutable status from OpenFileDescription.
#[derive(Clone)]
pub struct FHandle {
    pub path: String,
    pub node: Arc<FileNode>,
    pub(super) storage: FileStorage,
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
    pub(super) const TRANSFER_WRITE: u8 = 0;
    pub(super) const TRANSFER_READ: u8 = 1;

    // AGENT: create a fresh standalone regular node for device-like handles.
    pub fn new(path: &str, opt: FdOpt) -> Self {
        let storage = FileStorage::standalone();
        Self {
            path: path.to_string(),
            node: Arc::new(FileNode::regular(false)),
            storage,
            initial_status: opt,
            desc: FdState::create(),
        }
    }
    // AGENT: create a handle over a fresh regular file node.
    pub fn with_data(path: &str, opt: FdOpt, d: Vec<u8>) -> Self {
        let storage = FileStorage::standalone();
        let node = Arc::new(FileNode::regular(false));
        node.write_initial_bytes(&storage, &d)
            .expect("standalone RAM file seed should fit");
        Self {
            path: path.to_string(),
            node,
            storage,
            initial_status: opt,
            desc: FdState::create(),
        }
    }
    // AGENT: open a descriptor over an existing shared FileNode.
    pub fn with_node(path: &str, opt: FdOpt, node: Arc<FileNode>) -> Self {
        Self::with_node_on_storage(path, opt, node, FileStorage::standalone())
    }

    // AGENT: open a descriptor over an existing FileNode using the Kernel-owned
    // RAM block backend that stores that node's file contents.
    pub fn with_node_on_storage(
        path: &str,
        opt: FdOpt,
        node: Arc<FileNode>,
        storage: FileStorage,
    ) -> Self {
        Self {
            path: path.to_string(),
            node,
            storage,
            initial_status: opt,
            desc: FdState::create(),
        }
    }
    // AGENT: duplicate only descriptor state; file contents stay shared.
    pub fn dup(&self) -> Self {
        FHandle {
            path: self.path.clone(),
            node: self.node.clone(),
            storage: self.storage.clone(),
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
    fn copy_from_node_at(&self, off: usize, buf: &mut [u8]) -> Result<usize, &'static str> {
        self.node.read_bytes(&self.storage, off, buf)
    }

    // AGENT: read using status supplied by the owning OpenFileDescription and
    // advance the shared regular-file offset under one descriptor-state lock.
    pub(super) fn read_with_status(
        &self,
        status: FdOpt,
        buf: &mut [u8],
    ) -> Result<usize, &'static str> {
        if !status.rd {
            return Err("ebadf");
        }
        let mut desc = self.desc.write().unwrap();
        let off = desc.off as usize;
        let n = self.copy_from_node_at(off, buf)?;
        desc.off = (off + n) as u64;
        Ok(n)
    }

    // AGENT: positioned reads use the supplied status and do not advance the
    // shared descriptor offset.
    fn read_at_with_status(
        &self,
        status: FdOpt,
        off: usize,
        buf: &mut [u8],
    ) -> Result<usize, &'static str> {
        if !status.rd {
            return Err("ebadf");
        }
        self.copy_from_node_at(off, buf)
    }

    // AGENT: direct handle positioned reads use the open-time status snapshot.
    pub fn read_at(&self, off: usize, buf: &mut [u8]) -> Result<usize, &'static str> {
        self.read_at_with_status(self.get_opt(), off, buf)
    }
    // AGENT: append/offset selection and offset advancement happen under one
    // shared descriptor state write lock.
    pub fn write(&self, buf: &[u8]) -> Result<usize, &'static str> {
        self.write_with_status(self.get_opt(), buf)
    }

    // AGENT: write using open-file-description status so F_SETFL changes are
    // visible without copying mutable flags back into FHandle.
    pub(super) fn write_with_status(
        &self,
        status: FdOpt,
        buf: &[u8],
    ) -> Result<usize, &'static str> {
        let mut desc = self.desc.write().unwrap();
        if !status.wr {
            return Err("ebadf");
        }
        let off = if status.ap {
            None
        } else {
            Some(desc.off as usize)
        };
        let end = self.node.write_bytes(&self.storage, off, buf)?;
        desc.off = end as u64;
        Ok(buf.len())
    }

    // AGENT: explicit-offset writes use the supplied status and do not advance
    // the shared file offset.
    fn write_at_with_status(
        &self,
        status: FdOpt,
        off: usize,
        buf: &[u8],
    ) -> Result<usize, &'static str> {
        if !status.wr {
            return Err("ebadf");
        }
        self.node.write_bytes(&self.storage, Some(off), buf)?;
        Ok(buf.len())
    }

    // AGENT: direct handle positioned writes use the open-time status snapshot.
    pub fn write_at(&self, off: usize, buf: &[u8]) -> Result<usize, &'static str> {
        self.write_at_with_status(self.get_opt(), off, buf)
    }
    // AGENT: compute seek targets with checked signed deltas so invalid offsets
    // fail instead of wrapping into huge u64 values.
    pub fn seek(&self, pos: FSeek) -> Result<u64, &'static str> {
        let mut d = self.desc.write().unwrap();
        let next = match pos {
            FSeek::Start(off) => off,
            FSeek::End(delta) => {
                let end = self.node.len() as u64;
                end.checked_add_signed(delta).ok_or("einval")?
            }
            FSeek::Cur(delta) => d.off.checked_add_signed(delta).ok_or("einval")?,
        };
        d.off = next;
        Ok(next)
    }

    // AGENT: validate the legacy transfer-shaped API explicitly instead of
    // accepting arbitrary odd/even direction values or extra buffers.
    pub(super) fn transfer_with_status(
        &self,
        status: FdOpt,
        dir: u8,
        offset: Option<usize>,
        buf_rd: Option<&mut [u8]>,
        buf_wr: Option<&[u8]>,
    ) -> Result<usize, &'static str> {
        match (dir, offset, buf_rd, buf_wr) {
            (Self::TRANSFER_READ, Some(off), Some(buf), None) => {
                self.read_at_with_status(status, off, buf)
            }
            (Self::TRANSFER_READ, None, Some(buf), None) => self.read_with_status(status, buf),
            (Self::TRANSFER_WRITE, Some(off), None, Some(buf)) => {
                self.write_at_with_status(status, off, buf)
            }
            (Self::TRANSFER_WRITE, None, None, Some(buf)) => self.write_with_status(status, buf),
            _ => Err("einval"),
        }
    }

    // AGENT: retained for direct-handle compatibility; fd-table callers should
    // prefer FdEntry::transfer so mutable fd status is not bypassed.
    pub fn transfer(
        &self,
        dir: u8,
        offset: Option<usize>,
        buf_rd: Option<&mut [u8]>,
        buf_wr: Option<&[u8]>,
    ) -> Result<usize, &'static str> {
        self.transfer_with_status(self.get_opt(), dir, offset, buf_rd, buf_wr)
    }

    // AGENT: copy a regular-file byte range without changing descriptor state.
    fn copy_chunk_at(&self, off: usize, count: usize) -> Result<Vec<u8>, &'static str> {
        if count == 0 {
            return Ok(Vec::new());
        }
        let mut data = vec![0; count];
        let n = self.node.read_bytes(&self.storage, off, &mut data)?;
        data.truncate(n);
        Ok(data)
    }

    // AGENT: splice with explicit fd status supplied by OpenFileDescription.
    pub(super) fn splice_to_with_status(
        &self,
        src_status: FdOpt,
        dst: &FHandle,
        dst_status: FdOpt,
        count: usize,
    ) -> Result<usize, &'static str> {
        if !src_status.rd || !dst_status.wr {
            return Err("ebadf");
        }
        if self.node.kind != FileKind::Regular || dst.node.kind != FileKind::Regular {
            return Err("enodev");
        }
        if count == 0 {
            return Ok(0);
        }
        if Arc::ptr_eq(&self.desc, &dst.desc) {
            return self.splice_same_description(dst, dst_status, count);
        }

        let src_key = Arc::as_ptr(&self.desc) as usize;
        let dst_key = Arc::as_ptr(&dst.desc) as usize;
        if src_key < dst_key {
            let mut src_desc = self.desc.write().unwrap();
            let mut dst_desc = dst.desc.write().unwrap();
            self.splice_locked(&mut src_desc, dst, &mut dst_desc, dst_status, count)
        } else {
            let mut dst_desc = dst.desc.write().unwrap();
            let mut src_desc = self.desc.write().unwrap();
            self.splice_locked(&mut src_desc, dst, &mut dst_desc, dst_status, count)
        }
    }

    // AGENT: handle dup-style self-splice without trying to take the same
    // descriptor lock twice.
    fn splice_same_description(
        &self,
        dst: &FHandle,
        dst_status: FdOpt,
        count: usize,
    ) -> Result<usize, &'static str> {
        let mut desc = self.desc.write().unwrap();
        let src_off = match usize::try_from(desc.off) {
            Ok(off) => off,
            Err(_) => return Ok(0),
        };
        let chunk = self.copy_chunk_at(src_off, count)?;
        if chunk.is_empty() {
            return Ok(0);
        }

        let write_off = if dst_status.ap {
            None
        } else {
            Some(src_off.checked_add(chunk.len()).ok_or("efbig")?)
        };
        let end = dst.node.write_bytes(&dst.storage, write_off, &chunk)?;
        desc.off = u64::try_from(end).map_err(|_| "efbig")?;
        Ok(chunk.len())
    }

    // AGENT: commit source and destination offsets only after the destination
    // write has succeeded.
    fn splice_locked(
        &self,
        src_desc: &mut FdState,
        dst: &FHandle,
        dst_desc: &mut FdState,
        dst_status: FdOpt,
        count: usize,
    ) -> Result<usize, &'static str> {
        let src_off = match usize::try_from(src_desc.off) {
            Ok(off) => off,
            Err(_) => return Ok(0),
        };
        let chunk = self.copy_chunk_at(src_off, count)?;
        if chunk.is_empty() {
            return Ok(0);
        }

        let write_off = if dst_status.ap {
            None
        } else {
            Some(usize::try_from(dst_desc.off).map_err(|_| "efbig")?)
        };
        let end = dst.node.write_bytes(&dst.storage, write_off, &chunk)?;
        let moved = u64::try_from(chunk.len()).map_err(|_| "efbig")?;
        src_desc.off = src_desc.off.checked_add(moved).ok_or("efbig")?;
        dst_desc.off = u64::try_from(end).map_err(|_| "efbig")?;
        Ok(chunk.len())
    }

    // AGENT: direct truncation uses the handle's open-time write permission.
    pub fn set_len(&self, len: u64) -> Result<(), &'static str> {
        if !self.initial_status.wr {
            return Err("ebadf");
        }
        let len = usize::try_from(len).map_err(|_| "efbig")?;
        self.node.set_data_len(&self.storage, len)?;
        Ok(())
    }
    // AGENT: keep node-local lookup honest; full path lookup belongs to Kernel.
    pub fn lookup(&self, path: &str, depth: usize) -> Result<(), &'static str> {
        if depth > 40 {
            return Err("eloop");
        }
        if path.bytes().any(|b| b == 0) {
            return Err("einval");
        }
        if self.node.kind != FileKind::Directory {
            return Err("enotdir");
        }
        if path.is_empty() || path == "." {
            return Ok(());
        }
        if path.contains('/') {
            return Err("einval");
        }
        if self.node.has_dir_entry(path)? {
            Ok(())
        } else {
            Err("enoent")
        }
    }
    // AGENT: directory-style iteration reads real directory entries and advances
    // the handle offset only after a name is returned.
    pub fn read_entry(&self) -> Result<String, &'static str> {
        if !self.initial_status.rd {
            return Err("ebadf");
        }
        let mut desc = self.desc.write().unwrap();
        let entry = self.node.dir_entry_at(desc.off as usize)?;
        desc.off += 1;
        Ok(entry)
    }
    // AGENT: regular files do not carry pipe-style closed-peer state.
    pub fn poll_status(&self) -> PollStatus {
        self.poll_status_with_status(self.get_opt())
    }

    // AGENT: let OpenFileDescription provide the visible access mode when fd
    // polling goes through the fd table.
    pub(super) fn poll_status_with_status(&self, status: FdOpt) -> PollStatus {
        PollStatus {
            readable: status.rd,
            writable: status.wr,
            error: false,
            closed: false,
        }
    }
    // AGENT: regular files only report supported ioctl results; unknown
    // requests must not be silently treated as success.
    pub fn io_ctl(&self, cmd: usize, _arg: usize) -> Result<usize, &'static str> {
        match cmd {
            FIONREAD | TIOCINQ => {
                let off = self.desc.read().unwrap().off;
                let len = self.node.len() as u64;
                usize::try_from(len.saturating_sub(off)).map_err(|_| "eoverflow")
            }
            _ => Err("enotty"),
        }
    }
    // AGENT: validate readahead hints without claiming to prefetch through the
    // current minimal RamBlockDevice backend.
    pub fn advise_readahead(&self, offset: usize, len: usize) -> Result<(), &'static str> {
        if self.node.kind != FileKind::Regular {
            return Err("enodev");
        }
        if !self.initial_status.rd {
            return Err("ebadf");
        }
        offset.checked_add(len).ok_or("efbig")?;
        Ok(())
    }

    // AGENT: direct allocation validates regular-file semantics and grows the
    // node through the single-lock FileNode helper.
    pub fn fallocate(&self, offset: usize, len: usize) -> Result<(), &'static str> {
        if self.node.kind != FileKind::Regular {
            return Err("enodev");
        }
        if !self.initial_status.wr {
            return Err("ebadf");
        }
        if len == 0 {
            return Err("einval");
        }
        let needed = offset.checked_add(len).ok_or("efbig")?;
        self.node.ensure_data_len_at_least(&self.storage, needed)?;
        Ok(())
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
