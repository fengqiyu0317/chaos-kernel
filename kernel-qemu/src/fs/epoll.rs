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

// AGENT: epoll instances now own both the interest table and a wait queue that
// source readiness callbacks can wake.
#[derive(Clone)]
pub struct EpInst {
    pub events: Arc<Mutex<BTreeMap<usize, EpEvent>>>,
    pub ready: Arc<Mutex<BTreeSet<usize>>>,
    // AGENT: epoll_wait sleeps on this queue and source readiness callbacks
    // wake it when a registered fd becomes ready.
    pub waiters: Arc<Mutex<VecDeque<WaitToken>>>,
    // AGENT: fd -> EvBus subscription id for registrations backed by a
    // cancellable readiness source such as PipeNode.
    source_subs: Arc<Mutex<BTreeMap<usize, usize>>>,
}
impl EpInst {
    pub fn new() -> Self {
        EpInst {
            events: Arc::new(Mutex::new(BTreeMap::new())),
            ready: Arc::new(Mutex::new(BTreeSet::new())),
            waiters: Arc::new(Mutex::new(VecDeque::new())),
            source_subs: Arc::new(Mutex::new(BTreeMap::new())),
        }
    }
    // AGENT: notify epoll_wait waiters that one watched fd has reached a
    // readiness state. Stale callbacks are ignored if the fd is no longer
    // registered in this epoll instance.
    pub fn mark_ready(&self, fd: usize) {
        if !self.events.lock().unwrap().contains_key(&fd) {
            return;
        }
        self.ready.lock().unwrap().insert(fd);
        let batch: Vec<WaitToken> = self.waiters.lock().unwrap().drain(..).collect();
        for token in batch {
            token.wake();
        }
    }
    // AGENT: clear cached readiness before a level-triggered rescan; new
    // callbacks racing after this point repopulate the cache and wake waiters.
    pub fn clear_ready(&self) {
        self.ready.lock().unwrap().clear();
    }
    // AGENT: preserve the compatibility ready cache for FLike::Ep::poll().
    pub fn replace_ready(&self, ready_fds: BTreeSet<usize>) {
        *self.ready.lock().unwrap() = ready_fds;
    }
    // AGENT: enqueue an epoll_wait token only if no readiness callback has
    // populated the cache since the last scan.
    pub fn prepare_wait(&self) -> Option<WaitToken> {
        let ready = self.ready.lock().unwrap();
        if !ready.is_empty() {
            return None;
        }
        let token = WaitToken::current();
        self.waiters.lock().unwrap().push_back(token.clone());
        Some(token)
    }
    // AGENT: remove a timed-out epoll_wait token from the instance queue.
    pub fn remove_waiter(&self, token: &WaitToken) {
        self.waiters
            .lock()
            .unwrap()
            .retain(|queued| !queued.same(token));
    }
    // AGENT: remember which EvBus callback backs a watched fd.
    pub fn set_source_sub(&self, fd: usize, sub_id: usize) {
        self.source_subs.lock().unwrap().insert(fd, sub_id);
    }
    // AGENT: take the callback id so the caller can unregister it from the
    // concrete source while processing epoll_ctl(DEL/MOD).
    pub fn take_source_sub(&self, fd: usize) -> Option<usize> {
        self.source_subs.lock().unwrap().remove(&fd)
    }

    // AGENT: closing a watched fd removes its registration and returns the
    // source subscription id that must be cancelled on the watched file object.
    pub fn remove_closed_fd(&self, fd: usize) -> Option<usize> {
        self.events.lock().unwrap().remove(&fd);
        self.ready.lock().unwrap().remove(&fd);
        self.source_subs.lock().unwrap().remove(&fd)
    }

    // AGENT: closing the last descriptor for an epoll instance detaches every
    // source callback and wakes waiters so they can observe the closed fd.
    pub fn drain_source_subs_on_close(&self) -> Vec<(usize, usize)> {
        let subs = {
            let mut source_subs = self.source_subs.lock().unwrap();
            let drained = source_subs
                .iter()
                .map(|(&fd, &sub_id)| (fd, sub_id))
                .collect();
            source_subs.clear();
            drained
        };

        self.events.lock().unwrap().clear();
        self.ready.lock().unwrap().clear();

        let waiters: Vec<WaitToken> = self.waiters.lock().unwrap().drain(..).collect();
        for token in waiters {
            token.wake();
        }

        subs
    }

    // AGENT: EpInst clones share their tables through Arc<Mutex<_>>, so control
    // only needs &self and works for duplicated epoll fds.
    pub fn control(&self, op: i32, fd: usize, ev: &EpEvent) -> Result<(), &'static str> {
        let mut events = self.events.lock().unwrap();
        match op {
            EpCtlOp::ADD => {
                if events.contains_key(&fd) {
                    return Err("eexist");
                }
                events.insert(fd, ev.clone());
                Ok(())
            }
            EpCtlOp::MOD => {
                if !events.contains_key(&fd) {
                    return Err("enoent");
                }
                events.insert(fd, ev.clone());
                Ok(())
            }
            EpCtlOp::DEL => {
                if events.remove(&fd).is_none() {
                    return Err("enoent");
                }
                self.ready.lock().unwrap().remove(&fd);
                Ok(())
            }
            _ => Err("einval"),
        }
    }
}
