// AGENT
use super::*;

const IPC_PRIVATE_KEY: u32 = 0;
const IPC_CREAT: usize = 0o1000;
const IPC_EXCL: usize = 0o2000;
const IPC_MODE_MASK: u32 = 0o777;

// AGENT: keep only semaphore-set metadata that current kernel-qemu logic uses.
#[derive(Clone, Copy)]
pub struct SemDs {
    pub key: u32,
    pub mode: u32,
    pub nsems: usize,
}

// AGENT: model one semaphore set as metadata plus its contained semaphores.
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

    fn prune_dead_sets(store: &mut BTreeMap<u32, Weak<SemArr>>) {
        store.retain(|_, sems| sems.strong_count() > 0);
    }

    fn next_private_key(store: &BTreeMap<u32, Weak<SemArr>>) -> u32 {
        (1u32..)
            .find(|candidate| !store.contains_key(candidate))
            .unwrap()
    }

    fn new_set(key: u32, nsems: usize, flags: usize) -> Arc<Self> {
        let mut sems = Vec::with_capacity(nsems);
        for _ in 0..nsems {
            sems.push(Sema::new(0));
        }

        Arc::new(SemArr {
            ds: Mutex::new(SemDs {
                key,
                mode: (flags as u32) & IPC_MODE_MASK,
                nsems,
            }),
            sems,
        })
    }

    // AGENT: create or reuse a semaphore set using only key, mode, and set size metadata.
    pub fn get_or_create(
        key: u32,
        nsems: usize,
        flags: usize,
        store: &RwLock<BTreeMap<u32, Weak<SemArr>>>,
    ) -> Result<Arc<Self>, &'static str> {
        let mut sets = store.write().unwrap();
        Self::prune_dead_sets(&mut sets);

        let creating_private = key == IPC_PRIVATE_KEY;
        let wants_create = (flags & IPC_CREAT) != 0;
        let wants_exclusive = (flags & IPC_EXCL) != 0;

        if !creating_private {
            if let Some(existing) = sets.get(&key).and_then(Weak::upgrade) {
                let existing_nsems = existing.ds.lock().unwrap().nsems;
                if wants_create && wants_exclusive {
                    return Err("eexist");
                }
                if nsems > existing_nsems {
                    return Err("einval");
                }
                return Ok(existing);
            }

            if !wants_create {
                return Err("enoent");
            }
        }

        if nsems == 0 {
            return Err("einval");
        }

        let stored_key = if creating_private {
            Self::next_private_key(&sets)
        } else {
            key
        };
        let arr = Self::new_set(stored_key, nsems, flags);
        sets.insert(stored_key, Arc::downgrade(&arr));
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
        (0..).find(|i| !self.arrays.contains_key(i)).unwrap()
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

// AGENT: keep System V shared-memory backing as real shared physical pages.
pub struct ShmSegment {
    pages: Vec<SharedPage>,
}

impl ShmSegment {
    pub fn new(npages: usize, pool: &FramePool) -> Result<Arc<Self>, &'static str> {
        if npages == 0 {
            return Err("einval");
        }

        let mut pages = Vec::with_capacity(npages);
        for _ in 0..npages {
            let frame = pool.alloc_pg_frame().ok_or("enomem")?;
            zero_page(frame.paddr());
            pages.push(SharedPage::new(frame));
        }
        Ok(Arc::new(Self { pages }))
    }

    pub fn page_count(&self) -> usize {
        self.pages.len()
    }

    pub fn pages(&self) -> &[SharedPage] {
        &self.pages
    }
}

#[derive(Clone)]
pub struct ShmTag {
    pub addr: usize,
    pub segment: Arc<ShmSegment>,
}
impl ShmTag {
    pub fn set_addr(&mut self, a: usize) {
        self.addr = a;
    }
}

pub fn shm_get_or_create(
    key: usize,
    npages: usize,
    pool: &FramePool,
    store: &RwLock<BTreeMap<usize, Weak<ShmSegment>>>,
) -> Result<Arc<ShmSegment>, &'static str> {
    let mut m = store.write().unwrap();
    if let Some(w) = m.get(&key) {
        if let Some(segment) = w.upgrade() {
            if npages > segment.page_count() {
                return Err("einval");
            }
            return Ok(segment);
        }
    }
    let segment = ShmSegment::new(npages, pool)?;
    m.insert(key, Arc::downgrade(&segment));
    Ok(segment)
}

#[derive(Default)]
pub struct ShmCtx {
    pub ids: BTreeMap<ShmId, ShmTag>,
}
impl ShmCtx {
    pub fn add(&mut self, segment: Arc<ShmSegment>) -> ShmId {
        let id = (0..).find(|i| !self.ids.contains_key(i)).unwrap();
        self.ids.insert(id, ShmTag { addr: 0, segment });
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
