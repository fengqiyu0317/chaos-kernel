// AGENT: QEMU eager anonymous/file mmap regressions that require an installed
// Kernel, current task, real frame pool, Sv39, filesystem, and RV64 adapter.
use super::*;
use crate::syscall_abi::{
    decode_from_trap_frame, dispatch_from_trap_frame, map_riscv_nr, INTERNAL_SYS_MMAP,
    INTERNAL_SYS_MUNMAP, RISCV_SYS_MMAP, RISCV_SYS_MUNMAP,
};

const HINT_BASE: usize = 0x7100_0001;
const HINT_ALIGNED: usize = 0x7100_1000;
const FIXED_BASE: usize = 0x7200_0000;
const FILE_READ_BASE: usize = 0x7300_0000;
const FILE_PRIVATE_BASE: usize = 0x7310_0000;
const FILE_SHARED_BASE: usize = 0x7320_0000;
const FILE_FORK_BASE: usize = 0x7330_0000;
const FILE_PARTIAL_BASE: usize = 0x7340_0000;
const FILE_REPLACE_BASE: usize = 0x7350_0000;
const FILE_FAILURE_BASE: usize = 0x7360_0000;

// AGENT: run anonymous ABI checks on the shared selftest kernel, then isolate
// file fixtures in disposable kernels so later fs/checkpoint tests stay clean.
pub fn run_all(kernel: &Kernel) {
    rv64_mmap_and_munmap_round_trip(kernel);
    mmap_rejects_invalid_types_and_reserved_signal_page(kernel);
    mmap_honors_hint_conflicts_and_default_fallback(kernel);
    fixed_mmap_replaces_contents_and_permissions(kernel);
    file_mmap_contracts(kernel.pool.clone());
    file_mmap_failure_transactions(kernel.pool.clone());
}

// AGENT: prove that RV64 syscall numbers and all six argument slots reach the
// installed semantic entry, then observe zero-fill, usercopy, and frame release.
fn rv64_mmap_and_munmap_round_trip(kernel: &Kernel) {
    assert_eq!(map_riscv_nr(RISCV_SYS_MMAP), Some(INTERNAL_SYS_MMAP));
    assert_eq!(map_riscv_nr(RISCV_SYS_MUNMAP), Some(INTERNAL_SYS_MUNMAP));

    let args = [
        0,
        PAGE_SZ,
        PROT_READ | PROT_WRITE,
        MAP_PRIVATE | MAP_ANONYMOUS,
        usize::MAX,
        0,
    ];
    let mut frame = TrapFrame::new();
    frame.regs[10..16].copy_from_slice(&args);
    frame.regs[17] = RISCV_SYS_MMAP;
    let request = decode_from_trap_frame(&frame);
    assert_eq!(request.internal_nr, Some(INTERNAL_SYS_MMAP));
    assert_eq!(request.args, args);
    dispatch_from_trap_frame(&mut frame);
    let mapped = frame.regs[10];
    assert_eq!(mapped, 0x7000_0000);

    let task = kernel
        .cur_task(0)
        .expect("mmap selftest needs current init");
    let mut zeros = [0xffu8; 16];
    task.process
        .addr_space
        .lock()
        .unwrap()
        .read_user_bytes(mapped, &mut zeros)
        .unwrap();
    assert_eq!(zeros, [0u8; 16]);
    task.process
        .addr_space
        .lock()
        .unwrap()
        .write_user_bytes(mapped, b"mmap", &kernel.pool)
        .unwrap();

    let free_while_mapped = kernel.pool.free_count();
    let mut unmap = TrapFrame::new();
    unmap.regs[10] = mapped;
    unmap.regs[11] = PAGE_SZ;
    unmap.regs[17] = RISCV_SYS_MUNMAP;
    dispatch_from_trap_frame(&mut unmap);
    assert_eq!(unmap.regs[10], 0);
    assert!(task
        .process
        .addr_space
        .lock()
        .unwrap()
        .mapped_region(mapped)
        .is_none());
    assert!(kernel.pool.free_count() >= free_while_mapped + 1);
}

