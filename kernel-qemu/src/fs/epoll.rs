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

// AGENT: one watched fd inside an epoll instance. queued mirrors Linux epitem
// membership in the ready list so repeated source callbacks do not duplicate fd
// entries before epoll_wait consumes them.
struct EpItem {
    event: EpEvent,
    source_sub: Option<usize>,
    queued: bool,
}

#[derive(Default)]
struct EpInstInner {
    interests: BTreeMap<usize, EpItem>,
    ready_list: VecDeque<usize>,
}

impl EpInstInner {
    fn queue_ready(&mut self, fd: usize) -> bool {
        let Some(item) = self.interests.get_mut(&fd) else {
            return false;
        };
        if item.queued {
            return false;
        }
        item.queued = true;
        self.ready_list.push_back(fd);
        true
    }

    fn remove_ready(&mut self, fd: usize) {
        if let Some(item) = self.interests.get_mut(&fd) {
            item.queued = false;
        }
        self.ready_list.retain(|queued_fd| *queued_fd != fd);
    }
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
impl EpInst {
    pub fn new() -> Self {
        EpInst {
            inner: Arc::new(Mutex::new(EpInstInner::default())),
            waiters: Arc::new(WaitQueue::new()),
        }
    }
    // AGENT: source callbacks queue one watched fd onto the ready list. Stale
    // callbacks are ignored if the fd is no longer registered in this instance.
    pub fn mark_ready(&self, fd: usize) {
        let queued = self.inner.lock().unwrap().queue_ready(fd);
        if queued {
            self.wake_all_waiters();
        }
    }

    // AGENT: level-triggered epoll_wait puts still-ready items back on the
    // ready list so later waits can observe them without rescanning all fds.
    pub fn requeue_ready(&self, fd: usize) {
        self.inner.lock().unwrap().queue_ready(fd);
    }

    // AGENT: pop the next possibly-ready watched fd. The caller must poll the
    // current fd state before returning it to userspace because readiness can
    // become stale between callback delivery and epoll_wait.
    pub fn pop_ready(&self) -> Option<(usize, EpEvent)> {
        let mut inner = self.inner.lock().unwrap();
        while let Some(fd) = inner.ready_list.pop_front() {
            let Some(item) = inner.interests.get_mut(&fd) else {
                continue;
            };
            item.queued = false;
            return Some((fd, item.event.clone()));
        }
        None
    }

    pub fn has_interest(&self, fd: usize) -> bool {
        self.inner.lock().unwrap().interests.contains_key(&fd)
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
    pub fn prepare_wait(&self) -> Option<WaitToken> {
        let inner = self.inner.lock().unwrap();
        if !inner.ready_list.is_empty() {
            return None;
        }
        let token = self.waiters.enqueue_current_locked();
        Some(token)
    }
    // AGENT: remove a timed-out epoll_wait token from the instance queue.
    pub fn remove_waiter(&self, token: &WaitToken) {
        self.waiters.remove_waiter(token);
    }
    // AGENT: remember which EvBus callback backs a watched fd.
    pub fn set_source_sub(&self, fd: usize, sub_id: usize) {
        if let Some(item) = self.inner.lock().unwrap().interests.get_mut(&fd) {
            item.source_sub = Some(sub_id);
        }
    }
    // AGENT: take the callback id so the caller can unregister it from the
    // concrete source while processing epoll_ctl(DEL/MOD).
    pub fn take_source_sub(&self, fd: usize) -> Option<usize> {
        self.inner
            .lock()
            .unwrap()
            .interests
            .get_mut(&fd)
            .and_then(|item| item.source_sub.take())
    }

    // AGENT: closing a watched fd removes its registration and returns the
    // source subscription id that must be cancelled on the watched file object.
    pub fn remove_closed_fd(&self, fd: usize) -> Option<usize> {
        self.remove_interest(fd).and_then(|item| item.source_sub)
    }

    // AGENT: closing the last descriptor for an epoll instance detaches every
    // source callback and wakes waiters so they can observe the closed fd.
    pub fn drain_source_subs_on_close(&self) -> Vec<(usize, usize)> {
        let subs = {
            let mut inner = self.inner.lock().unwrap();
            let drained = inner
                .interests
                .iter()
                .filter_map(|(&fd, item)| item.source_sub.map(|sub_id| (fd, sub_id)))
                .collect();
            inner.interests.clear();
            inner.ready_list.clear();
            drained
        };
        self.wake_all_waiters();

        subs
    }

    // AGENT: remove the epoll-visible part of one watched fd registration.
    fn remove_interest(&self, fd: usize) -> Option<EpItem> {
        let mut inner = self.inner.lock().unwrap();
        inner.remove_ready(fd);
        inner.interests.remove(&fd)
    }

    // AGENT: finish all waiters that are sleeping on this epoll instance.
    fn wake_all_waiters(&self) {
        self.waiters.broadcast();
    }

    // AGENT: EpInst clones share their tables through Arc<Mutex<_>>, so control
    // only needs &self and works for duplicated epoll fds.
    pub fn control(&self, op: i32, fd: usize, ev: &EpEvent) -> Result<(), &'static str> {
        match op {
            EpCtlOp::ADD => {
                let mut inner = self.inner.lock().unwrap();
                if inner.interests.contains_key(&fd) {
                    return Err("eexist");
                }
                inner.interests.insert(
                    fd,
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
                inner.remove_ready(fd);
                match inner.interests.get_mut(&fd) {
                    Some(item) => {
                        item.event = ev.clone();
                        Ok(())
                    }
                    None => Err("enoent"),
                }
            }
            EpCtlOp::DEL => {
                if self.remove_interest(fd).is_none() {
                    return Err("enoent");
                }
                Ok(())
            }
            _ => Err("einval"),
        }
    }
}
