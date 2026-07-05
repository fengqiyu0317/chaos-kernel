// AGENT
use super::*;

// AGENT: keep Kernel as the shared simulator state container.
pub struct Kernel {
    pub tasks: TaskTable,
    pub run_queue: RunQueue,
    pub cache: BlockCache,
    pub pool: FramePool,
    pub cpus: Mutex<[Option<Arc<Task>>; MAX_CPU]>,
    pub mnt: MountTable,
    // AGENT: handle to the simulator-wide timer wheel driven from CPU0 ticks.
    pub timers: &'static Mutex<TimerWheel>,
    // AGENT: unified path-backed file table shared by open-like handles and exec.
    pub file_nodes: RwLock<BTreeMap<String, Arc<FileNode>>>,
    pub sem_store: RwLock<BTreeMap<u32, Weak<SemArr>>>,
    pub shm_store: RwLock<BTreeMap<usize, Weak<Mutex<Vec<usize>>>>>,
    pub tty_buf: Mutex<VecDeque<u8>>,
}
impl Kernel {
    // AGENT: construct shared kernel state; behavior methods live under kernel_ops/.
    pub fn new(nf: usize) -> Self {
        Self {
            tasks: TaskTable::new(),
            run_queue: RunQueue::new(),
            cache: BlockCache::new(N_CHAINS),
            pool: FramePool::new(nf),
            cpus: Mutex::new([None, None, None, None, None, None, None, None]),
            mnt: MountTable::new(),
            timers: global_timer_wheel(),
            file_nodes: RwLock::new(BTreeMap::new()),
            sem_store: RwLock::new(BTreeMap::new()),
            shm_store: RwLock::new(BTreeMap::new()),
            tty_buf: Mutex::new(VecDeque::new()),
        }
    }
}

// AGENT: root compatibility kernel moved from src/lib.rs; it exposes only the
// fields used by basic chaos-tests and delegates frame allocation to FramePool.
pub struct LegacyKernel {
    pub tasks: LegacyTaskTable,
    pub pool: FramePool,
}

// AGENT: keep the legacy constructor and proc_init shape.
impl LegacyKernel {
    pub fn new(nf: usize) -> Self {
        Self {
            tasks: LegacyTaskTable::new(),
            pool: FramePool::new(nf),
        }
    }

    pub fn proc_init(&self) {
        self.tasks.spawn_root();
    }
}