// AGENT: enforce exactly one mapping type and keep the kernel-owned rt_sigreturn
// page outside both fixed mmap replacement and user munmap ranges.
fn mmap_rejects_invalid_types_and_reserved_signal_page(kernel: &Kernel) {
    let prot = PROT_READ | PROT_WRITE;
    let anon = MAP_ANONYMOUS;
    assert_eq!(
        sys_mmap(kernel, 0, PAGE_SZ, prot, anon, usize::MAX, 0),
        Err("einval")
    );
    assert_eq!(
        sys_mmap(
            kernel,
            0,
            PAGE_SZ,
            prot,
            anon | MAP_PRIVATE | MAP_SHARED,
            usize::MAX,
            0,
        ),
        Err("einval")
    );
    assert_eq!(
        sys_mmap(kernel, 0, PAGE_SZ, prot, MAP_PRIVATE, 0, 0),
        Err("ebadf")
    );
    assert_eq!(
        sys_mmap(
            kernel,
            USER_SIGTRAMP,
            PAGE_SZ,
            prot,
            MAP_FIXED | MAP_PRIVATE | anon,
            usize::MAX,
            0,
        ),
        Err("enomem")
    );
    assert_eq!(sys_munmap(kernel, USER_SIGTRAMP, PAGE_SZ), Err("enomem"));
}

// AGENT: use an unaligned hint when free, advance past its conflict, and fall
// back to the default base when a top-of-user-space hint cannot fit one page.
fn mmap_honors_hint_conflicts_and_default_fallback(kernel: &Kernel) {
    let flags = MAP_PRIVATE | MAP_ANONYMOUS;
    let first = sys_mmap(kernel, HINT_BASE, PAGE_SZ, PROT_READ, flags, usize::MAX, 0).unwrap();
    assert_eq!(first, HINT_ALIGNED);
    let second = sys_mmap(kernel, HINT_BASE, PAGE_SZ, PROT_READ, flags, usize::MAX, 0).unwrap();
    assert_eq!(second, HINT_ALIGNED + PAGE_SZ);
    assert_eq!(sys_munmap(kernel, first, 2 * PAGE_SZ), Ok(0));

    let fallback = sys_mmap(
        kernel,
        USER_SIGTRAMP - PAGE_SZ + 1,
        PAGE_SZ,
        PROT_READ,
        flags,
        usize::MAX,
        0,
    )
    .unwrap();
    assert_eq!(fallback, 0x7000_0000);
    assert_eq!(sys_munmap(kernel, fallback, PAGE_SZ), Ok(0));
}

// AGENT: successful MAP_FIXED replacement must discard old bytes, install the
// new VMA permissions, and remain removable through the ordinary syscall path.
fn fixed_mmap_replaces_contents_and_permissions(kernel: &Kernel) {
    let rw_flags = MAP_FIXED | MAP_PRIVATE | MAP_ANONYMOUS;
    assert_eq!(
        sys_mmap(
            kernel,
            FIXED_BASE,
            PAGE_SZ,
            PROT_READ | PROT_WRITE,
            rw_flags,
            usize::MAX,
            0,
        ),
        Ok(FIXED_BASE)
    );
    let task = kernel.cur_task(0).expect("fixed mmap needs current init");
    task.process
        .addr_space
        .lock()
        .unwrap()
        .write_user_bytes(FIXED_BASE, &[0xa5], &kernel.pool)
        .unwrap();

    assert_eq!(
        sys_mmap(
            kernel,
            FIXED_BASE,
            PAGE_SZ,
            PROT_READ,
            rw_flags,
            usize::MAX,
            0,
        ),
        Ok(FIXED_BASE)
    );
    let mut value = [0xffu8; 1];
    let mut addr_space = task.process.addr_space.lock().unwrap();
    addr_space.read_user_bytes(FIXED_BASE, &mut value).unwrap();
    assert_eq!(value, [0]);
    assert_eq!(
        addr_space.write_user_bytes(FIXED_BASE, &[1], &kernel.pool),
        Err("efault")
    );
    drop(addr_space);
    assert_eq!(sys_munmap(kernel, FIXED_BASE, PAGE_SZ), Ok(0));
}

// AGENT: construct one disposable filesystem/process environment and cover
// positioned loading, access policy, private/shared behavior, fork, and split.
fn file_mmap_contracts(pool: FramePool) {
    let kernel = Kernel::new(pool);
    kernel.proc_init();
    file_mmap_loads_positionally_and_validates(&kernel);
    private_and_shared_file_mmap_semantics(&kernel);
    shared_file_fork_split_and_checkpoint(&kernel);
    fixed_file_mmap_writes_back_before_replacement(&kernel);
}

