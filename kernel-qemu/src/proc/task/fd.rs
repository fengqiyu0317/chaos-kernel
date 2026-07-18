// AGENT: isolate descriptor allocation, duplication, close cleanup, and
// checkpoint-fd translation from task lifecycle state.
use super::*;

// AGENT: keep descriptor entries and their allocation index behind one lock so
// callers cannot update one side without preserving the other.
pub(crate) struct FdTable {
    entries: BTreeMap<usize, FdEntry>,
    allocator: AllocatorState,
}

// AGENT: initialize an empty descriptor table with every bounded fd available.
impl Default for FdTable {
    fn default() -> Self {
        Self {
            entries: BTreeMap::new(),
            allocator: AllocatorState::new(0, MAX_FD),
        }
    }
}

// AGENT: centralize fd allocation and snapshot invariants inside FdTable.
impl FdTable {
    // AGENT: rebuild the generic id allocator from validated occupied fd slots.
    fn from_entries(entries: BTreeMap<usize, FdEntry>) -> Result<Self, &'static str> {
        let mut allocator = AllocatorState::new(0, MAX_FD);
        for fd in entries.keys() {
            allocator.reserve(*fd).ok_or("ebadf")?;
        }
        Ok(Self { entries, allocator })
    }

    // AGENT: expose a non-mutating lower-bound lookup for fd compatibility APIs.
    fn get_free_from(&self, start: usize) -> Option<usize> {
        self.allocator.peek_from(start)
    }

    // AGENT: allocate the lowest descriptor at or above an ABI lower bound.
    fn reserve_from(&mut self, start: usize) -> Result<usize, &'static str> {
        self.allocator.allocate_from(start).ok_or("emfile")
    }

    // AGENT: duplicate entries while preserving an identical independent id
    // allocator snapshot for the child process.
    fn fork_copy(&self) -> Self {
        Self {
            entries: self
                .entries
                .iter()
                .map(|(&fd, entry)| (fd, entry.fork_dup()))
                .collect(),
            allocator: self.allocator.clone(),
        }
    }

    fn cloexec_fds(&self) -> Vec<usize> {
        self.entries
            .iter()
            .filter_map(|(&fd, entry)| entry.is_cloexec().then_some(fd))
            .collect()
    }
}

// AGENT: collect close side effects under the fd-table lock and execute them
// after unlocking so callbacks and Drop cannot re-enter the table.
struct FdCloseCleanup {
    closed_entry: FdEntry,
    closed_fd_source_subs: Vec<usize>,
    epoll_source_subs: Vec<(FdEntry, usize)>,
}

// AGENT: detach all epoll source callbacks before dropping a closed entry.
impl FdCloseCleanup {
    // AGENT: run deferred unsubscriptions outside the fd-table lock.
    fn run(self) {
        for sub_id in self.closed_fd_source_subs {
            self.closed_entry.unregister_epoll_source(sub_id);
        }
        for (source, sub_id) in self.epoll_source_subs {
            source.unregister_epoll_source(sub_id);
        }
        drop(self.closed_entry);
    }
}

// AGENT: restrict the first checkpoint fd surface to supported stdio entries.
fn checkpoint_fd_kind(fd: usize) -> Result<SavedFdKind, &'static str> {
    match fd {
        0 => Ok(SavedFdKind::Stdin),
        1 => Ok(SavedFdKind::Stdout),
        2 => Ok(SavedFdKind::Stderr),
        _ => Err("enotsup"),
    }
}

// AGENT: assign stable image ids to shared open-file descriptions by Arc identity.
fn checkpoint_description_id(
    entry: &FdEntry,
    descriptions: &mut Vec<(FdEntry, u32)>,
) -> Result<u32, &'static str> {
    for (known, id) in descriptions.iter() {
        if entry.same_open_description(known) {
            return Ok(*id);
        }
    }
    let id = u32::try_from(descriptions.len() + 1).map_err(|_| "einval")?;
    descriptions.push((entry.clone(), id));
    Ok(id)
}

