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

// AGENT: keep broken-pipe notification distinct from errno so a large write
// can both report partial progress and request SIGPIPE generation at syscall ABI.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FdWriteOutcome {
    Written(usize),
    BrokenPipe { written: usize },
}

// AGENT: retain copied-in signed RV64 off_t values separately for each splice
// endpoint; None represents a null pointer and therefore an OFD-owned position
// for a regular file.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct SpliceOffsets {
    pub input: Option<i64>,
    pub output: Option<i64>,
}

// AGENT: convert one copied-in non-negative off_t into the position selector
// consumed by the regular-file layer.
fn splice_file_pos(offset: Option<i64>) -> Result<SpliceFilePos, &'static str> {
    match offset {
        Some(offset) => u64::try_from(offset)
            .map(SpliceFilePos::Explicit)
            .map_err(|_| "einval"),
        None => Ok(SpliceFilePos::Shared),
    }
}

// AGENT: copy an updated explicit position back into its syscall-owned slot
// while leaving null/shared offset arguments untouched.
fn update_splice_offset(slot: &mut Option<i64>, pos: &SpliceFilePos) -> Result<(), &'static str> {
    if slot.is_some() {
        let offset = pos.explicit().ok_or("eio")?;
        *slot = Some(i64::try_from(offset).map_err(|_| "efbig")?);
    }
    Ok(())
}