// AGENT: read one complete fixture through stable file identity without
// creating an OFD whose mutable offset could obscure mmap's positioned I/O.
fn read_fixture(kernel: &Kernel, path: &str, len: usize) -> Vec<u8> {
    let resolved = kernel.lookup_file_node(path).unwrap();
    let mut bytes = vec![0u8; len];
    let read = resolved.path_ref.read_at(0, &mut bytes).unwrap();
    bytes.truncate(read);
    bytes
}

// AGENT: prove cross-page/EOF eager reads preserve zero tails and OFD position,
// then exercise fd, type, offset-overflow, alignment, and access-mode errors.
fn file_mmap_loads_positionally_and_validates(kernel: &Kernel) {
    let path = "/mmap-positioned";
    let mut original = vec![0u8; PAGE_SZ + 31];
    for (idx, byte) in original.iter_mut().enumerate() {
        *byte = (idx % 251) as u8;
    }
    kernel.install_file(path, original.clone(), false).unwrap();
    let task = kernel.cur_task(0).unwrap();
    let fd = do_open(kernel, &task, path, 0, 0).unwrap();
    let entry = task.get_fd_entry(fd).unwrap();
    assert_eq!(entry.seek(FSeek::Start(37)), Ok(37));

    assert_eq!(
        sys_mmap(
            kernel,
            FILE_READ_BASE,
            2 * PAGE_SZ,
            PROT_READ,
            MAP_FIXED | MAP_PRIVATE,
            fd,
            0,
        ),
        Ok(FILE_READ_BASE)
    );
    let mut cross_page = [0u8; 48];
    let mut eof_tail = [0xffu8; 16];
    {
        let addr_space = task.process.addr_space.lock().unwrap();
        addr_space
            .read_user_bytes(FILE_READ_BASE + PAGE_SZ - 8, &mut cross_page)
            .unwrap();
        addr_space
            .read_user_bytes(FILE_READ_BASE + PAGE_SZ + 31, &mut eof_tail)
            .unwrap();
    }
    assert_eq!(&cross_page[..39], &original[PAGE_SZ - 8..]);
    assert!(cross_page[39..].iter().all(|&byte| byte == 0));
    assert!(eof_tail.iter().all(|&byte| byte == 0));
    assert_eq!(entry.offset(), 37);
    assert_eq!(sys_munmap(kernel, FILE_READ_BASE, 2 * PAGE_SZ), Ok(0));

    assert_eq!(
        sys_mmap(kernel, 0, PAGE_SZ, PROT_READ, MAP_PRIVATE, 200, 0),
        Err("ebadf")
    );
    assert_eq!(
        sys_mmap(kernel, 0, PAGE_SZ, PROT_READ, MAP_PRIVATE, fd, 1),
        Err("einval")
    );
    assert_eq!(
        sys_mmap(kernel, 0, PAGE_SZ, PROT_READ, MAP_PRIVATE, fd, usize::MAX,),
        Err("einval")
    );
    let largest_aligned_off = (i64::MAX as usize) & !(PAGE_SZ - 1);
    assert_eq!(
        sys_mmap(
            kernel,
            0,
            PAGE_SZ,
            PROT_READ,
            MAP_PRIVATE,
            fd,
            largest_aligned_off,
        ),
        Err("eoverflow")
    );

    let (pipe_reader, pipe_writer) = PipeNode::pair();
    let pipe_fd = task.add_file(FLike::Pipe(pipe_reader)).unwrap();
    assert_eq!(
        sys_mmap(kernel, 0, PAGE_SZ, PROT_READ, MAP_PRIVATE, pipe_fd, 0),
        Err("enodev")
    );
    drop(pipe_writer);
    kernel.close_task_fd(&task, pipe_fd).unwrap();

    let write_only = do_open(kernel, &task, path, 1, 0).unwrap();
    assert_eq!(
        sys_mmap(kernel, 0, PAGE_SZ, PROT_READ, MAP_PRIVATE, write_only, 0,),
        Err("eacces")
    );
    assert_eq!(
        sys_mmap(
            kernel,
            0,
            PAGE_SZ,
            PROT_READ | PROT_WRITE,
            MAP_SHARED,
            fd,
            0,
        ),
        Err("eacces")
    );
    kernel.close_task_fd(&task, write_only).unwrap();
    kernel.close_task_fd(&task, fd).unwrap();
}

