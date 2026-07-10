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

// AGENT: shared state produced by one open. dup/fork share this through
// OpenFileDesc instead of storing fd semantics on FInstance.
struct OpenFileState {
    offset: u64,
    status: FdOpt,
}

// AGENT: shared open-file description; dup/fork clone FdEntry while sharing
// this object, so offset/status state and pipe endpoint lifetime remain shared.
pub struct OpenFileDesc {
    file: FLike,
    state: RwLock<OpenFileState>,
}

impl OpenFileDesc {
    // AGENT: build an open-file description around a concrete file object.
    pub fn new(file: FLike) -> Self {
        let status = file.status_flags();
        Self::new_with_status(file, status)
    }

    // AGENT: build an open-file description with explicit open-time access and
    // initial status flags, used when FInstance carries only the file object.
    pub fn new_with_status(file: FLike, status: FdOpt) -> Self {
        Self {
            file,
            state: RwLock::new(OpenFileState { offset: 0, status }),
        }
    }

    pub fn file(&self) -> &FLike {
        &self.file
    }

    // AGENT: enforce open-file-description access flags before dispatching to
    // the concrete file-like object.
    pub fn read(&self, buf: &mut [u8]) -> Result<usize, &'static str> {
        if buf.is_empty() {
            return Ok(0);
        }
        match &self.file {
            FLike::File(f) => {
                let mut state = self.state.write().unwrap();
                f.read_with_state(state.status, &mut state.offset, buf)
            }
            FLike::Pipe(p) => {
                if !self.status_flags().rd {
                    return Err("ebadf");
                }
                p.read_at(buf)
            }
            FLike::Ep(_) => Err("enosys"),
        }
    }

    // AGENT: keep append/write permission checks in the shared open-file
    // description instead of duplicating mutable status in FInstance.
    pub fn write(&self, buf: &[u8]) -> Result<usize, &'static str> {
        if buf.is_empty() {
            return Ok(0);
        }
        match &self.file {
            FLike::File(f) => {
                let mut state = self.state.write().unwrap();
                f.write_with_state(state.status, &mut state.offset, buf)
            }
            FLike::Pipe(p) => {
                if !self.status_flags().wr {
                    return Err("ebadf");
                }
                p.write_at(buf)
            }
            FLike::Ep(_) => Err("enosys"),
        }
    }

    // AGENT: return explicit poll status so epoll can preserve closed peer state.
    pub fn poll(&self) -> PollStatus {
        match &self.file {
            FLike::File(f) => f.poll_status_with_status(self.status_flags()),
            FLike::Pipe(p) => p.poll(),
            FLike::Ep(e) => e.poll_status(),
        }
    }

    pub fn io_ctl(&self, req: usize, arg: usize) -> Result<usize, &'static str> {
        match &self.file {
            FLike::File(f) => {
                let state = self.state.read().unwrap();
                f.io_ctl_with_offset(req, arg, state.offset)
            }
            FLike::Pipe(p) => match req {
                FIONREAD => Ok(p.readable_len()),
                _ => Err("enotty"),
            },
            FLike::Ep(_) => Err("enosys"),
        }
    }

    // AGENT: directory iteration uses the shared open-file-description offset
    // so dup/fork observe the same stream position.
    pub fn read_entry(&self) -> Result<String, &'static str> {
        match &self.file {
            FLike::File(f) => {
                let mut state = self.state.write().unwrap();
                if !state.status.rd {
                    return Err("ebadf");
                }
                let idx = usize::try_from(state.offset).map_err(|_| "eoverflow")?;
                let entry = f.read_entry(idx)?;
                state.offset = state.offset.checked_add(1).ok_or("eoverflow")?;
                Ok(entry)
            }
            _ => Err("enotdir"),
        }
    }

    // AGENT: fd-level truncation checks the open-description write permission;
    // FInstance only performs the backing file mutation.
    pub fn set_len(&self, len: u64) -> Result<(), &'static str> {
        let status = self.status_flags();
        if !status.wr {
            return Err("ebadf");
        }
        match &self.file {
            FLike::File(f) => f.set_len(len),
            _ => Err("enodev"),
        }
    }

    // AGENT: allocation is a regular-file operation gated by the shared open
    // access mode, not by state stored on FInstance.
    pub fn fallocate(&self, offset: usize, len: usize) -> Result<(), &'static str> {
        let status = self.status_flags();
        if !status.wr {
            return Err("ebadf");
        }
        match &self.file {
            FLike::File(f) => f.fallocate(offset, len),
            _ => Err("enodev"),
        }
    }

    pub fn status_flags(&self) -> FdOpt {
        self.state.read().unwrap().status
    }

    pub fn offset(&self) -> u64 {
        self.state.read().unwrap().offset
    }

    // AGENT: lseek mutates the shared open-file-description offset. The backing
    // FInstance only supplies regular-file length for SEEK_END.
    pub fn seek(&self, pos: FSeek) -> Result<u64, &'static str> {
        let FLike::File(file) = &self.file else {
            return Err("espipe");
        };
        let mut state = self.state.write().unwrap();
        let next = match pos {
            FSeek::Start(off) => off,
            FSeek::End(delta) => {
                let end = file.len() as u64;
                end.checked_add_signed(delta).ok_or("einval")?
            }
            FSeek::Cur(delta) => state.offset.checked_add_signed(delta).ok_or("einval")?,
        };
        state.offset = next;
        Ok(next)
    }

    // AGENT: expose status flags in the same bit shape returned by F_GETFL so
    // checkpoint code does not duplicate fd option encoding.
    pub fn status_flags_bits(&self) -> usize {
        fdopt_to_open_flags(self.status_flags())
    }

    // AGENT: update only the mutable open-file status bits; access mode remains
    // fixed from open/pipe creation.
    pub fn set_status_flags(&self, flags: usize) -> Result<(), &'static str> {
        let mut state = self.state.write().unwrap();
        state.status.apply_status_flags(flags);
        Ok(())
    }

    pub fn regular_instance(&self) -> Option<FInstance> {
        match &self.file {
            FLike::File(f) => Some(f.clone()),
            _ => None,
        }
    }

    // AGENT: keep transfer-shaped callers on the shared open-file description
    // path so fd status changes such as O_APPEND remain visible.
    pub fn transfer(
        &self,
        dir: u8,
        offset: Option<usize>,
        buf_rd: Option<&mut [u8]>,
        buf_wr: Option<&[u8]>,
    ) -> Result<usize, &'static str> {
        match &self.file {
            FLike::File(f) => {
                let mut state = self.state.write().unwrap();
                f.transfer_with_state(state.status, &mut state.offset, dir, offset, buf_rd, buf_wr)
            }
            _ => match (dir, offset, buf_rd, buf_wr) {
                (FInstance::TRANSFER_READ, None, Some(buf), None) => self.read(buf),
                (FInstance::TRANSFER_WRITE, None, None, Some(buf)) => self.write(buf),
                (FInstance::TRANSFER_READ | FInstance::TRANSFER_WRITE, Some(_), _, _) => {
                    Err("espipe")
                }
                _ => Err("einval"),
            },
        }
    }

    // AGENT: keep splice on the shared open-file-description path so mutable
    // status flags such as O_APPEND are honored for both ends.
    pub fn splice_to(&self, dst: &OpenFileDesc, count: usize) -> Result<usize, &'static str> {
        match (&self.file, &dst.file) {
            (FLike::File(src), FLike::File(dst_file)) => {
                if ::core::ptr::eq(self, dst) {
                    let mut state = self.state.write().unwrap();
                    Self::splice_same_description(src, dst_file, &mut state, count)
                } else {
                    let self_key = self as *const OpenFileDesc as usize;
                    let dst_key = dst as *const OpenFileDesc as usize;
                    if self_key < dst_key {
                        let mut src_state = self.state.write().unwrap();
                        let mut dst_state = dst.state.write().unwrap();
                        Self::splice_locked(src, &mut src_state, dst_file, &mut dst_state, count)
                    } else {
                        let mut dst_state = dst.state.write().unwrap();
                        let mut src_state = self.state.write().unwrap();
                        Self::splice_locked(src, &mut src_state, dst_file, &mut dst_state, count)
                    }
                }
            }
            _ => Err("enosys"),
        }
    }

    // AGENT: self-splice observes one shared open-file-description offset instead
    // of trying to borrow the same state lock twice.
    fn splice_same_description(
        src: &FInstance,
        dst: &FInstance,
        state: &mut OpenFileState,
        count: usize,
    ) -> Result<usize, &'static str> {
        if !state.status.rd || !state.status.wr {
            return Err("ebadf");
        }
        if src.node.kind != FileKind::Regular || dst.node.kind != FileKind::Regular {
            return Err("enodev");
        }
        if count == 0 {
            return Ok(0);
        }
        let src_off = match usize::try_from(state.offset) {
            Ok(off) => off,
            Err(_) => return Ok(0),
        };
        let chunk = src.copy_chunk_at(src_off, count)?;
        if chunk.is_empty() {
            return Ok(0);
        }
        let write_off = if state.status.ap {
            None
        } else {
            Some(src_off.checked_add(chunk.len()).ok_or("efbig")?)
        };
        let end = dst.node.write_bytes(&dst.storage, write_off, &chunk)?;
        state.offset = u64::try_from(end).map_err(|_| "efbig")?;
        Ok(chunk.len())
    }

    // AGENT: copy regular-file bytes and commit both open-description offsets only
    // after the destination write succeeds.
    fn splice_locked(
        src: &FInstance,
        src_state: &mut OpenFileState,
        dst: &FInstance,
        dst_state: &mut OpenFileState,
        count: usize,
    ) -> Result<usize, &'static str> {
        if !src_state.status.rd || !dst_state.status.wr {
            return Err("ebadf");
        }
        if src.node.kind != FileKind::Regular || dst.node.kind != FileKind::Regular {
            return Err("enodev");
        }
        if count == 0 {
            return Ok(0);
        }
        let src_off = match usize::try_from(src_state.offset) {
            Ok(off) => off,
            Err(_) => return Ok(0),
        };
        let chunk = src.copy_chunk_at(src_off, count)?;
        if chunk.is_empty() {
            return Ok(0);
        }
        let write_off = if dst_state.status.ap {
            None
        } else {
            Some(usize::try_from(dst_state.offset).map_err(|_| "efbig")?)
        };
        let end = dst.node.write_bytes(&dst.storage, write_off, &chunk)?;
        let moved = u64::try_from(chunk.len()).map_err(|_| "efbig")?;
        src_state.offset = src_state.offset.checked_add(moved).ok_or("efbig")?;
        dst_state.offset = u64::try_from(end).map_err(|_| "efbig")?;
        Ok(chunk.len())
    }
}

