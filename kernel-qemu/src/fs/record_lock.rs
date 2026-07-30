// AGENT: implement process-associated POSIX record locks over stable ChaosFs
// file identity, including range replacement, blocking waits, and deadlock checks.
use super::*;

const MAX_RECORD_LOCKS: usize = 4096;

// AGENT: identify one inode across every mount view of the same filesystem.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct FileIdentity {
    pub fs_id: FsId,
    pub inode: InodeId,
}

// AGENT: keep the copied-in RV64 flock fields independent of Rust layout and
// kernel record-lock storage.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct FlockArg {
    pub lock_type: i16,
    pub whence: i16,
    pub start: i64,
    pub len: i64,
    pub pid: i32,
}

// AGENT: normalize every request to a half-open range; None reaches through
// the current EOF and every later extension of the file.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct LockRange {
    pub start: u64,
    pub end: Option<u64>,
}

// AGENT: distinguish compatible read locks from exclusive write locks without
// carrying ABI numeric values into the lock table.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LockKind {
    Read,
    Write,
}

// AGENT: carry one validated file/range operation from the fd layer into the
// process-associated global lock authority; None means F_UNLCK.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RecordLockRequest {
    pub identity: FileIdentity,
    pub kind: Option<LockKind>,
    pub range: LockRange,
}

// AGENT: validate ABI lock/access values and convert SEEK_SET/CUR/END plus
// positive, zero, or negative lengths into one checked normalized range.
impl RecordLockRequest {
    pub fn from_flock(
        identity: FileIdentity,
        status: FdOpt,
        offset: u64,
        file_len: usize,
        flock: FlockArg,
        allow_unlock: bool,
    ) -> Result<Self, &'static str> {
        let kind = match flock.lock_type {
            F_RDLCK if status.rd => Some(LockKind::Read),
            F_RDLCK => return Err("ebadf"),
            F_WRLCK if status.wr => Some(LockKind::Write),
            F_WRLCK => return Err("ebadf"),
            F_UNLCK if allow_unlock => None,
            _ => return Err("einval"),
        };
        let base = match flock.whence {
            SEEK_SET => 0,
            SEEK_CUR => i64::try_from(offset).map_err(|_| "eoverflow")?,
            SEEK_END => i64::try_from(file_len).map_err(|_| "eoverflow")?,
            _ => return Err("einval"),
        };
        let origin = base.checked_add(flock.start).ok_or("eoverflow")?;
        let (start, end) = match flock.len.cmp(&0) {
            CmpOrd::Greater => {
                let end = origin.checked_add(flock.len).ok_or("eoverflow")?;
                (origin, Some(end))
            }
            CmpOrd::Equal => (origin, None),
            CmpOrd::Less => {
                let start = origin.checked_add(flock.len).ok_or("eoverflow")?;
                (start, Some(origin))
            }
        };
        let start = u64::try_from(start).map_err(|_| "einval")?;
        let end = end
            .map(|end| u64::try_from(end).map_err(|_| "einval"))
            .transpose()?;
        if end.is_some_and(|end| end <= start) {
            return Err("einval");
        }
        Ok(Self {
            identity,
            kind,
            range: LockRange { start, end },
        })
    }
}

// AGENT: retain normalized lock ownership by process PID, never by fd, OFD, or
// the individual thread that happened to issue fcntl.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct RecordLock {
    owner_pid: usize,
    kind: LockKind,
    range: LockRange,
}

// AGENT: record the current blockers of one sleeping process so a new F_SETLKW
// can reject wait-for cycles before it queues another scheduler wait token.
#[derive(Clone, Debug)]
struct WaitingLock {
    request: RecordLockRequest,
    blockers: BTreeSet<usize>,
}

// AGENT: protect locks and the process wait-for graph with one IRQ-safe state
// lock; this gives close/exit bulk release one lock order across all files.
#[derive(Default)]
struct RecordLockState {
    locks: BTreeMap<FileIdentity, Vec<RecordLock>>,
    waiting: BTreeMap<usize, WaitingLock>,
}

