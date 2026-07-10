// AGENT
use super::*;

// AGENT: regular file handles only identify the backing file object. Per-open
// access flags, status flags, and current offset live in OpenFileDesc.
#[derive(Clone)]
pub struct FHandle {
    pub path: String,
    pub node: Arc<FileNode>,
    pub(super) storage: FileStorage,
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
    pub fn new(path: &str) -> Self {
        let storage = FileStorage::standalone();
        Self {
            path: path.to_string(),
            node: Arc::new(FileNode::regular(false)),
            storage,
        }
    }
    // AGENT: create a handle over a fresh regular file node.
    pub fn with_data(path: &str, d: Vec<u8>) -> Self {
        let storage = FileStorage::standalone();
        let node = Arc::new(FileNode::regular(false));
        node.write_initial_bytes(&storage, &d)
            .expect("standalone RAM file seed should fit");
        Self {
            path: path.to_string(),
            node,
            storage,
        }
    }
    // AGENT: open a descriptor over an existing shared FileNode.
    pub fn with_node(path: &str, node: Arc<FileNode>) -> Self {
        Self::with_node_on_storage(path, node, FileStorage::standalone())
    }

    // AGENT: open a descriptor over an existing FileNode using the Kernel-owned
    // RAM block backend that stores that node's file contents.
    pub fn with_node_on_storage(path: &str, node: Arc<FileNode>, storage: FileStorage) -> Self {
        Self {
            path: path.to_string(),
            node,
            storage,
        }
    }
    // AGENT: duplicate only the file object reference; open-description state is
    // intentionally not part of FHandle.
    pub fn dup(&self) -> Self {
        FHandle {
            path: self.path.clone(),
            node: self.node.clone(),
            storage: self.storage.clone(),
        }
    }

    // AGENT: expose the FileNode-owned byte-precise EOF through regular handles.
    pub fn len(&self) -> usize {
        self.node.len()
    }

    // AGENT: copy from a regular file node at an explicit offset without
    // touching descriptor state.
    fn copy_from_node_at(&self, off: usize, buf: &mut [u8]) -> Result<usize, &'static str> {
        self.node.read_bytes(&self.storage, off, buf)
    }

    // AGENT: read using state supplied by the owning OpenFileDesc.
    pub(super) fn read_with_state(
        &self,
        status: FdOpt,
        offset: &mut u64,
        buf: &mut [u8],
    ) -> Result<usize, &'static str> {
        if !status.rd {
            return Err("ebadf");
        }
        let off = match usize::try_from(*offset) {
            Ok(off) => off,
            Err(_) => return Ok(0),
        };
        let n = self.copy_from_node_at(off, buf)?;
        let moved = u64::try_from(n).map_err(|_| "efbig")?;
        *offset = offset.checked_add(moved).ok_or("efbig")?;
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

    // AGENT: direct positioned reads are pure file-object reads; fd permission
    // checks belong to OpenFileDesc.
    pub fn read_at(&self, off: usize, buf: &mut [u8]) -> Result<usize, &'static str> {
        self.copy_from_node_at(off, buf)
    }

    // AGENT: write using state supplied by the owning OpenFileDesc.
    pub(super) fn write_with_state(
        &self,
        status: FdOpt,
        offset: &mut u64,
        buf: &[u8],
    ) -> Result<usize, &'static str> {
        if !status.wr {
            return Err("ebadf");
        }
        let off = if status.ap {
            None
        } else {
            Some(usize::try_from(*offset).map_err(|_| "efbig")?)
        };
        let end = self.node.write_bytes(&self.storage, off, buf)?;
        *offset = u64::try_from(end).map_err(|_| "efbig")?;
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

    // AGENT: direct positioned writes are pure file-object writes; fd permission
    // checks belong to OpenFileDesc.
    pub fn write_at(&self, off: usize, buf: &[u8]) -> Result<usize, &'static str> {
        self.node.write_bytes(&self.storage, Some(off), buf)?;
        Ok(buf.len())
    }

    // AGENT: validate the legacy transfer-shaped API explicitly instead of
    // accepting arbitrary odd/even direction values or extra buffers.
    pub(super) fn transfer_with_state(
        &self,
        status: FdOpt,
        offset: &mut u64,
        dir: u8,
        positioned_offset: Option<usize>,
        buf_rd: Option<&mut [u8]>,
        buf_wr: Option<&[u8]>,
    ) -> Result<usize, &'static str> {
        match (dir, positioned_offset, buf_rd, buf_wr) {
            (Self::TRANSFER_READ, Some(off), Some(buf), None) => {
                self.read_at_with_status(status, off, buf)
            }
            (Self::TRANSFER_READ, None, Some(buf), None) => {
                self.read_with_state(status, offset, buf)
            }
            (Self::TRANSFER_WRITE, Some(off), None, Some(buf)) => {
                self.write_at_with_status(status, off, buf)
            }
            (Self::TRANSFER_WRITE, None, None, Some(buf)) => {
                self.write_with_state(status, offset, buf)
            }
            _ => Err("einval"),
        }
    }

    // AGENT: copy a regular-file byte range without changing descriptor state.
    pub(super) fn copy_chunk_at(&self, off: usize, count: usize) -> Result<Vec<u8>, &'static str> {
        if count == 0 {
            return Ok(Vec::new());
        }
        let mut data = vec![0; count];
        let n = self.copy_from_node_at(off, &mut data)?;
        data.truncate(n);
        Ok(data)
    }

    // AGENT: direct truncation mutates only the backing file object; write
    // permission checks belong to OpenFileDesc.
    pub fn set_len(&self, len: u64) -> Result<(), &'static str> {
        let len = usize::try_from(len).map_err(|_| "efbig")?;
        self.node.set_data_len(&self.storage, len)?;
        Ok(())
    }
    // AGENT: direct directory inspection stays stateless and uses the caller's
    // explicit entry index; fd-level iteration advances OpenFileDesc.
    pub fn read_entry(&self, idx: usize) -> Result<String, &'static str> {
        self.node.dir_entry_at(idx)
    }
    // AGENT: let OpenFileDesc provide the visible access mode when fd
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
    pub(super) fn io_ctl_with_offset(
        &self,
        cmd: usize,
        _arg: usize,
        offset: u64,
    ) -> Result<usize, &'static str> {
        match cmd {
            FIONREAD | TIOCINQ => {
                let len = self.len() as u64;
                usize::try_from(len.saturating_sub(offset)).map_err(|_| "eoverflow")
            }
            _ => Err("enotty"),
        }
    }

    // AGENT: direct allocation validates regular-file semantics and grows the
    // node through the single-lock FileNode helper.
    pub fn fallocate(&self, offset: usize, len: usize) -> Result<(), &'static str> {
        if self.node.kind != FileKind::Regular {
            return Err("enodev");
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
        f.debug_struct("FH").field("path", &self.path).finish()
    }
}