// AGENT: distinguish private COW-style changes from shared sticky writeback,
// including the mount-pinned lifetime after the creating descriptor is closed.
fn private_and_shared_file_mmap_semantics(kernel: &Kernel) {
    let task = kernel.cur_task(0).unwrap();
    let private_path = "/mmap-private";
    let private_original = vec![0x31u8; PAGE_SZ];
    kernel
        .install_file(private_path, private_original.clone(), false)
        .unwrap();
    let private_fd = do_open(kernel, &task, private_path, 0, 0).unwrap();
    assert_eq!(
        sys_mmap(
            kernel,
            FILE_PRIVATE_BASE,
            PAGE_SZ,
            PROT_READ | PROT_WRITE,
            MAP_FIXED | MAP_PRIVATE,
            private_fd,
            0,
        ),
        Ok(FILE_PRIVATE_BASE)
    );
    let mut private_parent = task.process.addr_space.lock().unwrap();
    private_parent
        .write_user_bytes(FILE_PRIVATE_BASE + 5, b"private", &kernel.pool)
        .unwrap();
    let mut private_child = AddrSpace::fork_from(&mut private_parent, &kernel.pool).unwrap();
    private_child
        .write_user_bytes(FILE_PRIVATE_BASE + 32, b"child", &kernel.pool)
        .unwrap();
    let mut parent_private_byte = [0u8; 1];
    private_parent
        .read_user_bytes(FILE_PRIVATE_BASE + 32, &mut parent_private_byte)
        .unwrap();
    assert_eq!(parent_private_byte, [0x31]);
    private_child
        .unmap_range(FILE_PRIVATE_BASE, PAGE_SZ, &kernel.pool)
        .unwrap();
    private_parent
        .unmap_range(FILE_PRIVATE_BASE, PAGE_SZ, &kernel.pool)
        .unwrap();
    drop(private_parent);
    assert_eq!(
        read_fixture(kernel, private_path, PAGE_SZ),
        private_original
    );
    kernel.close_task_fd(&task, private_fd).unwrap();

    let shared_path = "/mmap-shared-close";
    kernel
        .install_file(shared_path, vec![0x42u8; PAGE_SZ + 15], false)
        .unwrap();
    let shared_fd = do_open(kernel, &task, shared_path, 2, 0).unwrap();
    assert_eq!(
        sys_mmap(
            kernel,
            FILE_SHARED_BASE,
            2 * PAGE_SZ,
            PROT_READ | PROT_WRITE,
            MAP_FIXED | MAP_SHARED,
            shared_fd,
            0,
        ),
        Ok(FILE_SHARED_BASE)
    );
    kernel.close_task_fd(&task, shared_fd).unwrap();
    kernel
        .handle_pgfault(FILE_SHARED_BASE, KernelPageFaultAccess::Store)
        .unwrap();
    {
        let mut addr_space = task.process.addr_space.lock().unwrap();
        addr_space.check_page_table_consistency().unwrap();
        addr_space
            .write_user_bytes(FILE_SHARED_BASE + 9, b"shared", &kernel.pool)
            .unwrap();
        addr_space
            .write_user_bytes(FILE_SHARED_BASE + PAGE_SZ + 7, b"usercopy", &kernel.pool)
            .unwrap();
        addr_space
            .write_user_bytes(FILE_SHARED_BASE + PAGE_SZ + 100, b"no-extend", &kernel.pool)
            .unwrap();
        addr_space.check_page_table_consistency().unwrap();
    }
    assert_eq!(sys_munmap(kernel, FILE_SHARED_BASE, 2 * PAGE_SZ), Ok(0));
    let shared_bytes = read_fixture(kernel, shared_path, 2 * PAGE_SZ);
    assert_eq!(shared_bytes.len(), PAGE_SZ + 15);
    assert_eq!(&shared_bytes[9..15], b"shared");
    assert_eq!(&shared_bytes[PAGE_SZ + 7..PAGE_SZ + 15], b"usercopy");
}