// AGENT: shared open-file description; dup/fork clone FdEntry while sharing
// this object, so status flags, file handles, and pipe endpoint lifetime remain shared.
pub struct OpenFileDesc {
    file: FLike,
    status: RwLock<FdOpt>,
    // AGENT: count only installed fd-table slots; transient Arc clones held by
    // syscall helpers and epoll registrations must not delay last-fd cleanup.
    fd_slots: AtomicUsize,
    // AGENT: keep weak reverse links so the last fd slot for this OFD can
    // remove registrations from every epoll instance, including across fork.
    epoll_watchers: Mutex<Vec<EpInstWeak>>,
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
            fd_slots: AtomicUsize::new(0),
            epoll_watchers: Mutex::new(Vec::new()),
        }
    }

    pub fn file(&self) -> &FLike {
        &self.file
    }

    // AGENT: keep object-type dispatch below the syscall layer so every fd stat
    // observes the same open-file-description target as read, write, and ioctl.
    pub fn file_attr(&self) -> Result<FileAttr, &'static str> {
        self.file.file_attr()
    }

    // AGENT: enforce open-file-description access flags before dispatching to
    // the concrete file-like object.
    pub fn read(&self, task_id: usize, buf: &mut [u8]) -> Result<usize, &'static str> {
        if buf.is_empty() {
            return Ok(0);
        }
        let status = self.status_flags();
        match &self.file {
            FLike::File(f) => f.read_with_status(status, buf),
            FLike::Pipe(p) => {
                if !status.rd {
                    return Err("ebadf");
                }
                p.read_at(task_id, status.nb, buf)
            }
            FLike::Ep(_) => Err("enosys"),
            FLike::Tty(tty) => {
                if !status.rd {
                    return Err("ebadf");
                }
                Ok(tty.read(buf))
            }
        }
    }

    // AGENT: keep append/write permission checks in the shared open-file
    // description instead of duplicating mutable status in FInstance.
    pub fn write(&self, task_id: usize, buf: &[u8]) -> Result<FdWriteOutcome, &'static str> {
        if buf.is_empty() {
            return Ok(FdWriteOutcome::Written(0));
        }
        let status = self.status_flags();
        match &self.file {
            FLike::File(f) => {
                if !status.wr {
                    return Err("ebadf");
                }
                f.write_with_status(status, buf)
                    .map(FdWriteOutcome::Written)
            }
            FLike::Pipe(p) => {
                if !status.wr {
                    return Err("ebadf");
                }
                match p.write_at(task_id, status.nb, buf)? {
                    PipeWriteOutcome::Written(n) => Ok(FdWriteOutcome::Written(n)),
                    PipeWriteOutcome::Broken { written } => {
                        Ok(FdWriteOutcome::BrokenPipe { written })
                    }
                }
            }
            FLike::Ep(_) => Err("enosys"),
            FLike::Tty(tty) => {
                if !status.wr {
                    return Err("ebadf");
                }
                Ok(FdWriteOutcome::Written(tty.write(buf)))
            }
        }
    }

    // AGENT: return explicit poll status so epoll can preserve closed peer state.
    pub fn poll(&self) -> PollStatus {
        match &self.file {
            FLike::Pipe(p) => p.poll(),
            FLike::Ep(e) => e.poll_status(),
            // AGENT: regular files and the first EOF-placeholder terminal have
            // no object-local readiness state, so derive readiness directly
            // from the shared open-file-description access mode.
            FLike::File(_) | FLike::Tty(_) => {
                let status = self.status_flags();
                PollStatus {
                    readable: status.rd,
                    writable: status.wr,
                    error: false,
                    closed: false,
                }
            }
        }
    }

    // AGENT: dispatch ioctl semantics by concrete fd object, keeping the first
    // typed terminal honest about its not-yet-migrated termios support.
    pub fn io_ctl(&self, req: usize) -> Result<usize, &'static str> {
        match &self.file {
            FLike::File(f) => f.io_ctl(req),
            FLike::Pipe(p) => match req {
                FIONREAD => Ok(p.readable_len()),
                _ => Err("enotty"),
            },
            FLike::Ep(_) => Err("enosys"),
            // AGENT: do not pretend that the minimal SBI terminal implements
            // termios ioctls before its input and line discipline are migrated.
            FLike::Tty(_) => Err("enotty"),
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

    // AGENT: let checkpoint code validate stdio by concrete object type instead
    // of assuming that fd numbers 0, 1, and 2 contain path-tagged regular files.
    pub fn is_tty(&self) -> bool {
        matches!(self.file, FLike::Tty(_))
    }

    // AGENT: reject non-null offsets on pipe endpoints before syscall usercopy,
    // matching Linux's ESPIPE precedence over dereferencing such pointers.
    pub fn validate_splice_offset_args(
        &self,
        dst: &OpenFileDesc,
        input_offset_present: bool,
        output_offset_present: bool,
    ) -> Result<(), &'static str> {
        if matches!(self.file, FLike::Pipe(_)) && input_offset_present {
            return Err("espipe");
        }
        if matches!(dst.file, FLike::Pipe(_)) && output_offset_present {
            return Err("espipe");
        }
        Ok(())
    }

    // AGENT: dispatch Linux splice semantics at the shared OFD boundary so
    // access flags, O_NONBLOCK, O_APPEND, and shared file offsets stay coherent.
    pub fn splice_to(
        &self,
        dst: &OpenFileDesc,
        task_id: usize,
        offsets: &mut SpliceOffsets,
        count: usize,
        flags: usize,
    ) -> Result<SpliceOutcome, &'static str> {
        self.validate_splice_offset_args(dst, offsets.input.is_some(), offsets.output.is_some())?;

        let src_status = self.status_flags();
        let dst_status = dst.status_flags();
        if !src_status.rd || !dst_status.wr {
            return Err("ebadf");
        }
        let requested_nonblock = flags & SPLICE_F_NONBLOCK != 0;

        match (&self.file, &dst.file) {
            (FLike::File(src), FLike::Pipe(output)) => {
                let mut pos = splice_file_pos(offsets.input)?;
                let result = output.splice_from_file(
                    task_id,
                    requested_nonblock || dst_status.nb,
                    src,
                    src_status,
                    &mut pos,
                    count,
                )?;
                update_splice_offset(&mut offsets.input, &pos)?;
                Ok(result)
            }
            (FLike::Pipe(input), FLike::File(dst_file)) => {
                if dst_status.ap {
                    return Err("einval");
                }
                let mut pos = splice_file_pos(offsets.output)?;
                let result = input.splice_to_file(
                    task_id,
                    requested_nonblock || src_status.nb,
                    dst_file,
                    dst_status,
                    &mut pos,
                    count,
                )?;
                update_splice_offset(&mut offsets.output, &pos)?;
                Ok(result)
            }
            (FLike::Pipe(input), FLike::Pipe(output)) => input.splice_to_pipe(
                output,
                task_id,
                requested_nonblock || src_status.nb || dst_status.nb,
                count,
            ),
            _ => Err("einval"),
        }
    }
}

