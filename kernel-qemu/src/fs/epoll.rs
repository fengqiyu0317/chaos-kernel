// AGENT
use super::*;

#[repr(C)]
#[derive(Clone, Copy)]
pub struct EpData {
    pub ptr: u64,
}

#[repr(C)]
#[derive(Clone)]
pub struct EpEvent {
    pub events: u32,
    pub data: EpData,
}
impl EpEvent {
    pub const IN: u32 = 0x001;
    pub const OUT: u32 = 0x004;
    pub const ERR: u32 = 0x008;
    pub const HUP: u32 = 0x010;
    pub const PRI: u32 = 0x002;
    pub const RDNORM: u32 = 0x040;
    pub const RDBAND: u32 = 0x080;
    pub const WRNORM: u32 = 0x100;
    pub const WRBAND: u32 = 0x200;
    pub const MSG: u32 = 0x400;
    pub const RDHUP: u32 = 0x2000;
    pub const EXCL: u32 = 1 << 28;
    pub const WAKEUP: u32 = 1 << 29;
    pub const ONESHOT: u32 = 1 << 30;
    pub const ET: u32 = 1 << 31;
    pub fn has(&self, ev: u32) -> bool {
        (self.events & ev) != 0
    }
}

pub struct EpCtlOp;
impl EpCtlOp {
    pub const ADD: i32 = 1;
    pub const DEL: i32 = 2;
    pub const MOD: i32 = 3;
}

// AGENT: identify one Linux-style epoll registration by both the userspace fd
// number and the Arc-owned open-file description installed at ADD time.
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct EpKey {
    fd: usize,
    source: OpenFileRef,
}

// AGENT: build and inspect epoll keys without exposing OpenFileDesc internals.
impl EpKey {
    // AGENT: bind an fd number to the OFD installed in that slot at ctl time.
    pub(crate) fn from_entry(fd: usize, entry: &FdEntry) -> Self {
        Self {
            fd,
            source: entry.open_file_ref(),
        }
    }

    // AGENT: expose the userspace number carried by this exact registration.
    pub(crate) fn fd(&self) -> usize {
        self.fd
    }

    // AGENT: expose the Arc-owned source used for poll and callback cleanup.
    pub(crate) fn source(&self) -> &OpenFileRef {
        &self.source
    }

    // AGENT: remove strong source ownership before storing a key in its EvBus.
    pub(crate) fn downgrade(&self) -> EpWakeKey {
        EpWakeKey {
            fd: self.fd,
            source: self.source.downgrade(),
        }
    }
}

// AGENT: let source-owned EvBus callbacks name a registration without strongly
// retaining the source OFD that owns the callback.
#[derive(Clone)]
pub(crate) struct EpWakeKey {
    fd: usize,
    source: OpenFileWeak,
}

// AGENT: reconstruct a strong EpKey only for the duration of callback delivery.
impl EpWakeKey {
    // AGENT: rebuild a live exact key or discard a stale callback safely.
    pub(crate) fn upgrade(&self) -> Option<EpKey> {
        Some(EpKey {
            fd: self.fd,
            source: self.source.upgrade()?,
        })
    }
}

// AGENT: one watched fd inside an epoll instance. queued mirrors Linux epitem
// membership in the ready list so repeated source callbacks do not duplicate fd
// entries before epoll_wait consumes them.
struct EpItem {
    event: EpEvent,
    source_sub: Option<usize>,
    queued: bool,
}

// AGENT: key both interest and ready state by (fd, OFD) identity.
#[derive(Default)]
struct EpInstInner {
    interests: BTreeMap<EpKey, EpItem>,
    ready_list: VecDeque<EpKey>,
}

// AGENT: keep ready-list membership and interest removal keyed by the exact
// (fd, OFD) registration rather than by a reusable integer fd alone.
impl EpInstInner {
    // AGENT: enqueue one exact registration at most once until consumption.
    fn queue_ready(&mut self, key: &EpKey) -> bool {
        let Some(item) = self.interests.get_mut(key) else {
            return false;
        };
        if item.queued {
            return false;
        }
        item.queued = true;
        self.ready_list.push_back(key.clone());
        true
    }

    // AGENT: remove all queued occurrences for one exact registration.
    fn remove_ready(&mut self, key: &EpKey) {
        if let Some(item) = self.interests.get_mut(key) {
            item.queued = false;
        }
        self.ready_list.retain(|queued_key| queued_key != key);
    }