// AGENT: share PgFrame plus dirty state across fork, retain the correct offset
// after right-side munmap, and reject file-backed checkpoint serialization.
fn shared_file_fork_split_and_checkpoint(kernel: &Kernel) {
    let task = kernel.cur_task(0).unwrap();
    let fork_path = "/mmap-shared-fork";
    kernel
        .install_file(fork_path, vec![0x51u8; PAGE_SZ], false)
        .unwrap();
    let fork_fd = do_open(kernel, &task, fork_path, 2, 0).unwrap();
    assert_eq!(
        sys_mmap(
            kernel,
            FILE_FORK_BASE,
            PAGE_SZ,
            PROT_READ | PROT_WRITE,
            MAP_FIXED | MAP_SHARED,
            fork_fd,
            0,
        ),
        Ok(FILE_FORK_BASE)
    );
    kernel.close_task_fd(&task, fork_fd).unwrap();
    let mut parent = task.process.addr_space.lock().unwrap();
    assert_eq!(parent.snapshot_checkpoint_memory(), Err("enotsup"));
    let mut child = AddrSpace::fork_from(&mut parent, &kernel.pool).unwrap();
    child
        .write_user_bytes(FILE_FORK_BASE + 3, b"fork", &kernel.pool)
        .unwrap();
    let mut parent_view = [0u8; 4];
    parent
        .read_user_bytes(FILE_FORK_BASE + 3, &mut parent_view)
        .unwrap();
    assert_eq!(&parent_view, b"fork");
    child
        .unmap_range(FILE_FORK_BASE, PAGE_SZ, &kernel.pool)
        .unwrap();
    parent
        .unmap_range(FILE_FORK_BASE, PAGE_SZ, &kernel.pool)
        .unwrap();
    drop(parent);
    assert_eq!(&read_fixture(kernel, fork_path, PAGE_SZ)[3..7], b"fork");

    let partial_path = "/mmap-shared-partial";
    let mut partial_original = vec![0x61u8; 3 * PAGE_SZ];
    partial_original[PAGE_SZ..2 * PAGE_SZ].fill(0x62);
    partial_original[2 * PAGE_SZ..].fill(0x63);
    kernel
        .install_file(partial_path, partial_original, false)
        .unwrap();
    let partial_fd = do_open(kernel, &task, partial_path, 2, 0).unwrap();
    assert_eq!(
        sys_mmap(
            kernel,
            FILE_PARTIAL_BASE,
            3 * PAGE_SZ,
            PROT_READ | PROT_WRITE,
            MAP_FIXED | MAP_SHARED,
            partial_fd,
            0,
        ),
        Ok(FILE_PARTIAL_BASE)
    );
    task.process
        .addr_space
        .lock()
        .unwrap()
        .write_user_bytes(FILE_PARTIAL_BASE + PAGE_SZ + 11, b"right", &kernel.pool)
        .unwrap();
    assert_eq!(
        sys_munmap(kernel, FILE_PARTIAL_BASE + PAGE_SZ, PAGE_SZ),
        Ok(0)
    );
    {
        let addr_space = task.process.addr_space.lock().unwrap();
        let left = addr_space.mapped_region(FILE_PARTIAL_BASE).unwrap();
        assert_eq!(left.len, PAGE_SZ);
        let VmBacking::File { offset, .. } = &left.backing else {
            panic!("partial mmap left VMA lost file backing")
        };
        assert_eq!(*offset, 0);
        assert!(addr_space
            .mapped_region(FILE_PARTIAL_BASE + PAGE_SZ)
            .is_none());
        let right = addr_space
            .mapped_region(FILE_PARTIAL_BASE + 2 * PAGE_SZ)
            .unwrap();
        assert_eq!(right.len, PAGE_SZ);
        let VmBacking::File { offset, .. } = &right.backing else {
            panic!("partial mmap right VMA lost file backing")
        };
        assert_eq!(*offset, 2 * PAGE_SZ);
    }
    assert_eq!(
        &read_fixture(kernel, partial_path, 3 * PAGE_SZ)[PAGE_SZ + 11..PAGE_SZ + 16],
        b"right"
    );
    assert_eq!(sys_munmap(kernel, FILE_PARTIAL_BASE, PAGE_SZ), Ok(0));
    assert_eq!(
        sys_munmap(kernel, FILE_PARTIAL_BASE + 2 * PAGE_SZ, PAGE_SZ),
        Ok(0)
    );
    kernel.close_task_fd(&task, partial_fd).unwrap();
}