// AGENT: own one open-file-description Arc while comparing and ordering it by
// allocation identity, matching Linux's (fd number, open file description)
// epoll key without exposing a stale integer-only id.
#[derive(Clone)]
pub(crate) struct OpenFileRef(Arc<OpenFileDesc>);

// AGENT: preserve pointer identity for equality even when OFD status changes.
impl PartialEq for OpenFileRef {
    // AGENT: compare OFD allocations rather than mutable file/status contents.
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
}

// AGENT: complete identity equality for Arc-backed OFD keys.
impl Eq for OpenFileRef {}

// AGENT: give BTreeMap a stable identity order while both Arc-backed
// allocations remain alive through the keys being compared.
impl PartialOrd for OpenFileRef {
    // AGENT: delegate partial ordering to the total pointer-identity order.
    fn partial_cmp(&self, other: &Self) -> Option<CmpOrd> {
        Some(self.cmp(other))
    }
}

// AGENT: order live Arc allocations by address for BTreeMap key placement.
impl Ord for OpenFileRef {
    // AGENT: keep ordering stable because both compared Arcs retain allocations.
    fn cmp(&self, other: &Self) -> CmpOrd {
        let lhs = Arc::as_ptr(&self.0) as usize;
        let rhs = Arc::as_ptr(&other.0) as usize;
        lhs.cmp(&rhs)
    }
}

// AGENT: keep weak source identity in readiness callbacks so a Pipe EvBus does
// not retain its own OpenFileDesc through an EpKey reference cycle.
#[derive(Clone)]
pub(crate) struct OpenFileWeak(Weak<OpenFileDesc>);

// AGENT: centralize OFD identity, readiness, subscription, and fd-slot lifetime
// operations behind the Arc wrapper used by epoll.
impl OpenFileRef {
    // AGENT: create a non-owning source reference for readiness callbacks.
    pub(crate) fn downgrade(&self) -> OpenFileWeak {
        OpenFileWeak(Arc::downgrade(&self.0))
    }

    // AGENT: poll the exact registered OFD even after its fd number is reused.
    pub(crate) fn poll(&self) -> PollStatus {
        self.0.poll()
    }

    // AGENT: install a source callback keyed by the full (fd, OFD) identity.
    pub(crate) fn register_epoll_source(
        &self,
        key: &EpKey,
        ep: &EpInst,
        ev: &EpEvent,
    ) -> Option<usize> {
        self.0.file().register_epoll(key, ep, ev)
    }

    // AGENT: detach one callback from the concrete source object.
    pub(crate) fn unregister_epoll_source(&self, sub_id: usize) -> bool {
        self.0.file().unregister_epoll(sub_id)
    }

    // AGENT: expose a shared epoll object only when this OFD contains one.
    pub(crate) fn epoll_instance(&self) -> Option<EpInst> {
        match self.0.file() {
            FLike::Ep(inst) => Some(inst.clone()),
            _ => None,
        }
    }

    // AGENT: record one newly installed fd-table slot independently of Arc refs.
    pub(crate) fn acquire_fd_slot(&self) {
        self.0.fd_slots.fetch_add(1, Ordering::Relaxed);
    }

    // AGENT: report whether a removed slot was the last global descriptor slot.
    pub(crate) fn release_fd_slot(&self) -> bool {
        let previous = self.0.fd_slots.fetch_sub(1, Ordering::AcqRel);
        debug_assert!(previous > 0);
        previous == 1
    }

    // AGENT: remember one epoll registration through a weak reverse link.
    pub(crate) fn add_epoll_watcher(&self, epoll: &EpInst) {
        self.0
            .epoll_watchers
            .lock()
            .unwrap()
            .push(epoll.downgrade());
    }

