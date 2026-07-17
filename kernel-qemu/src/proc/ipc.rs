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