// AGENT: require fixed replacement to persist the overwritten shared page
// before reading and publishing the new private file backing.
fn fixed_file_mmap_writes_back_before_replacement(kernel: &Kernel) {
    let task = kernel.cur_task(0).unwrap();
    let old_path = "/mmap-fixed-old";
    let new_path = "/mmap-fixed-new";
    kernel
        .install_file(old_path, vec![0x71u8; PAGE_SZ], false)
        .unwrap();
    kernel
        .install_file(new_path, vec![0x72u8; PAGE_SZ], false)
        .unwrap();
    let old_fd = do_open(kernel, &task, old_path, 2, 0).unwrap();
    let new_fd = do_open(kernel, &task, new_path, 0, 0).unwrap();
    assert_eq!(
        sys_mmap(
            kernel,
            FILE_REPLACE_BASE,
            PAGE_SZ,
            PROT_READ | PROT_WRITE,
            MAP_FIXED | MAP_SHARED,
            old_fd,
            0,
        ),
        Ok(FILE_REPLACE_BASE)
    );
    task.process
        .addr_space
        .lock()
        .unwrap()
        .write_user_bytes(FILE_REPLACE_BASE, b"old!", &kernel.pool)
        .unwrap();
    assert_eq!(
        sys_mmap(
            kernel,
            FILE_REPLACE_BASE,
            PAGE_SZ,
            PROT_READ,
            MAP_FIXED | MAP_PRIVATE,
            new_fd,
            0,
        ),
        Ok(FILE_REPLACE_BASE)
    );
    let mut new_view = [0u8; 4];
    task.process
        .addr_space
        .lock()
        .unwrap()
        .read_user_bytes(FILE_REPLACE_BASE, &mut new_view)
        .unwrap();
    assert_eq!(&new_view, &[0x72; 4]);
    assert_eq!(&read_fixture(kernel, old_path, PAGE_SZ)[..4], b"old!");
    assert_eq!(sys_munmap(kernel, FILE_REPLACE_BASE, PAGE_SZ), Ok(0));
    kernel.close_task_fd(&task, old_fd).unwrap();
    kernel.close_task_fd(&task, new_fd).unwrap();
}

// AGENT: expose independent read and stable-flush failure switches around a
// real RAM block device so mmap transaction tests still exercise ChaosFs I/O.
struct ControlledFailureDevice {
    backing: RamBlockDevice,
    fail_reads: Arc<AtomicBool>,
    fail_flush: Arc<AtomicBool>,
}

// AGENT: delegate ordinary block traffic while failing only the operation
// selected by the test, keeping filesystem construction deterministic.
impl BlockDevice for ControlledFailureDevice {
    // AGENT: preserve the fixed RAM backing capacity.
    fn block_count(&self) -> usize {
        self.backing.block_count()
    }

    // AGENT: inject eager-file population failures without corrupting storage.
    fn read_block(&self, block: usize) -> Result<Vec<u8>, &'static str> {
        if self.fail_reads.load(Ordering::Acquire) {
            return Err("eio");
        }
        self.backing.read_block(block)
    }

    // AGENT: leave cached writeback functional so read and flush failures are
    // independently attributable to the intended transaction phase.
    fn write_block(&self, block: usize, data: &[u8]) -> Result<(), &'static str> {
        self.backing.write_block(block, data)
    }

    // AGENT: fail the final durability barrier after cached file bytes were
    // copied, exercising the required mapping-state preservation path.
    fn flush(&self) -> Result<(), &'static str> {
        if self.fail_flush.load(Ordering::Acquire) {
            return Err("eio");
        }
        self.backing.flush()
    }
}