// AGENT: own all process-associated record locks and the queue used to retry
// conflict checks after any unlock, replacement, close, or exit transition.
pub struct RecordLockTable {
    state: Mutex<RecordLockState>,
    waiters: WaitQueue,
}

// AGENT: create an empty record-lock authority for each Kernel instance.
impl Default for RecordLockTable {
    fn default() -> Self {
        Self::new()
    }
}

// AGENT: centralize conflict queries, transactional range replacement, blocking
// retry, close/exit release, and wait-for graph maintenance.
impl RecordLockTable {
    pub fn new() -> Self {
        Self {
            state: Mutex::new(RecordLockState::default()),
            waiters: WaitQueue::new(),
        }
    }

    // AGENT: report the first conflicting lock in normalized byte order and
    // translate it back into a SEEK_SET flock result for F_GETLK.
    pub fn query(
        &self,
        owner_pid: usize,
        request: RecordLockRequest,
        flock: &mut FlockArg,
    ) -> Result<(), &'static str> {
        let kind = request.kind.ok_or("einval")?;
        let state = self.state.lock().unwrap();
        let conflict = first_conflict(&state, owner_pid, request.identity, kind, request.range);
        let Some(conflict) = conflict else {
            flock.lock_type = F_UNLCK;
            return Ok(());
        };

        flock.lock_type = match conflict.kind {
            LockKind::Read => F_RDLCK,
            LockKind::Write => F_WRLCK,
        };
        flock.whence = SEEK_SET;
        flock.start = i64::try_from(conflict.range.start).map_err(|_| "eoverflow")?;
        flock.len = match conflict.range.end {
            Some(end) => i64::try_from(end - conflict.range.start).map_err(|_| "eoverflow")?,
            None => 0,
        };
        flock.pid = i32::try_from(conflict.owner_pid).map_err(|_| "eoverflow")?;
        Ok(())
    }

    // AGENT: fail immediately on an inter-process conflict while still applying
    // same-owner replacement, splitting, merging, and unlock atomically.
    pub fn set_nonblocking(
        &self,
        owner_pid: usize,
        request: RecordLockRequest,
    ) -> Result<(), &'static str> {
        let changed = {
            let mut state = self.state.lock().unwrap();
            if let Some(kind) = request.kind {
                if first_conflict(&state, owner_pid, request.identity, kind, request.range)
                    .is_some()
                {
                    return Err("eagain");
                }
            }
            apply_request(&mut state, owner_pid, request)?;
            refresh_wait_dependencies(&mut state);
            true
        };
        if changed {
            self.waiters.broadcast();
        }
        Ok(())
    }

    // AGENT: enqueue while the same state lock still protects the conflict
    // check, then sleep interruptibly only after releasing that lock.
    pub fn set_blocking(
        &self,
        owner_pid: usize,
        task_id: usize,
        request: RecordLockRequest,
    ) -> Result<(), &'static str> {
        if request.kind.is_none() {
            return self.set_nonblocking(owner_pid, request);
        }

        loop {
            let token = {
                let mut state = self.state.lock().unwrap();
                let kind = request.kind.ok_or("einval")?;
                let blockers =
                    conflict_owners(&state, owner_pid, request.identity, kind, request.range);
                if blockers.is_empty() {
                    state.waiting.remove(&owner_pid);
                    apply_request(&mut state, owner_pid, request)?;
                    refresh_wait_dependencies(&mut state);
                    drop(state);
                    self.waiters.broadcast();
                    return Ok(());
                }
                if would_deadlock(&state, owner_pid, &blockers) {
                    return Err("edeadlk");
                }
                state
                    .waiting
                    .insert(owner_pid, WaitingLock { request, blockers });
                self.waiters.enqueue_task_locked(task_id)
            };

            let outcome = token.wait_interruptible(None);
            self.waiters.remove_waiter(&token);
            self.state.lock().unwrap().waiting.remove(&owner_pid);
            if outcome == WaitOutcome::Signal {
                return Err("eintr");
            }
        }
    }

    // AGENT: closing any descriptor for a file releases every POSIX lock that
    // the process owns on that inode, independent of fd/OFD aliasing.
    pub fn release_file(&self, owner_pid: usize, identity: FileIdentity) {
        let changed = {
            let mut state = self.state.lock().unwrap();
            let mut changed = false;
            if let Some(locks) = state.locks.get_mut(&identity) {
                let before = locks.len();
                locks.retain(|lock| lock.owner_pid != owner_pid);
                changed = locks.len() != before;
                if locks.is_empty() {
                    state.locks.remove(&identity);
                }
            }
            if changed {
                refresh_wait_dependencies(&mut state);
            }
            changed
        };
        if changed {
            self.waiters.broadcast();
        }
    }

    // AGENT: process exit removes all owned locks and every outgoing/incoming
    // wait dependency before fd-table destruction drops the final objects.
    pub fn release_process(&self, owner_pid: usize) {
        let changed = {
            let mut state = self.state.lock().unwrap();
            let mut changed = state.waiting.remove(&owner_pid).is_some();
            state.locks.retain(|_, locks| {
                let before = locks.len();
                locks.retain(|lock| lock.owner_pid != owner_pid);
                changed |= locks.len() != before;
                !locks.is_empty()
            });
            if changed {
                refresh_wait_dependencies(&mut state);
            }
            changed
        };
        if changed {
            self.waiters.broadcast();
        }
    }

    // AGENT: let the first checkpoint format reject process-associated locks
    // instead of silently producing an image that loses them on restore.
    pub fn process_has_locks(&self, owner_pid: usize) -> bool {
        self.state
            .lock()
            .unwrap()
            .locks
            .values()
            .any(|locks| locks.iter().any(|lock| lock.owner_pid == owner_pid))
    }
}

