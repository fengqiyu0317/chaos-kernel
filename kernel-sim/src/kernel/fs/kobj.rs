// AGENT
use super::*;

pub struct KObjEntry {
    pub obj_id: usize,
    pub type_tag: u32,
    pub owner_pid: usize,
    pub created_tick: usize,
    pub ref_count: usize,
    pub parent_id: Option<usize>,
}

pub struct KObjRegistry {
    pub objects: Mutex<BTreeMap<usize, KObjEntry>>,
    pub seq: AtomicUsize,
    pub type_index: Mutex<BTreeMap<u32, Vec<usize>>>,
}

impl KObjRegistry {
    pub fn new() -> Self {
        Self {
            objects: Mutex::new(BTreeMap::new()),
            seq: AtomicUsize::new(1),
            type_index: Mutex::new(BTreeMap::new()),
        }
    }

    pub fn register(&self, type_tag: u32, owner_pid: usize) -> usize {
        let id = self.seq.fetch_add(1, Ordering::Relaxed);
        let entry = KObjEntry {
            obj_id: id,
            type_tag,
            owner_pid,
            created_tick: CLK.load(Ordering::Relaxed),
            ref_count: 1,
            parent_id: None,
        };
        self.objects.lock().unwrap().insert(id, entry);
        let mut idx = self.type_index.lock().unwrap();
        idx.entry(type_tag).or_insert_with(Vec::new).push(id);
        id
    }

    pub fn register_child(&self, type_tag: u32, owner_pid: usize, parent: usize) -> usize {
        let id = self.seq.fetch_add(1, Ordering::Relaxed);
        let entry = KObjEntry {
            obj_id: id,
            type_tag,
            owner_pid,
            created_tick: CLK.load(Ordering::Relaxed),
            ref_count: 1,
            parent_id: Some(parent),
        };
        self.objects.lock().unwrap().insert(id, entry);
        let mut idx = self.type_index.lock().unwrap();
        idx.entry(type_tag).or_insert_with(Vec::new).push(id);
        id
    }

    pub fn unregister(&self, id: usize) -> bool {
        let removed = self.objects.lock().unwrap().remove(&id);
        if let Some(entry) = removed {
            let mut idx = self.type_index.lock().unwrap();
            if let Some(list) = idx.get_mut(&entry.type_tag) {
                list.retain(|&x| x != id);
            }
            true
        } else {
            false
        }
    }

    pub fn find_by_type(&self, tag: u32) -> Vec<usize> {
        self.type_index
            .lock()
            .unwrap()
            .get(&tag)
            .cloned()
            .unwrap_or_default()
    }

    pub fn dump_graph(&self) -> Vec<(usize, usize)> {
        let objs = self.objects.lock().unwrap();
        let mut edges = Vec::new();
        for (id, entry) in objs.iter() {
            if let Some(parent) = entry.parent_id {
                edges.push((parent, *id));
            }
        }
        edges
    }

    pub fn gc_sweep(&self) -> usize {
        let mut objs = self.objects.lock().unwrap();
        let dead: Vec<usize> = objs
            .iter()
            .filter(|(_, e)| e.ref_count == 0)
            .map(|(id, _)| *id)
            .collect();
        let count = dead.len();
        for id in dead {
            // HUMAN
            self.unregister(id);
        }
        count
    }

    pub fn ref_up(&self, id: usize) -> bool {
        let mut objs = self.objects.lock().unwrap();
        if let Some(e) = objs.get_mut(&id) {
            e.ref_count += 1;
            true
        } else {
            false
        }
    }

    pub fn ref_down(&self, id: usize) -> bool {
        let mut objs = self.objects.lock().unwrap();
        if let Some(e) = objs.get_mut(&id) {
            if e.ref_count > 0 {
                e.ref_count = e.ref_count.saturating_sub(1);
            }
            true
        } else {
            false
        }
    }

    pub fn count(&self) -> usize {
        self.objects.lock().unwrap().len()
    }

    pub fn owner_objects(&self, pid: usize) -> Vec<usize> {
        self.objects
            .lock()
            .unwrap()
            .iter()
            .filter(|(_, e)| e.owner_pid == pid)
            .map(|(id, _)| *id)
            .collect()
    }
}
