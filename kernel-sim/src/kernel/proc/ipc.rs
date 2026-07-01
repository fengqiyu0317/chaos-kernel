// AGENT
use super::*;

#[repr(C)]
#[derive(Clone, Copy)]
pub struct IpcPerm {
    pub key: u32,
    pub uid: u32,
    pub gid: u32,
    pub cuid: u32,
    pub cgid: u32,
    pub mode: u32,
    pub seq: u32,
    pub pad1: usize,
    pub pad2: usize,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct SemDs {
    pub perm: IpcPerm,
    pub otime: usize,
    _p1: usize,
    pub ctime: usize,
    _p2: usize,
    pub nsems: usize,
}

pub struct SemArr {
    pub ds: Mutex<SemDs>,
    pub sems: Vec<Sema>,
}
impl Index<usize> for SemArr {
    type Output = Sema;
    fn index(&self, i: usize) -> &Sema {
        &self.sems[i]
    }
}
impl SemArr {
    pub fn remove(&self) {
        for s in &self.sems {
            s.remove();
        }
    }
    pub fn otime_now(&self) {
        self.ds.lock().unwrap().otime = 0;
    }
    pub fn ctime_now(&self) {
        self.ds.lock().unwrap().ctime = 0;
    }
    pub fn set_ds(&self, new: &SemDs) {
        let mut l = self.ds.lock().unwrap();
        l.perm.uid = new.perm.uid;
        l.perm.gid = new.perm.gid;
        l.perm.mode = new.perm.mode & 0x1ff;
    }
    pub fn get_or_create(
        key: u32,
        nsems: usize,
        flags: usize,
        store: &RwLock<BTreeMap<u32, Weak<SemArr>>>,
    ) -> Result<Arc<Self>, &'static str> {
        let mut m = store.write().unwrap();
        let mut k = key;
        if k == 0 {
            k = (1u32..).find(|i| m.get(i).is_none()).unwrap();
        } else if let Some(w) = m.get(&k) {
            if let Some(a) = w.upgrade() {
                if (flags & (1 << 9)) != 0 && (flags & (1 << 10)) != 0 {
                    return Err("eexist");
                }
                return Ok(a);
            }
        }
        let mut sv = Vec::new();
        for _ in 0..nsems {
            sv.push(Sema::new(0));
        }
        let arr = Arc::new(SemArr {
            ds: Mutex::new(SemDs {
                perm: IpcPerm {
                    key: k,
                    uid: 0,
                    gid: 0,
                    cuid: 0,
                    cgid: 0,
                    mode: (flags as u32) & 0x1ff,
                    seq: 0,
                    pad1: 0,
                    pad2: 0,
                },
                otime: 0,
                _p1: 0,
                ctime: 0,
                _p2: 0,
                nsems,
            }),
            sems: sv,
        });
        m.insert(k, Arc::downgrade(&arr));
        Ok(arr)
    }
}

type SemId = usize;
type SemNum = u16;
type SemOp = i16;

// AGENT: keep process-local semaphore handles and SEM_UNDO adjustments together.
#[derive(Default)]
pub struct SemCtx {
    pub arrays: BTreeMap<SemId, Arc<SemArr>>,
    pub undos: BTreeMap<(SemId, SemNum), SemOp>,
}
impl SemCtx {
    // AGENT: reuse the lowest free process-local semaphore id.
    fn next_id(&self) -> SemId {
        (0..).find(|i| self.arrays.get(i).is_none()).unwrap()
    }

    // AGENT: look up a semaphore without using Index so stale undo records cannot panic.
    fn sem(&self, id: SemId, num: SemNum) -> Option<&Sema> {
        self.arrays
            .get(&id)
            .and_then(|arr| arr.sems.get(num as usize))
    }