// AGENT: treat None as positive infinity for half-open interval comparisons.
fn end_after(end: Option<u64>, point: u64) -> bool {
    end.is_none_or(|end| end > point)
}

// AGENT: detect overlap between two normalized non-empty half-open ranges.
fn ranges_overlap(lhs: LockRange, rhs: LockRange) -> bool {
    end_after(lhs.end, rhs.start) && end_after(rhs.end, lhs.start)
}

// AGENT: merge two ordered overlapping or adjacent ranges of the same lock kind.
fn merge_ranges(lhs: LockRange, rhs: LockRange) -> LockRange {
    let end = match (lhs.end, rhs.end) {
        (None, _) | (_, None) => None,
        (Some(lhs), Some(rhs)) => Some(max(lhs, rhs)),
    };
    LockRange {
        start: min(lhs.start, rhs.start),
        end,
    }
}

// AGENT: subtract one target interval from an owned interval, preserving zero,
// one, or two non-empty fragments for later same-kind merging.
fn subtract_range(original: LockRange, removed: LockRange, out: &mut Vec<LockRange>) {
    if !ranges_overlap(original, removed) {
        out.push(original);
        return;
    }
    if original.start < removed.start {
        out.push(LockRange {
            start: original.start,
            end: Some(removed.start),
        });
    }
    if let Some(removed_end) = removed.end {
        if end_after(original.end, removed_end) {
            out.push(LockRange {
                start: removed_end,
                end: original.end,
            });
        }
    }
}

// AGENT: return the first conflicting lock according to normalized range, PID,
// and lock-kind order already maintained inside each file bucket.
fn first_conflict(
    state: &RecordLockState,
    owner_pid: usize,
    identity: FileIdentity,
    kind: LockKind,
    range: LockRange,
) -> Option<RecordLock> {
    state.locks.get(&identity).and_then(|locks| {
        locks
            .iter()
            .copied()
            .find(|lock| lock_conflicts(*lock, owner_pid, kind, range))
    })
}

// AGENT: collect every conflicting process once for wait-for graph construction.
fn conflict_owners(
    state: &RecordLockState,
    owner_pid: usize,
    identity: FileIdentity,
    kind: LockKind,
    range: LockRange,
) -> BTreeSet<usize> {
    state
        .locks
        .get(&identity)
        .into_iter()
        .flatten()
        .filter(|lock| lock_conflicts(**lock, owner_pid, kind, range))
        .map(|lock| lock.owner_pid)
        .collect()
}