// AGENT: rebuild the stdio-like instances supported by checkpoint restore.
fn checkpoint_stdio_instance(
    kind: SavedFdKind,
    status_flags: u32,
) -> Result<(FInstance, FdOpt), &'static str> {
    let mut opt = match kind {
        SavedFdKind::Stdin => FdOpt {
            rd: true,
            wr: false,
            ap: false,
            nb: false,
        },
        SavedFdKind::Stdout | SavedFdKind::Stderr => FdOpt {
            rd: false,
            wr: true,
            ap: false,
            nb: false,
        },
        SavedFdKind::RegularMemoryFile
        | SavedFdKind::Pipe
        | SavedFdKind::Epoll
        | SavedFdKind::Socket
        | SavedFdKind::Tty => return Err("enotsup"),
    };
    opt.apply_status_flags(status_flags as usize);
    let path = match kind {
        SavedFdKind::Stdin => "/dev/stdin",
        SavedFdKind::Stdout => "/dev/stdout",
        SavedFdKind::Stderr => "/dev/stderr",
        _ => return Err("enotsup"),
    };
    Ok((FInstance::new(path), opt))
}

// AGENT: seed stdio through the unified allocator used by later open, dup, and
// close operations.
pub(super) fn install_initial_stdio(task: &Arc<Task>) -> Result<(), &'static str> {
    let stdin_opt = FdOpt {
        rd: true,
        wr: false,
        ap: false,
        nb: false,
    };
    let stdout_opt = FdOpt {
        rd: false,
        wr: true,
        ap: false,
        nb: false,
    };
    let stdin_instance = FInstance::new("/dev/tty");
    let stdout_instance = FInstance::new("/dev/tty");
    let stderr_instance = stdout_instance.dup();
    let stdin = task.add_file_with_status(FLike::File(FHandle::new(stdin_instance)), stdin_opt)?;
    let stdout =
        task.add_file_with_status(FLike::File(FHandle::new(stdout_instance)), stdout_opt)?;
    let stderr =
        task.add_file_with_status(FLike::File(FHandle::new(stderr_instance)), stdout_opt)?;
    if (stdin, stdout, stderr) != (0, 1, 2) {
        return Err("ebadf");
    }
    Ok(())
}

// AGENT: implement the complete Task fd-table surface in the descriptor module.
impl Task {
    // AGENT: peek at the lowest free descriptor without scanning occupied slots.
    pub fn get_free_fd(&self) -> Option<usize> {
        self.get_free_fd_from(0)
    }

    // AGENT: find a free descriptor at or above an F_DUPFD-style lower bound.
    pub fn get_free_fd_from(&self, start: usize) -> Option<usize> {
        self.process.fd_table.lock().unwrap().get_free_from(start)
    }