// AGENT: fd flags that belong to one descriptor entry, not to the shared open
// file description.
#[derive(Clone)]
pub struct FdEntry {
    desc: Arc<OpenFileDesc>,
    cloexec: bool,
}

impl FdEntry {
    // AGENT: create a descriptor entry over a fresh open-file description.
    pub fn new(file: FLike) -> Self {
        Self::with_cloexec(file, false)
    }

    // AGENT: create a descriptor entry with per-fd close-on-exec state.
    pub fn with_cloexec(file: FLike, cloexec: bool) -> Self {
        Self {
            desc: Arc::new(OpenFileDesc::new(file)),
            cloexec,
        }
    }

    // AGENT: create a descriptor entry with explicit open-file-description state
    // for regular files whose FInstance no longer stores fd status.
    pub fn with_status(file: FLike, status: FdOpt, cloexec: bool) -> Self {
        Self {
            desc: Arc::new(OpenFileDesc::new_with_status(file, status)),
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

    // AGENT: expose directory iteration at the descriptor-entry layer while
    // keeping the shared offset inside OpenFileDesc.
    pub fn read_entry(&self) -> Result<String, &'static str> {
        self.desc.read_entry()
    }

    pub fn set_len(&self, len: u64) -> Result<(), &'static str> {
        self.desc.set_len(len)
    }

