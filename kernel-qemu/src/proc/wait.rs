// AGENT
use super::*;

// AGENT: ProcessGroup keeps only group identity, membership, and session
// ownership. The group leader is derived from pgid, and foreground state
// belongs to the future session/TTY layer.

pub type Pgid = i32;

pub struct ProcessGroup {
    pub pgid: Pgid,
    pub members: Mutex<Vec<usize>>,
    pub session_id: usize,
}

impl ProcessGroup {
    // AGENT: leader is only the initial member pid; do not store it separately
    // from pgid.
    pub fn new(pgid: Pgid, leader: usize, session: usize) -> Self {
        Self {
            pgid,
            members: Mutex::new(vec![leader]),
            session_id: session,
        }
    }

    pub fn add_member(&self, pid: usize) {
        let mut members = self.members.lock().unwrap();
        if !members.contains(&pid) {
            members.push(pid);
        }
    }

    pub fn remove_member(&self, pid: usize) -> bool {
        let mut members = self.members.lock().unwrap();
        let before = members.len();
        members.retain(|&m| m != pid);
        members.len() < before
    }

    // AGENT: snapshot membership before looking up tasks so callers do not
    // hold the group member lock while entering TaskTable.
    pub fn members_snapshot(&self) -> Vec<usize> {
        self.members.lock().unwrap().clone()
    }

    pub fn is_empty(&self) -> bool {
        self.members.lock().unwrap().is_empty()
    }

    pub fn member_count(&self) -> usize {
        self.members.lock().unwrap().len()
    }

    // AGENT: process-group leader identity is represented by pgid.
    pub fn is_leader(&self, pid: usize) -> bool {
        self.pgid as usize == pid
    }

    // AGENT: use the authoritative member snapshot and skip already-dead tasks
    // when broadcasting through a live process group.
    pub fn broadcast_signal(&self, signo: i32, tasks: &TaskTable) {
        for pid in self.members_snapshot() {
            let task = tasks.find(pid);
            match task {
                Some(t) => {
                    if !t.done() {
                        t.send_sig(signo, self.pgid as isize);
                    }
                }
                None => { /* do nothing */ }
            }
        }
    }
}

// AGENT: generic wait queues store WaitToken instead of std::thread::Thread.
pub struct WaitEntry {
    pub key: usize,
    pub token: WaitToken,
    pub flags: u32,
}

pub struct WaitQueue {
    pub inner: Mutex<VecDeque<WaitEntry>>,
    pub wake_count: AtomicUsize,
}

impl WaitQueue {
    pub fn new() -> Self {
        Self {
            inner: Mutex::new(VecDeque::new()),
            wake_count: AtomicUsize::new(0),
        }
    }

    pub fn sleep(&self, key: usize, flags: u32) {
        let token = WaitToken::current();
        let mut q = self.inner.lock().unwrap();
        q.push_back(WaitEntry {
            key,
            token: token.clone(),
            flags,
        });
        drop(q);
        token.wait(None);
    }

    pub fn sleep_timeout(&self, key: usize, flags: u32, timeout: Duration) -> bool {
        let token = WaitToken::current();
        let mut q = self.inner.lock().unwrap();
        q.push_back(WaitEntry {
            key,
            token: token.clone(),
            flags,
        });
        drop(q);
        match token.wait(Some(timeout)) {
            WaitOutcome::Event => true,
            WaitOutcome::Timeout => {
                let mut q = self.inner.lock().unwrap();
                q.retain(|entry| !entry.token.same(&token));
                false
            }
        }
    }

    pub fn wake_one(&self, key: usize) -> bool {
        loop {
            let entry = {
                let mut q = self.inner.lock().unwrap();
                q.iter()
                    .position(|entry| entry.key == key)
                    .map(|pos| q.remove(pos).unwrap())
            };
            let Some(entry) = entry else {
                return false;
            };
            if entry.token.wake() {
                self.wake_count.fetch_add(1, Ordering::Relaxed);
                return true;
            }
        }
    }

    pub fn wake_all(&self, key: usize) -> usize {
        let mut q = self.inner.lock().unwrap();
        let mut count = 0;
        let mut remaining = VecDeque::new();
        for entry in q.drain(..) {
            if entry.key == key {
                if entry.token.wake() {
                    count += 1;
                }
            } else {
                remaining.push_back(entry);
            }
        }
        *q = remaining;
        self.wake_count.fetch_add(count, Ordering::Relaxed);
        count
    }

    pub fn wake_filtered(&self, pred: impl Fn(usize, u32) -> bool) -> usize {
        let mut q = self.inner.lock().unwrap();
        let mut count = 0;
        let mut remaining = VecDeque::new();
        for entry in q.drain(..) {
            if pred(entry.key, entry.flags) {
                if entry.token.wake() {
                    count += 1;
                }
            } else {
                remaining.push_back(entry);
            }
        }
        *q = remaining;
        self.wake_count.fetch_add(count, Ordering::Relaxed);
        count
    }

    pub fn pending_count(&self) -> usize {
        self.inner.lock().unwrap().len()
    }

    pub fn total_wakes(&self) -> usize {
        self.wake_count.load(Ordering::Relaxed)
    }

    pub fn has_waiters_for(&self, key: usize) -> bool {
        self.inner
            .lock()
            .unwrap()
            .iter()
            .any(|entry| entry.key == key)
    }

    pub fn reorder_by_priority(&self) {
        let mut q = self.inner.lock().unwrap();
        q.make_contiguous().sort_by(|a, b| {
            let a_prio = a.flags;
            let b_prio = b.flags;
            b_prio.cmp(&a_prio)
        });
        // q.sort_by(|a, b| a.2.cmp(&b.2));
    }
}