    // AGENT: install a new entry with a fresh shared open-file description.
    pub fn add_file(&self, fl: FLike) -> Result<usize, &'static str> {
        self.add_file_with_cloexec(fl, false)
    }

    // AGENT: install an entry with explicit open-file-description status.
    pub fn add_file_with_status(&self, fl: FLike, status: FdOpt) -> Result<usize, &'static str> {
        self.add_file_with_cloexec_and_status(fl, status, false)
    }

    // AGENT: install an entry and record per-descriptor close-on-exec state.
    pub fn add_file_with_cloexec(&self, fl: FLike, cloexec: bool) -> Result<usize, &'static str> {
        let mut table = self.process.fd_table.lock().unwrap();
        let fd = table.reserve_from(0)?;
        table.entries.insert(fd, FdEntry::with_cloexec(fl, cloexec));
        Ok(fd)
    }

    // AGENT: install an entry with explicit status and close-on-exec state.
    pub fn add_file_with_cloexec_and_status(
        &self,
        fl: FLike,
        status: FdOpt,
        cloexec: bool,
    ) -> Result<usize, &'static str> {
        let mut table = self.process.fd_table.lock().unwrap();
        let fd = table.reserve_from(0)?;
        table
            .entries
            .insert(fd, FdEntry::with_status(fl, status, cloexec));
        Ok(fd)
    }

    // AGENT: reserve and install two descriptors atomically for pipe-like calls.
    pub fn add_file_pair_with_cloexec(
        &self,
        first: FLike,
        second: FLike,
        cloexec: bool,
    ) -> Result<(usize, usize), &'static str> {
        let mut table = self.process.fd_table.lock().unwrap();
        let first_fd = table.reserve_from(0)?;
        let second_fd = match table.reserve_from(0) {
            Ok(fd) => fd,
            Err(err) => {
                let released = table.allocator.release(first_fd);
                debug_assert!(released);
                return Err(err);
            }
        };
        table
            .entries
            .insert(first_fd, FdEntry::with_cloexec(first, cloexec));
        table
            .entries
            .insert(second_fd, FdEntry::with_cloexec(second, cloexec));
        Ok((first_fd, second_fd))
    }

    // AGENT: expose a compatibility FLike view without direct table mutation.
    pub fn get_file(&self, fd: usize) -> Option<FLike> {
        self.process
            .fd_table
            .lock()
            .unwrap()
            .entries
            .get(&fd)
            .map(FdEntry::as_flike)
    }

    // AGENT: clone an entry while preserving shared open-description semantics.
    pub fn get_fd_entry(&self, fd: usize) -> Option<FdEntry> {
        self.process
            .fd_table
            .lock()
            .unwrap()
            .entries
            .get(&fd)
            .cloned()
    }

    // AGENT: snapshot stdio descriptors with cloexec, status, offset, and sharing.
    pub fn snapshot_checkpoint_fds(&self) -> Result<Vec<SavedFdEntry>, &'static str> {
        let table = self.process.fd_table.lock().unwrap();
        let mut descriptions: Vec<(FdEntry, u32)> = Vec::new();
        let mut saved = Vec::with_capacity(table.entries.len());
        for (&fd, entry) in table.entries.iter() {
            let kind = checkpoint_fd_kind(fd)?;
            if !entry.is_regular_file() {
                return Err("enotsup");
            }
            let description_id = checkpoint_description_id(entry, &mut descriptions)?;
            saved.push(SavedFdEntry {
                fd: u32::try_from(fd).map_err(|_| "einval")?,
                description_id,
                cloexec: entry.is_cloexec(),
                status_flags: u32::try_from(entry.status_flags_bits()).map_err(|_| "einval")?,
                kind,
                offset: entry.offset(),
            });
        }
        Ok(saved)
    }

    // AGENT: restore stdio descriptors and reconstruct shared description ids.
    pub fn restore_checkpoint_fds(&self, fds: &[SavedFdEntry]) -> Result<(), &'static str> {
        let mut restored = BTreeMap::new();
        let mut descriptions: BTreeMap<u32, FdEntry> = BTreeMap::new();
        for saved in fds {
            let fd = usize::try_from(saved.fd).map_err(|_| "einval")?;
            if fd >= MAX_FD || restored.contains_key(&fd) {
                return Err("ebadf");
            }
            let entry = if let Some(template) = descriptions.get(&saved.description_id) {
                template.dup(saved.cloexec)
            } else {
                let (instance, status) = checkpoint_stdio_instance(saved.kind, saved.status_flags)?;
                let entry = FdEntry::with_status(
                    FLike::File(FHandle::new(instance)),
                    status,
                    saved.cloexec,
                );
                entry.seek(FSeek::Start(saved.offset))?;
                descriptions.insert(saved.description_id, entry.clone());
                entry
            };
            restored.insert(fd, entry);
        }

        let replacement = FdTable::from_entries(restored)?;
        let old_table = {
            let mut table = self.process.fd_table.lock().unwrap();
            mem::replace(&mut *table, replacement)
        };
        drop(old_table);
        Ok(())
    }

    // AGENT: snapshot inherited descriptors without exposing FdTable internals.
    pub(super) fn inherit_fds_from(&self, parent: &Task) {
        let inherited = parent.process.fd_table.lock().unwrap().fork_copy();
        *self.process.fd_table.lock().unwrap() = inherited;
    }

    // AGENT: collect close-on-exec descriptors under the unified fd-table lock.
    pub(crate) fn cloexec_fds(&self) -> Vec<usize> {
        self.process.fd_table.lock().unwrap().cloexec_fds()
    }

    // AGENT: remove one descriptor and collect epoll unsubscriptions for later.
    fn remove_fd_locked(
        files: &mut BTreeMap<usize, FdEntry>,
        fd: usize,
    ) -> Result<FdCloseCleanup, &'static str> {
        let closed_entry = files.remove(&fd).ok_or("ebadf")?;

        let mut closed_fd_source_subs = Vec::new();
        for entry in files.values() {
            if let Some(epoll) = entry.epoll_instance() {
                if let Some(sub_id) = epoll.remove_closed_fd(fd) {
                    closed_fd_source_subs.push(sub_id);
                }
            }
        }

        let mut epoll_source_subs = Vec::new();
        let still_open = files
            .values()
            .any(|entry| entry.same_open_description(&closed_entry));
        if !still_open {
            if let Some(epoll) = closed_entry.epoll_instance() {
                for (watched_fd, sub_id) in epoll.drain_source_subs_on_close() {
                    if let Some(source) = files.get(&watched_fd).cloned() {
                        epoll_source_subs.push((source, sub_id));
                    }
                }
            }
        }

        Ok(FdCloseCleanup {
            closed_entry,
            closed_fd_source_subs,
            epoll_source_subs,
        })
    }

    // AGENT: close an fd, detach epoll registrations, and drop it after unlock.
    pub fn close_fd(&self, fd: usize) -> Result<(), &'static str> {
        if fd >= MAX_FD {
            return Err("ebadf");
        }

        let cleanup = {
            let mut table = self.process.fd_table.lock().unwrap();
            let cleanup = Self::remove_fd_locked(&mut table.entries, fd)?;
            let released = table.allocator.release(fd);
            debug_assert!(released);
            cleanup
        };

        cleanup.run();
        Ok(())
    }

    // AGENT: duplicate an entry onto the lowest available descriptor.
    pub fn dup_fd(&self, old_fd: usize, cloexec: bool) -> Result<usize, &'static str> {
        self.dup_fd_from(old_fd, 0, cloexec)
    }

    // AGENT: duplicate an entry at or above the requested lower bound.
    pub fn dup_fd_from(
        &self,
        old_fd: usize,
        start: usize,
        cloexec: bool,
    ) -> Result<usize, &'static str> {
        let mut table = self.process.fd_table.lock().unwrap();
        let entry = table.entries.get(&old_fd).cloned().ok_or("ebadf")?;
        let new_entry = entry.dup(cloexec);
        let new_fd = table.reserve_from(start)?;
        table.entries.insert(new_fd, new_entry);
        Ok(new_fd)
    }

    // AGENT: replace a dup2 target through the same epoll-aware close path.
    pub fn dup2_fd(&self, old_fd: usize, new_fd: usize) -> Result<usize, &'static str> {
        if old_fd >= MAX_FD || new_fd >= MAX_FD {
            return Err("ebadf");
        }
        let cleanup = {
            let mut table = self.process.fd_table.lock().unwrap();
            let entry = table.entries.get(&old_fd).cloned().ok_or("ebadf")?;
            if old_fd == new_fd {
                return Ok(new_fd);
            }
            let cleanup = if table.entries.contains_key(&new_fd) {
                Some(Self::remove_fd_locked(&mut table.entries, new_fd)?)
            } else {
                None
            };
            if cleanup.is_none() && table.allocator.reserve(new_fd).is_none() {
                return Err("ebadf");
            }
            table.entries.insert(new_fd, entry.dup(false));
            cleanup
        };
        if let Some(cleanup) = cleanup {
            cleanup.run();
        }
        Ok(new_fd)
    }

    // AGENT: update per-descriptor close-on-exec state without changing the OFD.
    pub fn set_cloexec(&self, fd: usize, val: bool) -> Result<(), &'static str> {
        let mut table = self.process.fd_table.lock().unwrap();
        let entry = table.entries.get_mut(&fd).ok_or("ebadf")?;
        entry.set_cloexec(val);
        Ok(())
    }
}