// AGENT: retain VMA, PTE, frame contents, and sticky dirty state across failed
// unmap writeback and failed MAP_FIXED population, then prove both can retry.
fn file_mmap_failure_transactions(pool: FramePool) {
    let fail_reads = Arc::new(AtomicBool::new(false));
    let fail_flush = Arc::new(AtomicBool::new(false));
    let device = Arc::new(ControlledFailureDevice {
        backing: RamBlockDevice::empty(),
        fail_reads: fail_reads.clone(),
        fail_flush: fail_flush.clone(),
    });
    let kernel = Kernel::new_with_block_device(pool, device);
    kernel.proc_init();
    let task = kernel.cur_task(0).unwrap();

    let writeback_path = "/mmap-failed-writeback";
    kernel
        .install_file(writeback_path, vec![0x81u8; PAGE_SZ], false)
        .unwrap();
    let writeback_fd = do_open(&kernel, &task, writeback_path, 2, 0).unwrap();
    assert_eq!(
        sys_mmap(
            &kernel,
            FILE_FAILURE_BASE,
            PAGE_SZ,
            PROT_READ | PROT_WRITE,
            MAP_FIXED | MAP_SHARED,
            writeback_fd,
            0,
        ),
        Ok(FILE_FAILURE_BASE)
    );
    task.process
        .addr_space
        .lock()
        .unwrap()
        .write_user_bytes(FILE_FAILURE_BASE + 4, b"keep", &kernel.pool)
        .unwrap();
    fail_flush.store(true, Ordering::Release);
    assert_eq!(sys_munmap(&kernel, FILE_FAILURE_BASE, PAGE_SZ), Err("eio"));
    let mut retained = [0u8; 4];
    {
        let addr_space = task.process.addr_space.lock().unwrap();
        assert!(addr_space.mapped_region(FILE_FAILURE_BASE).is_some());
        addr_space
            .read_user_bytes(FILE_FAILURE_BASE + 4, &mut retained)
            .unwrap();
        addr_space.check_page_table_consistency().unwrap();
    }
    assert_eq!(&retained, b"keep");
    fail_flush.store(false, Ordering::Release);
    assert_eq!(sys_munmap(&kernel, FILE_FAILURE_BASE, PAGE_SZ), Ok(0));
    kernel.close_task_fd(&task, writeback_fd).unwrap();

    let old_path = "/mmap-failed-fixed-old";
    let new_path = "/mmap-failed-fixed-new";
    let large_len = 20 * PAGE_SZ;
    kernel
        .install_file(old_path, vec![0x91u8; large_len], false)
        .unwrap();
    kernel
        .install_file(new_path, vec![0x92u8; large_len], false)
        .unwrap();
    let old_fd = do_open(&kernel, &task, old_path, 2, 0).unwrap();
    let new_fd = do_open(&kernel, &task, new_path, 0, 0).unwrap();
    assert_eq!(
        sys_mmap(
            &kernel,
            FILE_FAILURE_BASE,
            large_len,
            PROT_READ | PROT_WRITE,
            MAP_FIXED | MAP_SHARED,
            old_fd,
            0,
        ),
        Ok(FILE_FAILURE_BASE)
    );
    task.process
        .addr_space
        .lock()
        .unwrap()
        .write_user_bytes(FILE_FAILURE_BASE, b"stay", &kernel.pool)
        .unwrap();
    fail_reads.store(true, Ordering::Release);
    assert_eq!(
        sys_mmap(
            &kernel,
            FILE_FAILURE_BASE,
            large_len,
            PROT_READ,
            MAP_FIXED | MAP_PRIVATE,
            new_fd,
            0,
        ),
        Err("eio")
    );
    fail_reads.store(false, Ordering::Release);
    let mut old_view = [0u8; 4];
    {
        let addr_space = task.process.addr_space.lock().unwrap();
        addr_space
            .read_user_bytes(FILE_FAILURE_BASE, &mut old_view)
            .unwrap();
        addr_space.check_page_table_consistency().unwrap();
    }
    assert_eq!(&old_view, b"stay");
    assert_eq!(
        sys_mmap(
            &kernel,
            FILE_FAILURE_BASE,
            large_len,
            PROT_READ,
            MAP_FIXED | MAP_PRIVATE,
            new_fd,
            0,
        ),
        Ok(FILE_FAILURE_BASE)
    );
    let mut new_view = [0u8; 4];
    task.process
        .addr_space
        .lock()
        .unwrap()
        .read_user_bytes(FILE_FAILURE_BASE, &mut new_view)
        .unwrap();
    assert_eq!(&new_view, &[0x92; 4]);
    assert_eq!(sys_munmap(&kernel, FILE_FAILURE_BASE, large_len), Ok(0));
    kernel.close_task_fd(&task, old_fd).unwrap();
    kernel.close_task_fd(&task, new_fd).unwrap();
}
