// AGENT
use super::*;

#[derive(Debug)]
pub enum FSeek {
    Start(u64),
    End(i64),
    Cur(i64),
}

// AGENT: distinguish an OFD-owned file position from one copied in through a
// non-null splice off_t pointer.
#[derive(Debug)]
pub enum SpliceFilePos {
    Shared,
    Explicit(u64),
}

impl SpliceFilePos {
    // AGENT: expose only caller-owned positions for syscall copy-out; shared
    // positions remain encapsulated by FHandle.
    pub fn explicit(&self) -> Option<u64> {
        match self {
            Self::Shared => None,
            Self::Explicit(offset) => Some(*offset),
        }
    }
}

// AGENT: regular-file handle for one open-file description. It owns the
// per-open offset while FInstance owns only the backing file object.
pub struct FHandle {
    instance: FInstance,
    offset: RwLock<u64>,
}

impl Clone for FHandle {
    // AGENT: cloning this compatibility view snapshots the current offset; fd
    // dup/fork still share state through Arc<OpenFileDesc>, not through Clone.
    fn clone(&self) -> Self {
        Self {
            instance: self.instance.clone(),
            offset: RwLock::new(self.offset()),
        }
    }
}

impl FHandle {
    // AGENT: create the regular-file per-open layer around a backing instance.
    pub fn new(instance: FInstance) -> Self {
        Self {
            instance,
            offset: RwLock::new(0),
        }
    }

    pub fn instance(&self) -> &FInstance {
        &self.instance
    }

    pub fn len(&self) -> usize {
        self.instance.len()
    }