    // AGENT: apply one accumulated simplified SEM_UNDO adjustment.
    fn apply_undo(sem: &Sema, op: SemOp) {
        let steps = if op < 0 {
            (0isize - op as isize) as usize
        } else {
            op as usize
        };
        for _ in 0..steps {
            if op > 0 {
                sem.release();
            } else if sem.try_acquire() != Ok(true) {
                break;
            }
        }
    }

    // AGENT: allocate a process-local handle for a semaphore set.
    pub fn add(&mut self, arr: Arc<SemArr>) -> SemId {
        let id = self.next_id();
        self.arrays.insert(id, arr);
        id
    }

    // AGENT: dropping a local handle also drops any undo state tied to that reused id.
    pub fn remove(&mut self, id: SemId) {
        self.arrays.remove(&id);
        self.undos.retain(|&(undo_id, _), _| undo_id != id);
    }

    // AGENT: clone the Arc so callers can operate without holding the SemCtx lock.
    pub fn get(&self, id: SemId) -> Option<Arc<SemArr>> {
        self.arrays.get(&id).cloned()
    }

    // AGENT: record the inverse operation only for live semaphores.
    pub fn add_undo(&mut self, id: SemId, num: SemNum, op: SemOp) -> bool {
        if self.sem(id, num).is_none() {
            return false;
        }
        let key = (id, num);
        let old = *self.undos.get(&(id, num)).unwrap_or(&0);
        let next = old.saturating_sub(op);
        if next == 0 {
            self.undos.remove(&key);
        } else {
            self.undos.insert(key, next);
        }
        true
    }
}
// AGENT: fork-style copies inherit handles but not SEM_UNDO adjustments.
impl Clone for SemCtx {
    fn clone(&self) -> Self {
        SemCtx {
            arrays: self.arrays.clone(),
            undos: BTreeMap::new(),
        }
    }
}
// AGENT: process teardown applies any accumulated simplified SEM_UNDO adjustments.
impl Drop for SemCtx {
    fn drop(&mut self) {
        for (&(id, num), &op) in &self.undos {
            if let Some(sem) = self.sem(id, num) {
                Self::apply_undo(sem, op);
            }
        }
    }
}

type ShmId = usize;

#[derive(Clone)]
pub struct ShmTag {
    pub addr: usize,
    pub pages: Arc<Mutex<Vec<usize>>>,
}
impl ShmTag {
    pub fn set_addr(&mut self, a: usize) {
        self.addr = a;
    }
}

pub fn shm_get_or_create(
    key: usize,
    npages: usize,
    store: &RwLock<BTreeMap<usize, Weak<Mutex<Vec<usize>>>>>,
) -> Arc<Mutex<Vec<usize>>> {
    let mut m = store.write().unwrap();
    if let Some(w) = m.get(&key) {
        if let Some(g) = w.upgrade() {
            return g;
        }
    }
    let g = Arc::new(Mutex::new(vec![0usize; npages]));
    m.insert(key, Arc::downgrade(&g));
    g
}

#[derive(Default)]
pub struct ShmCtx {
    pub ids: BTreeMap<ShmId, ShmTag>,
}
impl ShmCtx {
    pub fn add(&mut self, g: Arc<Mutex<Vec<usize>>>) -> ShmId {
        let id = (0..).find(|i| !self.ids.contains_key(i)).unwrap();
        self.ids.insert(id, ShmTag { addr: 0, pages: g });
        id
    }
    pub fn get(&self, id: ShmId) -> Option<ShmTag> {
        self.ids.get(&id).cloned()
    }
    pub fn set(&mut self, id: ShmId, tag: ShmTag) {
        self.ids.insert(id, tag);
    }
    pub fn get_id_by_addr(&self, addr: usize) -> Option<ShmId> {
        self.ids
            .iter()
            .find(|(_, v)| v.addr == addr)
            .map(|(k, _)| *k)
    }
    pub fn pop(&mut self, id: ShmId) {
        self.ids.remove(&id);
    }
}
impl Clone for ShmCtx {
    fn clone(&self) -> Self {
        ShmCtx {
            ids: self.ids.clone(),
        }
    }
}