// AGENT: same-process locks never conflict; overlapping reads from different
// processes remain compatible while every overlap involving a writer conflicts.
fn lock_conflicts(lock: RecordLock, owner_pid: usize, kind: LockKind, range: LockRange) -> bool {
    lock.owner_pid != owner_pid
        && ranges_overlap(lock.range, range)
        && (lock.kind == LockKind::Write || kind == LockKind::Write)
}

// AGENT: apply one same-owner replacement transaction to a single file bucket
// and reject table growth beyond the explicit first-stage resource limit.
fn apply_request(
    state: &mut RecordLockState,
    owner_pid: usize,
    request: RecordLockRequest,
) -> Result<(), &'static str> {
    let old_bucket = state
        .locks
        .get(&request.identity)
        .cloned()
        .unwrap_or_default();
    let mut other = Vec::new();
    let mut owned = Vec::new();
    for lock in old_bucket.iter().copied() {
        if lock.owner_pid != owner_pid {
            other.push(lock);
            continue;
        }
        let mut fragments = Vec::new();
        subtract_range(lock.range, request.range, &mut fragments);
        owned.extend(fragments.into_iter().map(|range| RecordLock {
            owner_pid,
            kind: lock.kind,
            range,
        }));
    }
    if let Some(kind) = request.kind {
        owned.push(RecordLock {
            owner_pid,
            kind,
            range: request.range,
        });
    }

    owned.sort_by_key(|lock| (lock.range.start, lock.kind as u8));
    let mut merged: Vec<RecordLock> = Vec::new();
    for lock in owned {
        if let Some(last) = merged.last_mut() {
            let touches = last.range.end == Some(lock.range.start);
            if last.kind == lock.kind && (ranges_overlap(last.range, lock.range) || touches) {
                last.range = merge_ranges(last.range, lock.range);
                continue;
            }
        }
        merged.push(lock);
    }
    other.extend(merged);
    other.sort_by_key(|lock| (lock.range.start, lock.owner_pid, lock.kind as u8));

    let current_total: usize = state.locks.values().map(Vec::len).sum();
    let next_total = current_total - old_bucket.len() + other.len();
    if next_total > MAX_RECORD_LOCKS {
        return Err("enolck");
    }
    if other.is_empty() {
        state.locks.remove(&request.identity);
    } else {
        state.locks.insert(request.identity, other);
    }
    Ok(())
}

// AGENT: refresh blockers against live locks after any mutation so stale edges
// cannot manufacture a false deadlock while awakened tasks retry.
fn refresh_wait_dependencies(state: &mut RecordLockState) {
    let waiting: Vec<(usize, RecordLockRequest)> = state
        .waiting
        .iter()
        .map(|(&owner, waiting)| (owner, waiting.request))
        .collect();
    for (owner, request) in waiting {
        let blockers = request.kind.map_or_else(BTreeSet::new, |kind| {
            conflict_owners(state, owner, request.identity, kind, request.range)
        });
        if let Some(waiting) = state.waiting.get_mut(&owner) {
            waiting.blockers = blockers;
        }
    }
}

// AGENT: reject one proposed edge set when any blocker already reaches the new
// waiter through the current process wait-for graph.
fn would_deadlock(state: &RecordLockState, owner_pid: usize, blockers: &BTreeSet<usize>) -> bool {
    blockers.iter().copied().any(|blocker| {
        let mut seen = BTreeSet::new();
        wait_path_reaches(state, blocker, owner_pid, &mut seen)
    })
}

// AGENT: walk process dependencies without recursion cycles or allocation-owned
// references that could outlive the record-lock state guard.
fn wait_path_reaches(
    state: &RecordLockState,
    current: usize,
    target: usize,
    seen: &mut BTreeSet<usize>,
) -> bool {
    if current == target {
        return true;
    }
    if !seen.insert(current) {
        return false;
    }
    state.waiting.get(&current).is_some_and(|waiting| {
        waiting
            .blockers
            .iter()
            .copied()
            .any(|next| wait_path_reaches(state, next, target, seen))
    })
}

