// AGENT: Route the external chaos-tests harness to kernel-sim while preserving
// the small legacy API surface used by the existing basic tests.
pub use kernel_sim::{
    check_access, Channel, CircBuf, Context, Disk, FramePool, MountTable, PgFrame, SharedPage,
    Spin, SyncQueue, TrapCtl, KERN_BASE, N_REGS, RBUF_CAP,
};

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, RwLock};

// AGENT: legacy chaos-tests call GKL.leave() without an owner id; delegate to
// kernel-sim's owner-checked GKL using its current recorded owner.
pub struct LegacyGkl;

pub static GKL: LegacyGkl = LegacyGkl;

impl LegacyGkl {
    pub fn enter(&self, id: usize) {
        kernel_sim::GKL.enter(id);
    }

    pub fn leave(&self) {
        kernel_sim::GKL.leave(kernel_sim::GKL.owner());
    }

    pub fn held(&self) -> bool {
        kernel_sim::GKL.held()
    }

    pub fn owner(&self) -> usize {
        kernel_sim::GKL.owner()
    }

    pub fn level(&self) -> usize {
        kernel_sim::GKL.level()
    }
}

#[derive(Clone, Debug)]
pub struct TaskInfo {
    pub id: usize,
    pub tag: String,
    pub status: Option<usize>,
}

// AGENT: compatibility wrapper that keeps the old public fields used by
// chaos-tests while carrying the real kernel-sim task internally.
pub struct Task {
    inner: Arc<kernel_sim::Task>,
    pub info: Mutex<TaskInfo>,
    pub parent: Mutex<Option<Arc<Task>>>,
}

impl Task {
    pub fn make(id: usize, tag: &str) -> Arc<Self> {
        Self::wrap(kernel_sim::Task::make(id, tag), None)
    }

    fn wrap(inner: Arc<kernel_sim::Task>, parent: Option<Arc<Task>>) -> Arc<Self> {
        Arc::new(Self {
            info: Mutex::new(TaskInfo {
                id: inner.id(),
                tag: inner.tag(),
                status: None,
            }),
            inner,
            parent: Mutex::new(parent),
        })
    }

    pub fn id(&self) -> usize {
        self.info.lock().unwrap().id
    }

    fn mark_reaped(&self) {
        self.info.lock().unwrap().status = Some(0);
    }
}

// AGENT: bridge the legacy infallible fork_task API to kernel-sim's fallible
// fork implementation without changing the existing basic tests.
pub struct TaskTable {
    inner: kernel_sim::TaskTable,
    map: RwLock<BTreeMap<usize, Arc<Task>>>,
    pub root: Mutex<Option<Arc<Task>>>,
}

impl TaskTable {
    pub fn new() -> Self {
        Self {
            inner: kernel_sim::TaskTable::new(),
            map: RwLock::new(BTreeMap::new()),
            root: Mutex::new(None),
        }
    }

    pub fn spawn(&self, tag: &str) -> Arc<Task> {
        let task = Task::wrap(self.inner.spawn(tag), None);
        self.map.write().unwrap().insert(task.id(), task.clone());
        task
    }

    pub fn spawn_root(&self) -> Arc<Task> {
        let task = Task::wrap(self.inner.spawn_root(), None);
        self.map.write().unwrap().insert(task.id(), task.clone());
        *self.root.lock().unwrap() = Some(task.clone());
        task
    }

    pub fn fork_task(&self, src: &Arc<Task>) -> Arc<Task> {
        let child_inner = self
            .inner
            .fork_task(&src.inner)
            .expect("kernel-sim fork_task should succeed for basic tests");
        let child = Task::wrap(child_inner, Some(src.clone()));
        self.map.write().unwrap().insert(child.id(), child.clone());
        child
    }

    pub fn find(&self, id: usize) -> Option<Arc<Task>> {
        self.map.read().unwrap().get(&id).cloned()
    }

    pub fn reap(&self, id: usize) {
        if let Some(task) = self.map.write().unwrap().remove(&id) {
            task.mark_reaped();
        }
        self.inner.reap(id);
    }

    pub fn count(&self) -> usize {
        self.map.read().unwrap().len()
    }
}

// AGENT: expose just the legacy Kernel fields used by basic chaos-tests while
// keeping FramePool behavior from kernel-sim.
pub struct Kernel {
    pub tasks: TaskTable,
    pub pool: FramePool,
}

impl Kernel {
    pub fn new(nf: usize) -> Self {
        Self {
            tasks: TaskTable::new(),
            pool: FramePool::new(nf),
        }
    }

    pub fn proc_init(&self) {
        self.tasks.spawn_root();
    }
}