    // AGENT: detach one interest while preserving cleanup data for the caller.
    fn remove_interest(&mut self, key: &EpKey) -> Option<RemovedEpItem> {
        self.remove_ready(key);
        self.interests.remove(key).map(|item| RemovedEpItem {
            key: key.clone(),
            source_sub: item.source_sub,
        })
    }

    // AGENT: detach every fd-number registration that shares one source OFD.
    fn remove_source(&mut self, source: &OpenFileRef) -> Vec<RemovedEpItem> {
        let keys: Vec<EpKey> = self
            .interests
            .keys()
            .filter(|key| key.source() == source)
            .cloned()
            .collect();
        keys.iter()
            .filter_map(|key| self.remove_interest(key))
            .collect()
    }

    // AGENT: detach all registrations when the epoll OFD loses its final slot.
    fn drain_interests(&mut self) -> Vec<RemovedEpItem> {
        let keys: Vec<EpKey> = self.interests.keys().cloned().collect();
        keys.iter()
            .filter_map(|key| self.remove_interest(key))
            .collect()
    }
}

// AGENT: carry enough source identity out of the EpInst lock to unsubscribe
// callbacks and reverse links without lock re-entry.
struct RemovedEpItem {
    key: EpKey,
    source_sub: Option<usize>,
}

// AGENT: epoll instances now hold an interest table plus a Linux-style ready
// list. epoll_wait consumes ready_list entries instead of scanning every
// registered fd on each wake.
#[derive(Clone)]
pub struct EpInst {
    inner: Arc<Mutex<EpInstInner>>,
    // AGENT: epoll_wait sleeps on this queue and source readiness callbacks
    // wake it when a registered fd becomes ready.
    waiters: Arc<WaitQueue>,
}

// AGENT: weak epoll handle used by OFD reverse links and source callbacks so
// registrations cannot keep a closed epoll instance alive by themselves.
#[derive(Clone)]
pub(crate) struct EpInstWeak {
    inner: Weak<Mutex<EpInstInner>>,
    waiters: Weak<WaitQueue>,
}

// AGENT: upgrade and compare epoll instances by their shared inner allocation.
impl EpInstWeak {
    // AGENT: recover both shared epoll components only while they remain live.
    pub(crate) fn upgrade(&self) -> Option<EpInst> {
        Some(EpInst {
            inner: self.inner.upgrade()?,
            waiters: self.waiters.upgrade()?,
        })
    }

    // AGENT: compare weak reverse links with a live epoll allocation by identity.
    pub(crate) fn same_instance(&self, epoll: &EpInst) -> bool {
        Weak::ptr_eq(&self.inner, &Arc::downgrade(&epoll.inner))
    }
}

// AGENT: drive exact-key readiness, OFD last-close removal, and epoll-instance
// teardown through one shared EpInst implementation.
impl EpInst {
    pub fn new() -> Self {
        EpInst {
            inner: Arc::new(Mutex::new(EpInstInner::default())),
            waiters: Arc::new(WaitQueue::new()),
        }
    }

    // AGENT: create a non-owning epoll handle for OFD backlinks and callbacks.
    pub(crate) fn downgrade(&self) -> EpInstWeak {
        EpInstWeak {
            inner: Arc::downgrade(&self.inner),
            waiters: Arc::downgrade(&self.waiters),
        }
    }

    // AGENT: source callbacks queue one exact watched registration. Stale
    // callbacks are ignored if that (fd, OFD) key has been removed.
    pub(crate) fn mark_ready(&self, key: &EpKey) {
        let queued = self.inner.lock().unwrap().queue_ready(key);
        if queued {
            self.wake_all_waiters();
        }
    }

    // AGENT: level-triggered epoll_wait puts still-ready items back on the
    // ready list so later waits can observe them without rescanning all fds.
    pub(crate) fn requeue_ready(&self, key: &EpKey) {
        self.inner.lock().unwrap().queue_ready(key);
    }

    // AGENT: pop the next possibly-ready watched fd. The caller must poll the
    // registered source state before returning it because readiness can become
    // stale between callback delivery and epoll_wait.
    pub(crate) fn pop_ready(&self) -> Option<(EpKey, EpEvent)> {
        let mut inner = self.inner.lock().unwrap();
        while let Some(key) = inner.ready_list.pop_front() {
            let Some(item) = inner.interests.get_mut(&key) else {
                continue;
            };
            item.queued = false;
            return Some((key, item.event.clone()));
        }
        None
    }

