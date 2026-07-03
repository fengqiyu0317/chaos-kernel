// AGENT
use super::*;

// AGENT: keep Kernel as the shared simulator state container and own the first
// QEMU block backend used by the migrated BlockCache path.
pub struct Kernel {
    pub tasks: TaskTable,
    pub run_queue: RunQueue,
    pub cache: Arc<BlockCache>,
    pub block_device: Arc<RamBlockDevice>,
    file_blocks: Arc<FileBlockAllocator>,
    pub pool: FramePool,
    pub cpus: Mutex<[Option<Arc<Task>>; MAX_CPU]>,
    pub mnt: MountTable,
    // AGENT: handle to the QEMU timer wheel driven from real timer interrupts.
    pub timers: &'static Mutex<TimerWheel>,
    // AGENT: unified path-backed file table shared by open-like handles and exec.
    pub file_nodes: RwLock<BTreeMap<String, Arc<FileNode>>>,
    pub sem_store: RwLock<BTreeMap<u32, Weak<SemArr>>>,
    pub shm_store: RwLock<BTreeMap<usize, Weak<ShmSegment>>>,
    pub tty_buf: Mutex<VecDeque<u8>>,
}
impl Kernel {
    // AGENT: construct shared kernel state around a caller-initialized frame
    // pool so QEMU boot can seed only linker/RAM-derived free pages.
    pub fn new(pool: FramePool) -> Self {
        Self::new_with_block_device(pool, Arc::new(RamBlockDevice::empty()))
    }

    // AGENT: allow QEMU boot to inject a concrete block backend while preserving
    // the default Kernel construction path used by focused selftests.
    pub fn new_with_block_device(pool: FramePool, block_device: Arc<RamBlockDevice>) -> Self {
        Self {
            tasks: TaskTable::new(),
            run_queue: RunQueue::new(),
            cache: Arc::new(BlockCache::new(N_CHAINS)),
            block_device,
            file_blocks: Arc::new(FileBlockAllocator::new()),
            pool,
            cpus: Mutex::new([None, None, None, None, None, None, None, None]),
            mnt: MountTable::new(),
            timers: global_timer_wheel(),
            file_nodes: RwLock::new(BTreeMap::new()),
            sem_store: RwLock::new(BTreeMap::new()),
            shm_store: RwLock::new(BTreeMap::new()),
            tty_buf: Mutex::new(VecDeque::new()),
        }
    }

    // AGENT: build the shared FileNode backend from the Kernel-owned RAM block
    // device and cache instead of storing file bytes inside each FileNode.
    pub fn file_storage(&self) -> FileStorage {
        FileStorage::new(
            self.cache.clone(),
            self.block_device.clone(),
            self.file_blocks.clone(),
        )
    }
}