    // AGENT: remove exactly one reverse link when one registration is deleted.
    pub(crate) fn remove_epoll_watcher(&self, epoll: &EpInst) {
        let mut watchers = self.0.epoll_watchers.lock().unwrap();
        if let Some(index) = watchers
            .iter()
            .position(|watcher| watcher.same_instance(epoll))
        {
            watchers.swap_remove(index);
        }
    }

    // AGENT: remove watched-source registrations globally and drain epoll-owned
    // subscriptions only when the final real fd-table slot disappears.
    pub(crate) fn close_last_fd_slot(&self) {
        let watchers = mem::take(&mut *self.0.epoll_watchers.lock().unwrap());
        for watcher in watchers {
            if let Some(epoll) = watcher.upgrade() {
                epoll.remove_source_on_last_close(self);
            }
        }
        if let Some(epoll) = self.epoll_instance() {
            epoll.close_last_fd_slot();
        }
    }
}

// AGENT: upgrade callback-held weak OFD identity only while delivering a live
// readiness notification.
impl OpenFileWeak {
    // AGENT: regain a short-lived OFD owner only while a callback is delivered.
    pub(crate) fn upgrade(&self) -> Option<OpenFileRef> {
        self.0.upgrade().map(OpenFileRef)
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

    // AGENT: expose an Arc-owning OFD identity without copying per-fd flags.
    pub(crate) fn open_file_ref(&self) -> OpenFileRef {
        OpenFileRef(self.desc.clone())
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
        self.open_file_ref().epoll_instance()
    }

    // AGENT: compare open-file-description identity so close can distinguish the
    // last fd-table reference from temporary cloned FdEntry handles.
    pub fn same_open_description(&self, other: &FdEntry) -> bool {
        self.open_file_ref() == other.open_file_ref()
    }

    // AGENT: install a source-backed epoll subscription through the descriptor
    // entry so callers do not need a compatibility FLike clone.
    pub fn register_epoll_source(&self, key: &EpKey, ep: &EpInst, ev: &EpEvent) -> Option<usize> {
        self.open_file_ref().register_epoll_source(key, ep, ev)
    }

    // AGENT: remove a source-backed epoll subscription from this file object.
    pub fn unregister_epoll_source(&self, sub_id: usize) -> bool {
        self.desc.file().unregister_epoll(sub_id)
    }

    // AGENT: carry the calling task identity down to potentially blocking file
    // implementations without making FdEntry depend on scheduler globals.
    pub fn read(&self, task_id: usize, buf: &mut [u8]) -> Result<usize, &'static str> {
        self.desc.read(task_id, buf)
    }

    // AGENT: preserve typed broken-pipe progress until syscall glue can pair
    // the return value with process signal generation.
    pub fn write(&self, task_id: usize, buf: &[u8]) -> Result<FdWriteOutcome, &'static str> {
        self.desc.write(task_id, buf)
    }

    // AGENT: forward stat through the shared open-file description without
    // treating descriptor flags or the current file offset as inode metadata.
    pub fn file_attr(&self) -> Result<FileAttr, &'static str> {
        self.desc.file_attr()
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

    // AGENT: expose terminal classification without leaking the polymorphic
    // open-file-description internals into task fd-table code.
    pub fn is_tty(&self) -> bool {
        self.desc.is_tty()
    }

    // AGENT: expose pipe-offset validation without leaking concrete FLike
    // objects from the shared open-file description.
    pub fn validate_splice_offset_args(
        &self,
        dst: &FdEntry,
        input_offset_present: bool,
        output_offset_present: bool,
    ) -> Result<(), &'static str> {
        self.desc.validate_splice_offset_args(
            dst.desc.as_ref(),
            input_offset_present,
            output_offset_present,
        )
    }

    // AGENT: carry the calling task and copied offset state through the
    // authoritative shared open-file descriptions for both endpoints.
    pub fn splice_to(
        &self,
        dst: &FdEntry,
        task_id: usize,
        offsets: &mut SpliceOffsets,
        count: usize,
        flags: usize,
    ) -> Result<SpliceOutcome, &'static str> {
        self.desc
            .splice_to(dst.desc.as_ref(), task_id, offsets, count, flags)
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