    pub fn fallocate(&self, offset: usize, len: usize) -> Result<(), &'static str> {
        self.desc.fallocate(offset, len)
    }

    pub fn status_flags(&self) -> FdOpt {
        self.desc.status_flags()
    }

    pub fn offset(&self) -> u64 {
        self.desc.offset()
    }

    pub fn seek(&self, pos: FSeek) -> Result<u64, &'static str> {
        self.desc.seek(pos)
    }

    // AGENT: expose serialized status flags for checkpoint fd table snapshots.
    pub fn status_flags_bits(&self) -> usize {
        self.desc.status_flags_bits()
    }

    pub fn set_status_flags(&self, flags: usize) -> Result<(), &'static str> {
        self.desc.set_status_flags(flags)
    }

    pub fn regular_instance(&self) -> Option<FInstance> {
        self.desc.regular_instance()
    }

    // AGENT: expose the transfer helper at the fd-entry layer so callers do not
    // bypass shared open-file-description status.
    pub fn transfer(
        &self,
        dir: u8,
        offset: Option<usize>,
        buf_rd: Option<&mut [u8]>,
        buf_wr: Option<&[u8]>,
    ) -> Result<usize, &'static str> {
        self.desc.transfer(dir, offset, buf_rd, buf_wr)
    }

    // AGENT: fd-table callers splice through descriptor entries rather than
    // raw instances, preserving shared status and offset semantics.
    pub fn splice_to(&self, dst: &FdEntry, count: usize) -> Result<usize, &'static str> {
        self.desc.splice_to(dst.desc.as_ref(), count)
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

// AGENT: keep fd-focused regressions in a separate source file while preserving
// the existing crate::kernel::fs::fd::tests::run_all() selftest entry.
#[cfg(any(test, feature = "qemu-sync-selftest"))]
#[path = "fd_tests.rs"]
pub(crate) mod tests;
