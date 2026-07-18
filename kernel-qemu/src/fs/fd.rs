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

// AGENT: shared open-file description; dup/fork clone FdEntry while sharing
// this object, so status flags, file handles, and pipe endpoint lifetime remain shared.
pub struct OpenFileDesc {
    file: FLike,
    status: RwLock<FdOpt>,
}

impl OpenFileDesc {
    // AGENT: build an open-file description around a concrete file object.
    pub fn new(file: FLike) -> Self {
        let status = file.status_flags();
        Self::new_with_status(file, status)
    }

    // AGENT: build an open-file description with explicit open-time access and
    // initial status flags.
    pub fn new_with_status(file: FLike, status: FdOpt) -> Self {
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
        if buf.is_empty() {
            return Ok(0);
        }
        match &self.file {
            FLike::File(f) => {
                let status = self.status_flags();
                f.read_with_status(status, buf)
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
                let status = self.status_flags();
                f.write_with_status(status, buf)
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

    pub fn io_ctl(&self, req: usize) -> Result<usize, &'static str> {
        match &self.file {
            FLike::File(f) => f.io_ctl(req),
            FLike::Pipe(p) => match req {
                FIONREAD => Ok(p.readable_len()),
                _ => Err("enotty"),
            },
            FLike::Ep(_) => Err("enosys"),
        }
    }

    // AGENT: directory iteration uses the shared FHandle offset so dup/fork
    // observe the same stream position.
    pub fn read_entry(&self) -> Result<String, &'static str> {
        match &self.file {
            FLike::File(f) => {
                let status = self.status_flags();
                f.read_entry_with_status(status)
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
        *self.status.read().unwrap()
    }

    pub fn offset(&self) -> u64 {
        match &self.file {
            FLike::File(f) => f.offset(),
            _ => 0,
        }
    }

    // AGENT: lseek mutates the shared regular-file handle offset.
    pub fn seek(&self, pos: FSeek) -> Result<u64, &'static str> {
        let FLike::File(file) = &self.file else {
            return Err("espipe");
        };
        file.seek(pos)
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

    // AGENT: checkpoint only needs to reject non-regular fd objects here; avoid
    // cloning the backing file instance when the offset/status live elsewhere.
    pub fn is_regular_file(&self) -> bool {
        matches!(self.file, FLike::File(_))
    }

    // AGENT: keep splice on the shared open-file-description path so mutable
    // status flags such as O_APPEND are honored for both ends.
    pub fn splice_to(&self, dst: &OpenFileDesc, count: usize) -> Result<usize, &'static str> {
        match (&self.file, &dst.file) {
            (FLike::File(src), FLike::File(dst_file)) => {
                let src_status = self.status_flags();
                let dst_status = dst.status_flags();
                src.splice_to(dst_file, src_status, dst_status, count)
            }
            _ => Err("enosys"),
        }
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

    // AGENT: create a descriptor entry with explicit open-file-description
    // status; regular-file offsets live in FHandle.
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

    pub fn io_ctl(&self, req: usize) -> Result<usize, &'static str> {
        self.desc.io_ctl(req)
    }

    // AGENT: expose directory iteration at the descriptor-entry layer while
    // keeping the shared offset inside FHandle.
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

    // AGENT: expose regular-file object classification without cloning the
    // underlying FInstance or implying fd-dup semantics.
    pub fn is_regular_file(&self) -> bool {
        self.desc.is_regular_file()
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
