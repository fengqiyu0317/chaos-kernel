// AGENT
use super::*;
use core::sync::atomic::AtomicPtr;

// AGENT: focused selftests construct several Kernel values on the early QEMU
// heap, so they use a tiny cache while ordinary boot keeps N_CHAINS.
const TEST_KERNEL_BLOCK_CACHE_CHAINS: usize = 1;

// AGENT: optional boot selftest builds run many heap-allocating checks before
// constructing the real Kernel, so keep their post-test backend small too.
fn boot_kernel_block_cache_chains() -> usize {
    #[cfg(any(
        test,
        feature = "qemu-mm-selftest",
        feature = "qemu-fs-selftest",
        feature = "qemu-sync-selftest",
        feature = "qemu-sched-selftest",
        feature = "qemu-proc-selftest",
        feature = "qemu-checkpoint-selftest"
    ))]
    {
        TEST_KERNEL_BLOCK_CACHE_CHAINS
    }
    #[cfg(not(any(
        test,
        feature = "qemu-mm-selftest",
        feature = "qemu-fs-selftest",
        feature = "qemu-sync-selftest",
        feature = "qemu-sched-selftest",
        feature = "qemu-proc-selftest",
        feature = "qemu-checkpoint-selftest"
    )))]
    {
        N_CHAINS
    }
}

// AGENT: keep Kernel as the shared simulator state container and own filesystem
// state only through the Vfs -> root Mount -> root FsInstance object chain.
pub struct Kernel {
    pub tasks: TaskTable,
    pub run_queue: RunQueue,
    pub pool: FramePool,
    // AGENT: keep current-task and idle-context ownership together per hart;
    // only processors[0] is allowed to enter the scheduler in this milestone.
    pub processors: [Mutex<Processor>; MAX_CPU],
    // AGENT: make the object VFS the sole owner of root storage, mounts, and
    // filesystem-local node namespaces.
    pub vfs: Vfs,
    pub sem_store: RwLock<BTreeMap<u32, Weak<SemArr>>>,
    pub shm_store: RwLock<BTreeMap<usize, Weak<ShmSegment>>>,
    pub tty_buf: Mutex<VecDeque<u8>>,
}

// AGENT: publish the one boot-lifetime Kernel for architecture, syscall, timer,
// and wait entry points whose fixed ABI cannot carry an explicit reference.
// Focused selftests may replace it only with another leaked Kernel.
static KERNEL: AtomicPtr<Kernel> = AtomicPtr::new(ptr::null_mut());

// AGENT: install a Kernel whose leaked/static lifetime makes lock-free global
// lookup safe for every later trap and scheduler entry.
pub fn install_kernel(kernel: &'static Kernel) {
    KERNEL.store(kernel as *const Kernel as *mut Kernel, Ordering::Release);
}

// AGENT: return the installed global Kernel without tying general runtime
// lookup to the synchronization subsystem.
pub fn global_kernel() -> Option<&'static Kernel> {
    let kernel = KERNEL.load(Ordering::Acquire);
    if kernel.is_null() {
        None
    } else {
        // SAFETY: install_kernel accepts only leaked/static Kernel references.
        Some(unsafe { &*kernel })
    }
}

// AGENT: isolate test-only global reset instead of exposing KERNEL mutation to
// synchronization tests directly.
#[cfg(any(test, feature = "qemu-sync-selftest"))]
pub(crate) fn clear_global_kernel_for_test() {
    KERNEL.store(ptr::null_mut(), Ordering::Release);
}

impl Kernel {
    // AGENT: construct shared kernel state around a caller-initialized frame
    // pool so QEMU boot can seed only linker/RAM-derived free pages.
    pub fn new(pool: FramePool) -> Self {
        Self::new_with_block_device_and_cache_chains(
            pool,
            Arc::new(RamBlockDevice::empty()),
            TEST_KERNEL_BLOCK_CACHE_CHAINS,
        )
    }

    // AGENT: allow QEMU boot to inject a concrete block backend while preserving
    // the default Kernel construction path used by focused selftests.
    pub fn new_with_block_device(pool: FramePool, block_device: Arc<dyn BlockDevice>) -> Self {
        Self::new_with_block_device_and_cache_chains(
            pool,
            block_device,
            boot_kernel_block_cache_chains(),
        )
    }

    // AGENT: centralize Kernel construction so tests can shrink only cache
    // residency while production boot keeps the normal cache width.
    fn new_with_block_device_and_cache_chains(
        pool: FramePool,
        block_device: Arc<dyn BlockDevice>,
        cache_chains: usize,
    ) -> Self {
        let file_blocks = Arc::new(FileBlockAllocator::new(block_device.block_count()));
        let file_storage = FileStorage::new(
            Arc::new(BlockCache::new(cache_chains)),
            block_device,
            file_blocks,
        );
        let root_fs = FsInstance::new(ROOT_FS_ID, file_storage);
        let vfs = Vfs::new(root_fs.clone());
        // AGENT: Publish the already-live root filesystem under an explicit
        // runtime source name without pretending that a /dev node exists.
        vfs.register_source("rootfs", root_fs)
            .expect("root filesystem source should register");
        let tasks = TaskTable::new(pool.clone());
        Self {
            tasks,
            run_queue: RunQueue::new(),
            pool,
            processors: core::array::from_fn(|_| Mutex::new(Processor::new())),
            vfs,
            sem_store: RwLock::new(BTreeMap::new()),
            shm_store: RwLock::new(BTreeMap::new()),
            tty_buf: Mutex::new(VecDeque::new()),
        }
    }
}