    // AGENT: expose backing inode attributes without mixing the per-open offset
    // into stat results shared by every handle for this file.
    pub fn file_attr(&self) -> Result<FileAttr, &'static str> {
        self.instance.file_attr()
    }

    pub fn offset(&self) -> u64 {
        *self.offset.read().unwrap()
    }

    // AGENT: read advances this regular-file handle's per-open offset.
    pub(super) fn read_with_status(
        &self,
        status: FdOpt,
        buf: &mut [u8],
    ) -> Result<usize, &'static str> {
        if !status.rd {
            return Err("ebadf");
        }
        let mut offset = self.offset.write().unwrap();
        let off = match usize::try_from(*offset) {
            Ok(off) => off,
            Err(_) => return Ok(0),
        };
        let n = self.instance.read_at(off, buf)?;
        let moved = u64::try_from(n).map_err(|_| "efbig")?;
        *offset = offset.checked_add(moved).ok_or("efbig")?;
        Ok(n)
    }

    // AGENT: write advances this regular-file handle's per-open offset, while
    // O_APPEND redirects the backing write to the file EOF.
    pub(super) fn write_with_status(
        &self,
        status: FdOpt,
        buf: &[u8],
    ) -> Result<usize, &'static str> {
        if !status.wr {
            return Err("ebadf");
        }
        let mut offset = self.offset.write().unwrap();
        let off = if status.ap {
            None
        } else {
            Some(usize::try_from(*offset).map_err(|_| "efbig")?)
        };
        let end = self
            .instance
            .node
            .write_bytes(self.instance.storage(), off, buf)?;
        *offset = u64::try_from(end).map_err(|_| "efbig")?;
        Ok(buf.len())
    }

    // AGENT: directory iteration advances this regular-file handle's per-open
    // offset; the backing instance still reads by explicit entry index.
    pub(super) fn read_entry_with_status(&self, status: FdOpt) -> Result<String, &'static str> {
        if !status.rd {
            return Err("ebadf");
        }
        let mut offset = self.offset.write().unwrap();
        let idx = usize::try_from(*offset).map_err(|_| "eoverflow")?;
        let entry = self.instance.read_entry(idx)?;
        *offset = offset.checked_add(1).ok_or("eoverflow")?;
        Ok(entry)
    }

    pub(super) fn set_len(&self, len: u64) -> Result<(), &'static str> {
        self.instance.set_len(len)
    }

    pub(super) fn fallocate(&self, offset: usize, len: usize) -> Result<(), &'static str> {
        self.instance.fallocate(offset, len)
    }

    pub(super) fn io_ctl(&self, cmd: usize) -> Result<usize, &'static str> {
        self.instance.io_ctl_with_offset(cmd, self.offset())
    }

    // AGENT: lseek mutates only this regular-file handle's per-open offset.
    pub(super) fn seek(&self, pos: FSeek) -> Result<u64, &'static str> {
        let mut offset = self.offset.write().unwrap();
        let next = match pos {
            FSeek::Start(off) => off,
            FSeek::End(delta) => {
                let end = self.instance.len() as u64;
                end.checked_add_signed(delta).ok_or("einval")?
            }
            FSeek::Cur(delta) => offset.checked_add_signed(delta).ok_or("einval")?,
        };
        *offset = next;
        Ok(next)
    }

    // AGENT: read a regular-file chunk for splice and advance exactly the
    // selected shared or explicit position after the backing read succeeds.
    pub(super) fn splice_read(
        &self,
        status: FdOpt,
        pos: &mut SpliceFilePos,
        count: usize,
    ) -> Result<Vec<u8>, &'static str> {
        if !status.rd {
            return Err("ebadf");
        }
        if self.instance.node.kind != FileKind::Regular {
            return Err("einval");
        }
        match pos {
            SpliceFilePos::Shared => {
                let mut offset = self.offset.write().unwrap();
                Self::splice_read_at(&self.instance, &mut offset, count)
            }
            SpliceFilePos::Explicit(offset) => Self::splice_read_at(&self.instance, offset, count),
        }
    }

    // AGENT: keep checked regular-file splice reads independent of whether the
    // selected position is shared by an OFD or owned by the syscall arguments.
    fn splice_read_at(
        instance: &FInstance,
        offset: &mut u64,
        count: usize,
    ) -> Result<Vec<u8>, &'static str> {
        if *offset > i64::MAX as u64 {
            return Err("einval");
        }
        if count == 0 {
            return Ok(Vec::new());
        }
        let start = usize::try_from(*offset).map_err(|_| "einval")?;
        let chunk = instance.copy_chunk_at(start, count)?;
        let moved = u64::try_from(chunk.len()).map_err(|_| "efbig")?;
        let next = offset.checked_add(moved).ok_or("efbig")?;
        if next > i64::MAX as u64 {
            return Err("efbig");
        }
        *offset = next;
        Ok(chunk)
    }

    // AGENT: write a pipe prefix to a regular file and advance exactly the
    // selected position only after the complete backing write succeeds.
    pub(super) fn splice_write(
        &self,
        status: FdOpt,
        pos: &mut SpliceFilePos,
        bytes: &[u8],
    ) -> Result<usize, &'static str> {
        if !status.wr {
            return Err("ebadf");
        }
        if status.ap {
            return Err("einval");
        }
        if self.instance.node.kind != FileKind::Regular {
            return Err("einval");
        }
        match pos {
            SpliceFilePos::Shared => {
                let mut offset = self.offset.write().unwrap();
                Self::splice_write_at(&self.instance, &mut offset, bytes)
            }
            SpliceFilePos::Explicit(offset) => Self::splice_write_at(&self.instance, offset, bytes),
        }
    }

    // AGENT: keep the pipe unchanged until this checked backing write commits;
    // callers discard only the returned number of pipe bytes.
    fn splice_write_at(
        instance: &FInstance,
        offset: &mut u64,
        bytes: &[u8],
    ) -> Result<usize, &'static str> {
        if *offset > i64::MAX as u64 {
            return Err("einval");
        }
        if bytes.is_empty() {
            return Ok(0);
        }
        let start = usize::try_from(*offset).map_err(|_| "einval")?;
        let expected_end = start.checked_add(bytes.len()).ok_or("efbig")?;
        if expected_end > i64::MAX as usize {
            return Err("efbig");
        }
        let end = instance
            .node
            .write_bytes(instance.storage(), Some(start), bytes)?;
        if end != expected_end {
            return Err("eio");
        }
        *offset = u64::try_from(end).map_err(|_| "efbig")?;
        Ok(bytes.len())
    }
}

impl fmt::Debug for FHandle {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.debug_struct("FH")
            .field("instance", &self.instance)
            .field("offset", &self.offset())
            .finish()
    }
}