// AGENT: keep pure range/conflict/deadlock regressions beside the lock authority
// and expose them through the existing QEMU filesystem selftest feature.
#[cfg(any(test, feature = "qemu-fs-selftest"))]
pub mod tests {
    use super::*;

    const FILE_A: FileIdentity = FileIdentity { fs_id: 7, inode: 9 };
    const FILE_B: FileIdentity = FileIdentity {
        fs_id: 7,
        inode: 10,
    };

    pub fn run_all() {
        flock_ranges_follow_seek_and_signed_length_rules();
        read_write_conflicts_use_file_and_process_identity();
        same_process_updates_split_replace_and_merge();
        wait_graph_detects_process_cycles();
    }

    fn status(rd: bool, wr: bool) -> FdOpt {
        FdOpt {
            rd,
            wr,
            ap: false,
            nb: false,
        }
    }

    fn flock(lock_type: i16, whence: i16, start: i64, len: i64) -> FlockArg {
        FlockArg {
            lock_type,
            whence,
            start,
            len,
            pid: 0,
        }
    }

    fn request(
        identity: FileIdentity,
        kind: Option<LockKind>,
        start: u64,
        end: Option<u64>,
    ) -> RecordLockRequest {
        RecordLockRequest {
            identity,
            kind,
            range: LockRange { start, end },
        }
    }

    // AGENT: cover SEEK_SET/CUR/END, positive/zero/negative lengths, access-mode
    // checks, invalid origins, and signed overflow at the ABI-neutral boundary.
    #[cfg_attr(test, test)]
    fn flock_ranges_follow_seek_and_signed_length_rules() {
        assert_eq!(
            RecordLockRequest::from_flock(
                FILE_A,
                status(true, true),
                20,
                100,
                flock(F_RDLCK, SEEK_SET, 5, 10),
                false,
            )
            .unwrap()
            .range,
            LockRange {
                start: 5,
                end: Some(15)
            }
        );
        assert_eq!(
            RecordLockRequest::from_flock(
                FILE_A,
                status(true, true),
                20,
                100,
                flock(F_WRLCK, SEEK_CUR, -5, 0),
                false,
            )
            .unwrap()
            .range,
            LockRange {
                start: 15,
                end: None
            }
        );
        assert_eq!(
            RecordLockRequest::from_flock(
                FILE_A,
                status(true, true),
                20,
                100,
                flock(F_WRLCK, SEEK_END, 0, -30),
                false,
            )
            .unwrap()
            .range,
            LockRange {
                start: 70,
                end: Some(100)
            }
        );
        assert_eq!(
            RecordLockRequest::from_flock(
                FILE_A,
                status(false, true),
                0,
                0,
                flock(F_RDLCK, SEEK_SET, 0, 1),
                false,
            ),
            Err("ebadf")
        );
        assert_eq!(
            RecordLockRequest::from_flock(
                FILE_A,
                status(true, false),
                0,
                0,
                flock(F_WRLCK, SEEK_SET, 0, 1),
                false,
            ),
            Err("ebadf")
        );
        assert_eq!(
            RecordLockRequest::from_flock(
                FILE_A,
                status(true, true),
                0,
                0,
                flock(F_UNLCK, SEEK_SET, 0, 1),
                false,
            ),
            Err("einval")
        );
        assert_eq!(
            RecordLockRequest::from_flock(
                FILE_A,
                status(true, true),
                0,
                0,
                flock(F_WRLCK, SEEK_SET, -1, 1),
                false,
            ),
            Err("einval")
        );
        assert_eq!(
            RecordLockRequest::from_flock(
                FILE_A,
                status(true, true),
                0,
                0,
                flock(F_WRLCK, SEEK_SET, i64::MAX, 1),
                false,
            ),
            Err("eoverflow")
        );
    }