    // AGENT: query one exact registration instead of an ambiguous fd number.
    pub(crate) fn has_interest(&self, key: &EpKey) -> bool {
        self.inner.lock().unwrap().interests.contains_key(key)
    }

    pub fn ready_len(&self) -> usize {
        self.inner.lock().unwrap().ready_list.len()
    }

    // AGENT: expose epoll-fd readiness without making FLike inspect EpInst
    // internals directly.
    pub fn poll_status(&self) -> PollStatus {
        let inner = self.inner.lock().unwrap();
        PollStatus {
            readable: !inner.ready_list.is_empty(),
            ..PollStatus::default()
        }
    }

    // AGENT: enqueue an epoll_wait token only while the ready list is still
    // empty. Holding inner while enqueueing closes the check-then-sleep race
    // against mark_ready().
    pub fn prepare_wait(&self, task_id: usize) -> Option<WaitToken> {
        let inner = self.inner.lock().unwrap();
        if !inner.ready_list.is_empty() {
            return None;
        }
        let token = self.waiters.enqueue_task_locked(task_id);
        Some(token)
    }
    // AGENT: remove a timed-out epoll_wait token from the instance queue.
    pub fn remove_waiter(&self, token: &WaitToken) {
        self.waiters.remove_waiter(token);
    }

    // AGENT: expose only waiter cardinality to cooperative-exit selftests.
    #[cfg(any(test, feature = "qemu-sync-selftest"))]
    pub(crate) fn pending_waiters(&self) -> usize {
        self.waiters.pending()
    }
    // AGENT: remember which EvBus callback backs a watched fd.
    pub(crate) fn set_source_sub(&self, key: &EpKey, sub_id: usize) {
        if let Some(item) = self.inner.lock().unwrap().interests.get_mut(key) {
            item.source_sub = Some(sub_id);
        }
    }
    // AGENT: take the callback id so the caller can unregister it from the
    // concrete source while processing epoll_ctl(DEL/MOD).
    pub(crate) fn take_source_sub(&self, key: &EpKey) -> Option<usize> {
        self.inner
            .lock()
            .unwrap()
            .interests
            .get_mut(key)
            .and_then(|item| item.source_sub.take())
    }

    // AGENT: remove every registration for an OFD only after its final real fd
    // slot closes; duplicate/fork aliases keep these interests alive.
    pub(crate) fn remove_source_on_last_close(&self, source: &OpenFileRef) {
        let removed = self.inner.lock().unwrap().remove_source(source);
        for item in removed {
            if let Some(sub_id) = item.source_sub {
                item.key.source().unregister_epoll_source(sub_id);
            }
        }
    }

    // AGENT: closing the last fd slot for an epoll instance detaches all source
    // callbacks and OFD reverse links outside the EpInst lock.
    pub(crate) fn close_last_fd_slot(&self) {
        let removed = self.inner.lock().unwrap().drain_interests();
        for item in removed {
            item.key.source().remove_epoll_watcher(self);
            if let Some(sub_id) = item.source_sub {
                item.key.source().unregister_epoll_source(sub_id);
            }
        }
        self.wake_all_waiters();
    }

    // AGENT: finish all waiters that are sleeping on this epoll instance.
    fn wake_all_waiters(&self) {
        self.waiters.broadcast();
    }

    // AGENT: EpInst clones share their tables through Arc<Mutex<_>>, so control
    // only needs &self and works for duplicated epoll fds.
    pub(crate) fn control(&self, op: i32, key: EpKey, ev: &EpEvent) -> Result<(), &'static str> {
        match op {
            EpCtlOp::ADD => {
                let mut inner = self.inner.lock().unwrap();
                if inner.interests.contains_key(&key) {
                    return Err("eexist");
                }
                inner.interests.insert(
                    key,
                    EpItem {
                        event: ev.clone(),
                        source_sub: None,
                        queued: false,
                    },
                );
                Ok(())
            }
            EpCtlOp::MOD => {
                let mut inner = self.inner.lock().unwrap();
                inner.remove_ready(&key);
                match inner.interests.get_mut(&key) {
                    Some(item) => {
                        item.event = ev.clone();
                        Ok(())
                    }
                    None => Err("enoent"),
                }
            }
            EpCtlOp::DEL => {
                if self.inner.lock().unwrap().remove_interest(&key).is_none() {
                    return Err("enoent");
                }
                Ok(())
            }
            _ => Err("einval"),
        }
    }
}
