// AGENT
use super::*;

#[derive(Debug)]
pub enum FSeek {
    Start(u64),
    End(i64),
    Cur(i64),
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

    // AGENT: copy regular-file bytes and commit handle offsets only after the
    // destination write succeeds.
    pub(super) fn splice_to(
        &self,
        dst: &FHandle,
        src_status: FdOpt,
        dst_status: FdOpt,
        count: usize,
    ) -> Result<usize, &'static str> {
        if ptr::eq(self, dst) {
            let mut offset = self.offset.write().unwrap();
            return Self::splice_same_locked(
                &self.instance,
                &dst.instance,
                &mut offset,
                src_status,
                dst_status,
                count,
            );
        }

        let self_key = self as *const FHandle as usize;
        let dst_key = dst as *const FHandle as usize;
        if self_key < dst_key {
            let mut src_offset = self.offset.write().unwrap();
            let mut dst_offset = dst.offset.write().unwrap();
            Self::splice_locked(
                &self.instance,
                &mut src_offset,
                &dst.instance,
                &mut dst_offset,
                src_status,
                dst_status,
                count,
            )
        } else {
            let mut dst_offset = dst.offset.write().unwrap();
            let mut src_offset = self.offset.write().unwrap();
            Self::splice_locked(
                &self.instance,
                &mut src_offset,
                &dst.instance,
                &mut dst_offset,
                src_status,
                dst_status,
                count,
            )
        }
    }

    // AGENT: splice through one shared offset without reintroducing an inode
    // accessor that duplicates FInstance's public object identity.
    fn splice_same_locked(
        src: &FInstance,
        dst: &FInstance,
        offset: &mut u64,
        src_status: FdOpt,
        dst_status: FdOpt,
        count: usize,
    ) -> Result<usize, &'static str> {
        if !src_status.rd || !dst_status.wr {
            return Err("ebadf");
        }
        if src.node.kind != FileKind::Regular || dst.node.kind != FileKind::Regular {
            return Err("enodev");
        }
        if count == 0 {
            return Ok(0);
        }
        let src_off = match usize::try_from(*offset) {
            Ok(off) => off,
            Err(_) => return Ok(0),
        };
        let chunk = src.copy_chunk_at(src_off, count)?;
        if chunk.is_empty() {
            return Ok(0);
        }
        let write_off = if dst_status.ap {
            None
        } else {
            Some(src_off.checked_add(chunk.len()).ok_or("efbig")?)
        };
        let end = dst.node.write_bytes(dst.storage(), write_off, &chunk)?;
        *offset = u64::try_from(end).map_err(|_| "efbig")?;
        Ok(chunk.len())
    }

    // AGENT: splice between distinct offsets using each FInstance's public node
    // and mount-derived storage backend.
    fn splice_locked(
        src: &FInstance,
        src_offset: &mut u64,
        dst: &FInstance,
        dst_offset: &mut u64,
        src_status: FdOpt,
        dst_status: FdOpt,
        count: usize,
    ) -> Result<usize, &'static str> {
        if !src_status.rd || !dst_status.wr {
            return Err("ebadf");
        }
        if src.node.kind != FileKind::Regular || dst.node.kind != FileKind::Regular {
            return Err("enodev");
        }
        if count == 0 {
            return Ok(0);
        }
        let src_off = match usize::try_from(*src_offset) {
            Ok(off) => off,
            Err(_) => return Ok(0),
        };
        let chunk = src.copy_chunk_at(src_off, count)?;
        if chunk.is_empty() {
            return Ok(0);
        }
        let write_off = if dst_status.ap {
            None
        } else {
            Some(usize::try_from(*dst_offset).map_err(|_| "efbig")?)
        };
        let end = dst.node.write_bytes(dst.storage(), write_off, &chunk)?;
        let moved = u64::try_from(chunk.len()).map_err(|_| "efbig")?;
        *src_offset = src_offset.checked_add(moved).ok_or("efbig")?;
        *dst_offset = u64::try_from(end).map_err(|_| "efbig")?;
        Ok(chunk.len())
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