    // AGENT: verify compatible cross-process reads, write conflicts, GETLK PID,
    // same-source identity collisions, and independent fs/inode namespaces.
    #[cfg_attr(test, test)]
    fn read_write_conflicts_use_file_and_process_identity() {
        let table = RecordLockTable::new();
        let read = request(FILE_A, Some(LockKind::Read), 0, Some(20));
        let write = request(FILE_A, Some(LockKind::Write), 10, Some(30));
        assert_eq!(table.set_nonblocking(11, read), Ok(()));
        assert_eq!(table.set_nonblocking(12, read), Ok(()));
        assert_eq!(table.set_nonblocking(13, write), Err("eagain"));

        let mut result = flock(F_WRLCK, SEEK_SET, 10, 20);
        table.query(13, write, &mut result).unwrap();
        assert_eq!(result.lock_type, F_RDLCK);
        assert_eq!(result.start, 0);
        assert_eq!(result.len, 20);
        assert_eq!(result.pid, 11);

        let other_fs = request(
            FileIdentity { fs_id: 8, inode: 9 },
            Some(LockKind::Write),
            0,
            Some(20),
        );
        let other_inode = request(FILE_B, Some(LockKind::Write), 0, Some(20));
        assert_eq!(table.set_nonblocking(13, other_fs), Ok(()));
        assert_eq!(table.set_nonblocking(13, other_inode), Ok(()));

        table.release_file(11, FILE_A);
        table.release_file(12, FILE_A);
        assert_eq!(table.set_nonblocking(13, write), Ok(()));
        assert!(table.process_has_locks(13));
        table.release_process(13);
        assert!(!table.process_has_locks(13));
    }

    // AGENT: prove partial unlock splits, replacement changes only the selected
    // interval, and restoring one kind coalesces adjacent same-owner segments.
    #[cfg_attr(test, test)]
    fn same_process_updates_split_replace_and_merge() {
        let table = RecordLockTable::new();
        table
            .set_nonblocking(21, request(FILE_A, Some(LockKind::Write), 0, Some(100)))
            .unwrap();
        table
            .set_nonblocking(21, request(FILE_A, None, 20, Some(40)))
            .unwrap();
        {
            let state = table.state.lock().unwrap();
            let locks = state.locks.get(&FILE_A).unwrap();
            assert_eq!(locks.len(), 2);
            assert_eq!(
                locks[0].range,
                LockRange {
                    start: 0,
                    end: Some(20)
                }
            );
            assert_eq!(
                locks[1].range,
                LockRange {
                    start: 40,
                    end: Some(100)
                }
            );
        }

        table
            .set_nonblocking(21, request(FILE_A, Some(LockKind::Read), 20, Some(40)))
            .unwrap();
        {
            let state = table.state.lock().unwrap();
            let locks = state.locks.get(&FILE_A).unwrap();
            assert_eq!(locks.len(), 3);
            assert_eq!(locks[1].kind, LockKind::Read);
        }

        table
            .set_nonblocking(21, request(FILE_A, Some(LockKind::Write), 20, Some(40)))
            .unwrap();
        let state = table.state.lock().unwrap();
        let locks = state.locks.get(&FILE_A).unwrap();
        assert_eq!(locks.len(), 1);
        assert_eq!(locks[0].kind, LockKind::Write);
        assert_eq!(
            locks[0].range,
            LockRange {
                start: 0,
                end: Some(100)
            }
        );
    }

    // AGENT: build a two-process wait-for edge and prove the reverse request is
    // rejected as EDEADLK before it can enqueue a second waiter.
    #[cfg_attr(test, test)]
    fn wait_graph_detects_process_cycles() {
        let mut state = RecordLockState::default();
        apply_request(
            &mut state,
            31,
            request(FILE_A, Some(LockKind::Write), 0, Some(10)),
        )
        .unwrap();
        apply_request(
            &mut state,
            32,
            request(FILE_B, Some(LockKind::Write), 0, Some(10)),
        )
        .unwrap();
        state.waiting.insert(
            31,
            WaitingLock {
                request: request(FILE_B, Some(LockKind::Write), 0, Some(10)),
                blockers: BTreeSet::from([32]),
            },
        );
        assert!(would_deadlock(&state, 32, &BTreeSet::from([31])));
        assert!(!would_deadlock(&state, 33, &BTreeSet::from([31])));
    }
}
