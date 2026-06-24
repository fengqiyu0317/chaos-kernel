// AGENT
use kernel_sim::{
    AddrSpace, EpData, EpEvent, ExitReason, FHandle, FLike, FdOpt, Kernel, PageBacking,
    PageTableEntry, PgFrame, SchedulePolicy, Task, TaskRunState, TaskTable, VmRegion, AT_ENTRY,
    AT_PAGESZ, KERN_BASE, MAP_ANONYMOUS, MAP_PRIVATE, MAP_SHARED, N_FRAMES, N_PROC, N_REGS,
    O_CLOEXEC, O_CREAT, PAGE_SZ, PROT_READ, PROT_WRITE, SIGUSR1, SYS_BRK, SYS_DUP,
    SYS_EPOLL_CREATE, SYS_EPOLL_CTL, SYS_EXEC, SYS_EXIT, SYS_FORK, SYS_FUTEX, SYS_GETPID, SYS_KILL,
    SYS_MMAP, SYS_MUNMAP, SYS_OPEN, SYS_READ, SYS_SIGACTION, SYS_SIGRETURN, SYS_WAIT4, SYS_WRITE,
    USR_STK_OFF, USR_STK_SZ, VM_EXEC, VM_READ, VM_SHARED, VM_WRITE,
};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Barrier, Mutex};
use std::thread;
use std::time::{Duration, Instant};

const TEST_EXEC_ENTRY: usize = 0x0040_0000;
const TEST_EXEC_LOAD_OFFSET: usize = PAGE_SZ;
const ELF_PH_OFF: usize = 64;
const ELF_PH_SIZE: usize = 56;
const TEST_EXEC_PAYLOAD: &[u8] = b"kernel-sim exec payload";

#[repr(C)]
#[derive(Clone, Copy)]
struct UserSigAction {
    sa_handler: usize,
    sa_sigaction: usize,
    sa_mask: u64,
    sa_flags: i32,
}

fn usize_array_bytes(values: &[usize]) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(values.len() * std::mem::size_of::<usize>());
    for value in values {
        bytes.extend_from_slice(&value.to_ne_bytes());
    }
    bytes
}

fn read_user_c_string(addr_space: &AddrSpace, addr: usize) -> String {
    let mut bytes = Vec::new();
    loop {
        let mut byte = [0u8; 1];
        addr_space
            .read_user_bytes(addr + bytes.len(), &mut byte)
            .expect("user string byte should be readable");
        if byte[0] == 0 {
            break;
        }
        bytes.push(byte[0]);
    }
    String::from_utf8(bytes).expect("user string should be utf-8")
}

// AGENT: place a NUL-terminated path into user memory for syscall open/exec tests.
fn write_user_c_string(kernel: &Kernel, task: &Arc<Task>, addr: usize, value: &str) {
    let mut bytes = value.as_bytes().to_vec();
    bytes.push(0);
    let page = addr & !(PAGE_SZ - 1);
    let end = addr + bytes.len();
    let mapped_end = (end + PAGE_SZ - 1) & !(PAGE_SZ - 1);
    {
        let mut addr_space = task.process.addr_space.lock().unwrap();
        if addr_space.vm_map.find(addr).is_none() {
            addr_space
                .map_region(
                    VmRegion::new(page, mapped_end - page, VM_READ | VM_WRITE),
                    &kernel.pool,
                )
                .expect("user string page should map");
        }
        addr_space
            .write_user_bytes(addr, &bytes, &kernel.pool)
            .expect("user string should be writable");
    }
}

// AGENT: create a user mapping for syscall copy-in/copy-out tests.
fn map_user_region(kernel: &Kernel, task: &Arc<Task>, addr: usize, len: usize, flags: u32) {
    task.process
        .addr_space
        .lock()
        .unwrap()
        .map_region(VmRegion::new(addr, len, flags), &kernel.pool)
        .expect("user test region should map");
}

fn install_test_exec(kernel: &Kernel, path: &str) {
    kernel
        .install_exec_file(path, test_exec_elf())
        .expect("test exec file should install");
}

// AGENT: seed live process state that must survive failed exec preparation.
fn seed_failed_exec_state(
    kernel: &Kernel,
    task: &Arc<Task>,
    old_mapping: usize,
) -> (usize, usize, usize) {
    let path_addr = old_mapping + PAGE_SZ;
    write_user_c_string(kernel, task, path_addr, "/tmp/failed-exec-cloexec");
    let close_fd = kernel
        .dispatch_syscall(SYS_OPEN, path_addr, O_CREAT | O_CLOEXEC, 0, 0, 0, 0)
        .expect("open should create a cloexec fd");
    *task.process.exec_path.lock().unwrap() = String::from("/bin/old");
    {
        let mut addr_space = task.process.addr_space.lock().unwrap();
        addr_space
            .map_region(
                VmRegion::new(old_mapping, PAGE_SZ, VM_READ | VM_WRITE),
                &kernel.pool,
            )
            .expect("old mapping should be created");
    }
    {
        let mut thd = task.thd_ctx.lock().unwrap();
        let ctx = thd.as_mut().expect("thread context should exist");
        ctx.uctx.set_ip(0x1111);
        ctx.uctx.set_sp(0x2222);
        ctx.clear_tid = 123;
    }
    (close_fd, task.vm_token(), kernel.pool.free_count())
}

// AGENT: verify failed exec left address space, context, fd table, and frames intact.
fn assert_failed_exec_state_preserved(
    kernel: &Kernel,
    task: &Arc<Task>,
    close_fd: usize,
    old_token: usize,
    free_before: usize,
    old_mapping: usize,
) {
    assert_eq!(kernel.pool.free_count(), free_before);
    assert_eq!(&*task.process.exec_path.lock().unwrap(), "/bin/old");
    assert_eq!(task.vm_token(), old_token);
    match task
        .get_file(close_fd)
        .expect("cloexec fd should survive failed exec")
    {
        FLike::File(f) => assert!(f.cloexec),
        _ => panic!("expected regular file"),
    }
    {
        let addr_space = task.process.addr_space.lock().unwrap();
        assert!(addr_space.vm_map.find(old_mapping).is_some());
        assert!(addr_space.vm_map.find(TEST_EXEC_ENTRY).is_none());
    }
    {
        let thd = task.thd_ctx.lock().unwrap();
        let ctx = thd.as_ref().expect("thread context should exist");
        assert_eq!(ctx.uctx.ip, 0x1111);
        assert_eq!(*ctx.uctx.r.last().unwrap(), 0x2222);
        assert_eq!(ctx.clear_tid, 123);
    }
}

fn test_exec_elf() -> Vec<u8> {
    elf_with_load_payload(
        TEST_EXEC_LOAD_OFFSET,
        TEST_EXEC_ENTRY,
        TEST_EXEC_PAYLOAD,
        PAGE_SZ,
        0x5,
    )
}

fn elf_with_load_payload(
    offset: usize,
    vaddr: usize,
    payload: &[u8],
    mem_size: usize,
    flags: u32,
) -> Vec<u8> {
    let file_size = payload.len();
    let mut data = vec![0u8; (ELF_PH_OFF + ELF_PH_SIZE).max(offset + file_size)];
    data[0] = 0x7f;
    data[1] = b'E';
    data[2] = b'L';
    data[3] = b'F';
    data[4] = 2;
    data[5] = 1;
    data[6] = 1;
    write_u16_le(&mut data, 16, 2);
    write_u16_le(&mut data, 18, 0x3e);
    write_u32_le(&mut data, 20, 1);
    write_u64_le(&mut data, 24, vaddr as u64);
    write_u64_le(&mut data, 32, ELF_PH_OFF as u64);
    write_u16_le(&mut data, 52, 64);
    write_u16_le(&mut data, 54, ELF_PH_SIZE as u16);
    write_u16_le(&mut data, 56, 1);

    write_u32_le(&mut data, ELF_PH_OFF, 1);
    write_u32_le(&mut data, ELF_PH_OFF + 4, flags);
    write_u64_le(&mut data, ELF_PH_OFF + 8, offset as u64);
    write_u64_le(&mut data, ELF_PH_OFF + 16, vaddr as u64);
    write_u64_le(&mut data, ELF_PH_OFF + 24, vaddr as u64);
    write_u64_le(&mut data, ELF_PH_OFF + 32, file_size as u64);
    write_u64_le(&mut data, ELF_PH_OFF + 40, mem_size as u64);
    write_u64_le(&mut data, ELF_PH_OFF + 48, PAGE_SZ as u64);
    data[offset..offset + file_size].copy_from_slice(payload);
    data
}

fn write_u16_le(data: &mut [u8], off: usize, value: u16) {
    data[off..off + 2].copy_from_slice(&value.to_le_bytes());
}

fn write_u32_le(data: &mut [u8], off: usize, value: u32) {
    data[off..off + 4].copy_from_slice(&value.to_le_bytes());
}

fn write_u64_le(data: &mut [u8], off: usize, value: u64) {
    data[off..off + 8].copy_from_slice(&value.to_le_bytes());
}

#[test]
fn boot_kernel_in_standalone_runtime() {
    let kernel = Kernel::new(N_FRAMES);
    kernel.proc_init();

    assert_eq!(kernel.cur_task(0).expect("init should be current").id(), 1);

    let pid = kernel
        .dispatch_syscall(SYS_GETPID, 0, 0, 0, 0, 0, 0)
        .expect("getpid should succeed in standalone runtime");

    assert_eq!(pid, 1);
}

#[test]
// AGENT
fn syscall_fork_creates_child_task_and_enqueues_it() {
    let kernel = Kernel::new(N_FRAMES);
    kernel.proc_init();

    let child = kernel
        .dispatch_syscall(SYS_FORK, 0, 0, 0, 0, 0, 0)
        .expect("fork syscall should create child task");
    let child_task = kernel
        .tasks
        .find(child)
        .expect("fork syscall should register child task");

    assert_eq!(kernel.tasks.count(), 2);
    assert_eq!(
        child_task
            .process
            .parent
            .lock()
            .unwrap()
            .as_ref()
            .expect("child should remember parent")
            .id(),
        1
    );
    assert_eq!(child_task.sched_state(), TaskRunState::Runnable);
    assert_eq!(kernel.run_queue.len(), 1);
}

#[test]
// AGENT
fn fork_copies_context_address_space_cwd_and_kernel_stack() {
    let kernel = Kernel::new(N_FRAMES);
    kernel.proc_init();
    let parent = kernel.cur_task(0).expect("init should be current");
    let parent_token = parent.vm_token();
    {
        *parent.process.debug_fds.lock().unwrap() = vec![String::from("fd:tracked")];
        parent.sched.lock().unwrap().policy = SchedulePolicy::with_prio(-4);
        parent.process.sig_state.lock().unwrap().sig_raise(SIGUSR1);
        let mut thd = parent.thd_ctx.lock().unwrap();
        let ctx = thd.as_mut().expect("parent context should exist");
        ctx.uctx.set_ip(0x1234);
        ctx.uctx.r[0] = 99;
        ctx.uctx.r[3] = 0x7777;
        ctx.clear_tid = 42;
        ctx.smask = 0x55;
    }
    {
        *parent.process.cwd.lock().unwrap() = String::from("caf\u{e9}/fork");
        let mut addr_space = parent.process.addr_space.lock().unwrap();
        addr_space.vm_map.brk = 0x0060_0000;
        addr_space
            .vm_map
            .insert(VmRegion::new(0x5000_0000, PAGE_SZ, VM_READ | VM_WRITE))
            .expect("test mapping should not overlap");
        addr_space.page_table.lock().unwrap().insert(
            0x5000_0000,
            PageTableEntry::new(17, PgFrame::with_rc(1), VM_READ | VM_WRITE),
        );
    }

    let child = kernel.do_fork(1).expect("fork should create child");
    let child_task = kernel
        .tasks
        .find(child)
        .expect("child should be registered");

    assert_ne!(child_task.vm_token(), parent_token);
    assert!(child_task.kstk.lock().unwrap().is_some());
    assert_eq!(&*child_task.process.cwd.lock().unwrap(), "caf\u{e9}/fork");
    assert_eq!(
        child_task.process.debug_fds.lock().unwrap().as_slice(),
        &[String::from("fd:tracked")]
    );
    {
        let child_policy = child_task.sched_policy();
        assert_eq!(child_policy.prio, -4);
        assert_eq!(child_policy.nice, -4);
    }
    assert_eq!(child_task.process.sig_state.lock().unwrap().pending, 0);
    {
        let child_addr_space = child_task.process.addr_space.lock().unwrap();
        assert_eq!(child_addr_space.vm_map.brk, 0x0060_0000);
        assert!(child_addr_space.vm_map.find(0x5000_0000).is_some());
        let child_pte = child_addr_space
            .page_table
            .lock()
            .unwrap()
            .get(&0x5000_0000)
            .expect("child should have a COW PTE")
            .clone();
        assert!(child_pte.cow);
        assert!(!child_pte.writable);
        assert_eq!(child_pte.frame.count(), 2);
    }
    {
        let parent_addr_space = parent.process.addr_space.lock().unwrap();
        let parent_pte = parent_addr_space
            .page_table
            .lock()
            .unwrap()
            .get(&0x5000_0000)
            .expect("parent should have a COW PTE")
            .clone();
        assert!(parent_pte.cow);
        assert!(!parent_pte.writable);
        assert_eq!(parent_pte.frame.count(), 2);
    }
    {
        parent.process.addr_space.lock().unwrap().vm_map.brk = 0x0070_0000;
        let child_addr_space = child_task.process.addr_space.lock().unwrap();
        assert_eq!(child_addr_space.vm_map.brk, 0x0060_0000);
    }
    {
        let thd = child_task.thd_ctx.lock().unwrap();
        let ctx = thd.as_ref().expect("child context should exist");
        assert_eq!(ctx.uctx.ip, 0x1234);
        assert_eq!(ctx.uctx.r[0], 0);
        assert_eq!(ctx.uctx.r[3], 0x7777);
        assert_eq!(ctx.clear_tid, 42);
        assert_eq!(ctx.smask, 0x55);
    }
    {
        let thd = parent.thd_ctx.lock().unwrap();
        let ctx = thd.as_ref().expect("parent context should still exist");
        assert_eq!(ctx.uctx.r[0], 99);
    }
}

#[test]
// AGENT
fn cow_write_fault_copies_child_page_and_keeps_parent_shared() {
    let kernel = Kernel::new(N_FRAMES);
    kernel.proc_init();
    let parent = kernel.cur_task(0).expect("init should be current");
    let page = 0x5100_0000;
    {
        let mut addr_space = parent.process.addr_space.lock().unwrap();
        addr_space
            .vm_map
            .insert(VmRegion::new(page, PAGE_SZ, VM_READ | VM_WRITE))
            .expect("test mapping should not overlap");
        addr_space.page_table.lock().unwrap().insert(
            page,
            PageTableEntry::new(33, PgFrame::with_rc(1), VM_READ | VM_WRITE),
        );
    }

    let child = kernel.do_fork(1).expect("fork should create child");
    let child_task = kernel
        .tasks
        .find(child)
        .expect("child should be registered");
    kernel.set_cur(0, Some(child_task.clone()));

    assert!(kernel.handle_pgfault_ext(page, 0x2));

    let child_addr_space = child_task.process.addr_space.lock().unwrap();
    let child_pte = child_addr_space
        .page_table
        .lock()
        .unwrap()
        .get(&page)
        .expect("child should retain a mapped PTE")
        .clone();
    assert!(child_pte.writable);
    assert!(!child_pte.cow);
    assert_ne!(child_pte.frame_id, 33);
    assert_eq!(child_pte.frame.count(), 1);
    assert_eq!(child_addr_space.cow_sharers(), 0);
    drop(child_addr_space);

    let parent_addr_space = parent.process.addr_space.lock().unwrap();
    let parent_pte = parent_addr_space
        .page_table
        .lock()
        .unwrap()
        .get(&page)
        .expect("parent should keep the original PTE")
        .clone();
    assert_eq!(parent_pte.frame_id, 33);
    assert!(parent_pte.cow);
    assert!(!parent_pte.writable);
    assert_eq!(parent_pte.frame.count(), 1);
}

#[test]
// AGENT: MAP_PRIVATE file mmap loads file bytes but does not write changes back.
fn mmap_private_file_mapping_does_not_write_back() {
    let kernel = Kernel::new(N_FRAMES);
    kernel.proc_init();
    let task = kernel.cur_task(0).expect("init should be current");
    let handle = FHandle::with_data(
        "private-map",
        FdOpt {
            rd: true,
            wr: false,
            ap: false,
            nb: false,
        },
        b"abcdef".to_vec(),
    );
    let file_data = handle.inode_ref();
    let fd = task.add_file(FLike::File(handle));

    let addr = kernel
        .dispatch_syscall(
            SYS_MMAP,
            0,
            PAGE_SZ,
            PROT_READ | PROT_WRITE,
            MAP_PRIVATE,
            fd,
            0,
        )
        .expect("private file mmap should succeed with a readable fd");
    {
        let addr_space = task.process.addr_space.lock().unwrap();
        let mut loaded = [0u8; 8];
        addr_space
            .read_user_bytes(addr, &mut loaded)
            .expect("mapped private bytes should be readable");
        assert_eq!(&loaded[..6], b"abcdef");
        assert_eq!(&loaded[6..], &[0, 0]);
    }
    {
        let mut addr_space = task.process.addr_space.lock().unwrap();
        addr_space
            .write_user_bytes(addr + 1, b"ZZ", &kernel.pool)
            .expect("private mapping should be writable");
    }

    assert_eq!(&*file_data.lock().unwrap(), b"abcdef");
}

#[test]
// AGENT: MAP_SHARED file mmap writes through only the file-backed part of pages.
fn mmap_shared_file_mapping_writes_back_without_extending_tail() {
    let kernel = Kernel::new(N_FRAMES);
    kernel.proc_init();
    let task = kernel.cur_task(0).expect("init should be current");
    let handle = FHandle::with_data(
        "shared-map",
        FdOpt {
            rd: true,
            wr: true,
            ap: false,
            nb: false,
        },
        b"abcde".to_vec(),
    );
    let file_data = handle.inode_ref();
    let fd = task.add_file(FLike::File(handle));

    let addr = kernel
        .dispatch_syscall(
            SYS_MMAP,
            0,
            PAGE_SZ * 2,
            PROT_READ | PROT_WRITE,
            MAP_SHARED,
            fd,
            0,
        )
        .expect("shared writable file mmap should succeed with an rdwr fd");
    {
        let addr_space = task.process.addr_space.lock().unwrap();
        let mut loaded = [0xFFu8; 8];
        addr_space
            .read_user_bytes(addr, &mut loaded)
            .expect("shared mapping should be readable");
        assert_eq!(&loaded[..5], b"abcde");
        assert_eq!(&loaded[5..], &[0, 0, 0]);
    }
    {
        let mut addr_space = task.process.addr_space.lock().unwrap();
        addr_space
            .write_user_bytes(addr + 1, b"YY", &kernel.pool)
            .expect("shared mapping should write back inside file");
        addr_space
            .write_user_bytes(addr + PAGE_SZ, b"tail", &kernel.pool)
            .expect("zero-filled page beyond EOF stays writable in memory");
    }

    assert_eq!(&*file_data.lock().unwrap(), b"aYYde");
}

#[test]
// AGENT: file mmap rejects unaligned offsets and shared writable mappings on read-only fds.
fn mmap_file_mapping_validates_offset_and_shared_write_permissions() {
    let kernel = Kernel::new(N_FRAMES);
    kernel.proc_init();
    let task = kernel.cur_task(0).expect("init should be current");
    let handle = FHandle::with_data(
        "readonly-map",
        FdOpt {
            rd: true,
            wr: false,
            ap: false,
            nb: false,
        },
        b"page".to_vec(),
    );
    let fd = task.add_file(FLike::File(handle));

    let bad_offset = kernel
        .dispatch_syscall(SYS_MMAP, 0, PAGE_SZ, PROT_READ, MAP_PRIVATE, fd, 1)
        .expect_err("file mmap offset must be page-aligned");
    assert_eq!(bad_offset, "einval");

    let bad_write = kernel
        .dispatch_syscall(
            SYS_MMAP,
            0,
            PAGE_SZ,
            PROT_READ | PROT_WRITE,
            MAP_SHARED,
            fd,
            0,
        )
        .expect_err("shared writable mmap requires a writable fd");
    assert_eq!(bad_write, "eacces");
}

#[test]
// AGENT: sys_read copies real file bytes into user memory and shares dup offset.
fn sys_read_regular_file_copies_data_and_advances_shared_offset() {
    let kernel = Kernel::new(N_FRAMES);
    kernel.proc_init();
    kernel
        .install_file("/tmp/read-real", b"abcdef".to_vec(), false)
        .expect("test file should install");
    let task = kernel.cur_task(0).expect("init should be current");
    const PATH: usize = 0x2100_0000;
    const BUF: usize = 0x2101_0000;
    write_user_c_string(&kernel, &task, PATH, "/tmp/read-real");
    map_user_region(&kernel, &task, BUF, PAGE_SZ, VM_READ | VM_WRITE);

    let fd = kernel
        .dispatch_syscall(SYS_OPEN, PATH, 0, 0, 0, 0, 0)
        .expect("open should resolve an installed file");
    let first = kernel
        .dispatch_syscall(SYS_READ, fd, BUF, 3, 0, 0, 0)
        .expect("first read should succeed");
    assert_eq!(first, 3);
    let mut loaded = [0u8; 3];
    task.process
        .addr_space
        .lock()
        .unwrap()
        .read_user_bytes(BUF, &mut loaded)
        .expect("read bytes should be in user memory");
    assert_eq!(&loaded, b"abc");

    let dup_fd = kernel
        .dispatch_syscall(SYS_DUP, fd, 0, 0, 0, 0, 0)
        .expect("dup should share the open-file description");
    let second = kernel
        .dispatch_syscall(SYS_READ, dup_fd, BUF, 3, 0, 0, 0)
        .expect("dup read should continue from the shared offset");
    assert_eq!(second, 3);
    task.process
        .addr_space
        .lock()
        .unwrap()
        .read_user_bytes(BUF, &mut loaded)
        .expect("second read bytes should be in user memory");
    assert_eq!(&loaded, b"def");

    let eof = kernel
        .dispatch_syscall(SYS_READ, fd, BUF, 3, 0, 0, 0)
        .expect("EOF read should succeed");
    assert_eq!(eof, 0);
}

#[test]
// AGENT: sys_read reports real fd and user-buffer errors instead of synthetic lengths.
fn sys_read_validates_fd_permissions_and_user_buffer() {
    let kernel = Kernel::new(N_FRAMES);
    kernel.proc_init();
    let task = kernel.cur_task(0).expect("init should be current");
    const PATH: usize = 0x2200_0000;
    const BUF: usize = 0x2201_0000;
    const RO_BUF: usize = 0x2202_0000;
    write_user_c_string(&kernel, &task, PATH, "/tmp/write-only");
    map_user_region(&kernel, &task, BUF, PAGE_SZ, VM_READ | VM_WRITE);
    map_user_region(&kernel, &task, RO_BUF, PAGE_SZ, VM_READ);

    let wr_fd = kernel
        .dispatch_syscall(SYS_OPEN, PATH, O_CREAT | 1, 0, 0, 0, 0)
        .expect("write-only open should create a fd");
    let bad_fd = kernel
        .dispatch_syscall(SYS_READ, wr_fd + 100, BUF, 1, 0, 0, 0)
        .expect_err("invalid fd should fail");
    assert_eq!(bad_fd, "ebadf");
    let bad_perm = kernel
        .dispatch_syscall(SYS_READ, wr_fd, BUF, 1, 0, 0, 0)
        .expect_err("read on write-only fd should fail");
    assert_eq!(bad_perm, "ebadf");
    let unmapped = kernel
        .dispatch_syscall(SYS_READ, wr_fd, 0x2300_0000, 1, 0, 0, 0)
        .expect_err("unmapped user buffer should fail before fd read");
    assert_eq!(unmapped, "efault");
    let readonly = kernel
        .dispatch_syscall(SYS_READ, wr_fd, RO_BUF, 1, 0, 0, 0)
        .expect_err("read-only user buffer should fail before fd read");
    assert_eq!(readonly, "efault");
}

#[test]
// AGENT: sys_write and sys_read move real bytes through pipe file objects.
fn sys_read_and_write_use_pipe_file_objects() {
    let kernel = Kernel::new(N_FRAMES);
    kernel.proc_init();
    let task = kernel.cur_task(0).expect("init should be current");
    const SRC: usize = 0x2400_0000;
    const DST: usize = 0x2401_0000;
    map_user_region(&kernel, &task, SRC, PAGE_SZ, VM_READ | VM_WRITE);
    map_user_region(&kernel, &task, DST, PAGE_SZ, VM_READ | VM_WRITE);
    task.process
        .addr_space
        .lock()
        .unwrap()
        .write_user_bytes(SRC, b"pipe", &kernel.pool)
        .expect("source bytes should be writable");
    let (rd_fd, wr_fd) = kernel.do_pipe(1).expect("pipe should create fds");

    let written = kernel
        .dispatch_syscall(SYS_WRITE, wr_fd, SRC, 4, 0, 0, 0)
        .expect("pipe write should read user bytes");
    assert_eq!(written, 4);
    let read = kernel
        .dispatch_syscall(SYS_READ, rd_fd, DST, 8, 0, 0, 0)
        .expect("pipe read should copy queued bytes");
    assert_eq!(read, 4);
    let mut loaded = [0u8; 4];
    task.process
        .addr_space
        .lock()
        .unwrap()
        .read_user_bytes(DST, &mut loaded)
        .expect("pipe bytes should be copied to user memory");
    assert_eq!(&loaded, b"pipe");

    let empty = kernel
        .dispatch_syscall(SYS_READ, rd_fd, DST, 1, 0, 0, 0)
        .expect_err("empty pipe with a live writer reports again");
    assert_eq!(empty, "again");
}

#[test]
// AGENT: munmap requires a current task after validating the syscall range.
fn munmap_without_current_task_returns_esrch() {
    let kernel = Kernel::new(N_FRAMES);

    let err = kernel
        .dispatch_syscall(SYS_MUNMAP, 0x5600_0000, PAGE_SZ, 0, 0, 0, 0)
        .expect_err("munmap without a current task should fail");

    assert_eq!(err, "esrch");
}

#[test]
// AGENT: munmap validates syscall range parameters before removing mappings.
fn munmap_rejects_invalid_ranges_before_unmapping() {
    let kernel = Kernel::new(N_FRAMES);
    kernel.proc_init();
    let task = kernel.cur_task(0).expect("init should be current");
    let base = 0x5400_0000;
    {
        let mut addr_space = task.process.addr_space.lock().unwrap();
        addr_space
            .map_region(
                VmRegion::new(base, PAGE_SZ, VM_READ | VM_WRITE),
                &kernel.pool,
            )
            .expect("test mapping should be created");
    }

    let cases = [
        (base, 0, "einval"),
        (base + 1, PAGE_SZ, "einval"),
        (base, usize::MAX, "enomem"),
        (usize::MAX & !(PAGE_SZ - 1), PAGE_SZ * 2, "enomem"),
        (KERN_BASE - PAGE_SZ, PAGE_SZ * 2, "enomem"),
    ];
    for (addr, len, expected) in cases {
        let err = kernel
            .dispatch_syscall(SYS_MUNMAP, addr, len, 0, 0, 0, 0)
            .expect_err("invalid munmap should fail");
        assert_eq!(err, expected);
        assert!(
            task.process
                .addr_space
                .lock()
                .unwrap()
                .vm_map
                .find(base)
                .is_some(),
            "failed munmap must not remove the existing mapping"
        );
    }
}

#[test]
// AGENT: munmap rounds non-zero lengths up to full pages on the syscall path.
fn munmap_rounds_length_up_to_pages() {
    let kernel = Kernel::new(N_FRAMES);
    kernel.proc_init();
    let task = kernel.cur_task(0).expect("init should be current");
    let base = 0x5500_0000;
    {
        let mut addr_space = task.process.addr_space.lock().unwrap();
        addr_space
            .map_region(
                VmRegion::new(base, PAGE_SZ * 2, VM_READ | VM_WRITE),
                &kernel.pool,
            )
            .expect("test mapping should be created");
    }

    kernel
        .dispatch_syscall(SYS_MUNMAP, base, PAGE_SZ + 1, 0, 0, 0, 0)
        .expect("valid munmap should succeed");

    let addr_space = task.process.addr_space.lock().unwrap();
    assert!(addr_space.vm_map.find(base).is_none());
    assert!(addr_space.vm_map.find(base + PAGE_SZ).is_none());
}

#[test]
// AGENT: munmap releases last-reference resident pages back to the frame pool.
fn munmap_returns_frames_to_pool() {
    let kernel = Kernel::new(N_FRAMES);
    kernel.proc_init();
    let task = kernel.cur_task(0).expect("init should be current");
    let free_before = kernel.pool.free_count();

    let addr = kernel
        .dispatch_syscall(
            SYS_MMAP,
            0,
            PAGE_SZ * 2,
            PROT_READ | PROT_WRITE,
            MAP_PRIVATE | MAP_ANONYMOUS,
            0,
            0,
        )
        .expect("anonymous mmap should allocate two pages");
    assert_eq!(kernel.pool.free_count(), free_before - 2);

    kernel
        .dispatch_syscall(SYS_MUNMAP, addr, PAGE_SZ * 2, 0, 0, 0, 0)
        .expect("munmap should release mapped pages");

    assert_eq!(kernel.pool.free_count(), free_before);
    assert!(task
        .process
        .addr_space
        .lock()
        .unwrap()
        .vm_map
        .find(addr)
        .is_none());
}

#[test]
// AGENT: failed MAP_SHARED writeback reports an error and leaves mappings intact.
fn munmap_propagates_shared_writeback_error_without_unmapping() {
    let kernel = Kernel::new(N_FRAMES);
    kernel.proc_init();
    let task = kernel.cur_task(0).expect("init should be current");
    let base = 0x5600_0000;
    let file_data = Arc::new(Mutex::new(vec![0; PAGE_SZ]));
    {
        let mut addr_space = task.process.addr_space.lock().unwrap();
        addr_space
            .vm_map
            .insert(VmRegion::new(base, PAGE_SZ, VM_READ | VM_WRITE | VM_SHARED))
            .expect("test mapping should not overlap");
        let pte = PageTableEntry::with_backing(
            70,
            PgFrame::with_rc(1),
            VM_READ | VM_WRITE | VM_SHARED,
            PageBacking::File {
                data: file_data,
                offset: usize::MAX,
                valid_len: PAGE_SZ,
                shared: true,
            },
        );
        pte.data.lock().unwrap()[0] = 1;
        addr_space.page_table.lock().unwrap().insert(base, pte);
    }

    let err = kernel
        .dispatch_syscall(SYS_MUNMAP, base, PAGE_SZ, 0, 0, 0, 0)
        .expect_err("writeback overflow should fail munmap");

    assert_eq!(err, "efault");
    let addr_space = task.process.addr_space.lock().unwrap();
    assert!(addr_space.vm_map.find(base).is_some());
    let pte = addr_space
        .page_table
        .lock()
        .unwrap()
        .get(&base)
        .expect("failed munmap should keep the PTE")
        .clone();
    assert_eq!(pte.frame.count(), 1);
}

#[test]
// AGENT: shrinking brk uses unmap_range and returns heap pages to FramePool.
fn brk_shrink_returns_frames_to_pool() {
    let kernel = Kernel::new(N_FRAMES);
    kernel.proc_init();
    let task = kernel.cur_task(0).expect("init should be current");
    let initial_brk = task.process.addr_space.lock().unwrap().vm_map.brk;
    let free_before = kernel.pool.free_count();

    let grown = kernel
        .dispatch_syscall(SYS_BRK, initial_brk + PAGE_SZ * 2, 0, 0, 0, 0, 0)
        .expect("brk should grow by two pages");
    assert_eq!(grown, initial_brk + PAGE_SZ * 2);
    assert_eq!(kernel.pool.free_count(), free_before - 2);

    let shrunk = kernel
        .dispatch_syscall(SYS_BRK, initial_brk + PAGE_SZ, 0, 0, 0, 0, 0)
        .expect("brk should shrink by one page");
    assert_eq!(shrunk, initial_brk + PAGE_SZ);
    assert_eq!(kernel.pool.free_count(), free_before - 1);

    let reset = kernel
        .dispatch_syscall(SYS_BRK, initial_brk, 0, 0, 0, 0, 0)
        .expect("brk should shrink back to the original break");
    assert_eq!(reset, initial_brk);
    assert_eq!(kernel.pool.free_count(), free_before);
    assert_eq!(
        task.process.addr_space.lock().unwrap().vm_map.brk,
        initial_brk
    );
}

#[test]
// AGENT
fn unmap_range_returns_unmapped_page_count_and_splits_region() {
    let kernel = Kernel::new(N_FRAMES);
    kernel.proc_init();
    let task = kernel.cur_task(0).expect("init should be current");
    let base = 0x5300_0000;
    let frames = [60, 61, 62];
    {
        let mut addr_space = task.process.addr_space.lock().unwrap();
        addr_space
            .vm_map
            .insert(VmRegion::new(base, PAGE_SZ * 3, VM_READ | VM_WRITE))
            .expect("test mapping should not overlap");
        let mut pt = addr_space.page_table.lock().unwrap();
        for (idx, frame_id) in frames.into_iter().enumerate() {
            pt.insert(
                base + idx * PAGE_SZ,
                PageTableEntry::new(frame_id, PgFrame::with_rc(1), VM_READ | VM_WRITE),
            );
        }
    }

    let unmapped = task
        .process
        .addr_space
        .lock()
        .unwrap()
        .unmap_range(base + PAGE_SZ, PAGE_SZ, &kernel.pool)
        .expect("direct unmap should succeed");

    let addr_space = task.process.addr_space.lock().unwrap();
    assert_eq!(unmapped, 1);
    assert_eq!(addr_space.vm_map.regions.len(), 2);
    assert!(addr_space.vm_map.find(base).is_some());
    assert!(addr_space.vm_map.find(base + PAGE_SZ).is_none());
    assert!(addr_space.vm_map.find(base + PAGE_SZ * 2).is_some());
    let pt = addr_space.page_table.lock().unwrap();
    assert!(pt.contains_key(&base));
    assert!(!pt.contains_key(&(base + PAGE_SZ)));
    assert!(pt.contains_key(&(base + PAGE_SZ * 2)));
}

#[test]
// AGENT
fn fork_keeps_shared_writable_mapping_without_cow() {
    let kernel = Kernel::new(N_FRAMES);
    kernel.proc_init();
    let parent = kernel.cur_task(0).expect("init should be current");
    let page = 0x5200_0000;
    {
        let mut addr_space = parent.process.addr_space.lock().unwrap();
        addr_space
            .vm_map
            .insert(VmRegion::new(page, PAGE_SZ, VM_READ | VM_WRITE | VM_SHARED))
            .expect("test mapping should not overlap");
        addr_space.page_table.lock().unwrap().insert(
            page,
            PageTableEntry::new(44, PgFrame::with_rc(1), VM_READ | VM_WRITE | VM_SHARED),
        );
    }

    let child = kernel.do_fork(1).expect("fork should create child");
    let child_task = kernel
        .tasks
        .find(child)
        .expect("child should be registered");

    let child_addr_space = child_task.process.addr_space.lock().unwrap();
    let child_pte = child_addr_space
        .page_table
        .lock()
        .unwrap()
        .get(&page)
        .expect("child should inherit shared PTE")
        .clone();
    assert!(child_pte.writable);
    assert!(!child_pte.cow);
    assert_eq!(child_pte.frame.count(), 2);

    let parent_addr_space = parent.process.addr_space.lock().unwrap();
    let parent_pte = parent_addr_space
        .page_table
        .lock()
        .unwrap()
        .get(&page)
        .expect("parent should keep shared PTE")
        .clone();
    assert!(parent_pte.writable);
    assert!(!parent_pte.cow);
    assert_eq!(parent_pte.frame.count(), 2);
}

#[test]
// AGENT
fn fork_preserves_cloexec_and_epoll_state() {
    let kernel = Kernel::new(N_FRAMES);
    kernel.proc_init();
    let task = kernel.cur_task(0).expect("init should be current");
    write_user_c_string(&kernel, &task, 0x1000, "/tmp/fork-cloexec");
    let fd = kernel
        .dispatch_syscall(SYS_OPEN, 0x1000, O_CREAT | O_CLOEXEC, 0, 0, 0, 0)
        .expect("open should create cloexec file");
    let epfd = kernel
        .dispatch_syscall(SYS_EPOLL_CREATE, 1, 0, 0, 0, 0, 0)
        .expect("epoll_create should create epoll fd");
    let ev = EpEvent {
        events: EpEvent::IN,
        data: EpData { ptr: 0x55 },
    };
    kernel
        .dispatch_syscall(
            SYS_EPOLL_CTL,
            epfd,
            1,
            fd,
            &ev as *const EpEvent as usize,
            0,
            0,
        )
        .expect("epoll_ctl should register fd");

    let child = kernel.do_fork(1).expect("fork should create child");
    let child_task = kernel
        .tasks
        .find(child)
        .expect("child should be registered");

    match child_task.get_file(fd).expect("child should inherit fd") {
        FLike::File(f) => assert!(f.cloexec),
        _ => panic!("expected inherited regular file"),
    }
    let modified_ev = EpEvent {
        events: EpEvent::OUT,
        data: EpData { ptr: 0xaa },
    };
    kernel
        .dispatch_syscall(
            SYS_EPOLL_CTL,
            epfd,
            3,
            fd,
            &modified_ev as *const EpEvent as usize,
            0,
            0,
        )
        .expect("parent epoll_ctl should update shared epoll instance");
    let ep = child_task.process.ep_inst.lock().unwrap();
    let inst = ep.get(&epfd).expect("child should inherit epoll instance");
    assert_eq!(
        inst.events
            .lock()
            .unwrap()
            .get(&fd)
            .expect("child epoll instance should share watched fd")
            .data
            .ptr,
        0xaa
    );
}

#[test]
// AGENT
fn do_exec_commits_new_address_space_context_and_cloexec() {
    let kernel = Kernel::new(N_FRAMES);
    kernel.proc_init();
    install_test_exec(&kernel, "/bin/next");
    let task = kernel.cur_task(0).expect("init should be current");
    write_user_c_string(&kernel, &task, 0x2000, "/tmp/exec-keep");
    write_user_c_string(&kernel, &task, 0x3000, "/tmp/exec-close");
    let keep_fd = kernel
        .dispatch_syscall(SYS_OPEN, 0x2000, O_CREAT, 0, 0, 0, 0)
        .expect("open should create a non-cloexec fd");
    let close_fd = kernel
        .dispatch_syscall(SYS_OPEN, 0x3000, O_CREAT | O_CLOEXEC, 0, 0, 0, 0)
        .expect("open should create a cloexec fd");
    let old_token = task.vm_token();
    {
        let mut addr_space = task.process.addr_space.lock().unwrap();
        addr_space
            .map_region(
                VmRegion::new(0x5300_0000, PAGE_SZ, VM_READ | VM_WRITE),
                &kernel.pool,
            )
            .expect("old mapping should be created");
    }
    {
        let mut thd = task.thd_ctx.lock().unwrap();
        let ctx = thd.as_mut().expect("thread context should exist");
        ctx.uctx.set_ip(0x1111);
        ctx.uctx.set_sp(0x2222);
        ctx.clear_tid = 77;
    }

    kernel
        .do_exec(
            1,
            "/bin/next",
            vec![String::from("next")],
            vec![String::from("A=B")],
        )
        .expect("exec should commit the prepared image");

    assert_eq!(&*task.process.exec_path.lock().unwrap(), "/bin/next");
    assert!(task.get_file(keep_fd).is_some());
    assert!(task.get_file(close_fd).is_none());
    assert_ne!(task.vm_token(), old_token);
    {
        let addr_space = task.process.addr_space.lock().unwrap();
        assert!(addr_space.vm_map.find(0x5300_0000).is_none());
        let text = addr_space
            .vm_map
            .find(0x0040_0000)
            .expect("exec should map the text segment");
        assert_ne!(text.flags & VM_EXEC, 0);
        assert!(addr_space.vm_map.find(USR_STK_OFF).is_some());
        assert!(addr_space
            .vm_map
            .find(USR_STK_OFF + USR_STK_SZ - 1)
            .is_some());
        assert_eq!(addr_space.vm_map.brk, 0x0040_1000);
        assert!(addr_space
            .page_table
            .lock()
            .unwrap()
            .contains_key(&0x0040_0000));
        let mut payload = vec![0u8; TEST_EXEC_PAYLOAD.len()];
        addr_space
            .read_user_bytes(TEST_EXEC_ENTRY, &mut payload)
            .expect("exec payload should be readable");
        assert_eq!(payload, TEST_EXEC_PAYLOAD);
    }
    let sp = {
        let thd = task.thd_ctx.lock().unwrap();
        let ctx = thd.as_ref().expect("thread context should exist");
        let sp = *ctx
            .uctx
            .r
            .last()
            .expect("context should have a stack register") as usize;
        assert_eq!(ctx.uctx.ip, 0x0040_0000);
        assert!(sp >= USR_STK_OFF && sp <= USR_STK_OFF + USR_STK_SZ);
        assert_eq!(ctx.clear_tid, 0);
        assert!(ctx.sig_frames.is_empty());
        sp
    };
    {
        let addr_space = task.process.addr_space.lock().unwrap();
        let word = std::mem::size_of::<usize>();
        assert_eq!(sp & 0xF, 0);
        assert_eq!(addr_space.read_user_usize(sp).unwrap(), 1);
        let argv0 = addr_space.read_user_usize(sp + word).unwrap();
        assert_eq!(read_user_c_string(&addr_space, argv0), "next");
        assert_eq!(addr_space.read_user_usize(sp + word * 2).unwrap(), 0);
        let env0 = addr_space.read_user_usize(sp + word * 3).unwrap();
        assert_eq!(read_user_c_string(&addr_space, env0), "A=B");
        assert_eq!(addr_space.read_user_usize(sp + word * 4).unwrap(), 0);

        let mut aux_at = sp + word * 5;
        let mut saw_pagesz = false;
        let mut saw_entry = false;
        loop {
            let key = addr_space.read_user_usize(aux_at).unwrap();
            let value = addr_space.read_user_usize(aux_at + word).unwrap();
            aux_at += word * 2;
            if key == 0 {
                assert_eq!(value, 0);
                break;
            }
            match key as u8 {
                AT_PAGESZ => {
                    assert_eq!(value, PAGE_SZ);
                    saw_pagesz = true;
                }
                AT_ENTRY => {
                    assert_eq!(value, 0x0040_0000);
                    saw_entry = true;
                }
                _ => {}
            }
        }
        assert!(saw_pagesz);
        assert!(saw_entry);
    }
}

#[test]
// AGENT
fn do_exec_loads_registered_elf_segment_bytes_and_zeroes_bss() {
    let kernel = Kernel::new(N_FRAMES);
    kernel.proc_init();
    let payload = b"segment-bytes-cross-page";
    let vaddr = TEST_EXEC_ENTRY + PAGE_SZ - 8;
    let elf = elf_with_load_payload(
        TEST_EXEC_LOAD_OFFSET + PAGE_SZ - 8,
        vaddr,
        payload,
        payload.len() + 32,
        0x5,
    );
    kernel
        .install_exec_file("/bin/cross-page", elf)
        .expect("test exec file should install");
    let task = kernel.cur_task(0).expect("init should be current");

    kernel
        .do_exec(
            1,
            "/bin/cross-page",
            vec![String::from("cross-page")],
            Vec::new(),
        )
        .expect("exec should load registered ELF bytes");

    {
        let addr_space = task.process.addr_space.lock().unwrap();
        let text = addr_space
            .vm_map
            .find(vaddr)
            .expect("exec should map cross-page load segment");
        assert_eq!(text.flags & VM_WRITE, 0);
        assert_eq!(text.flags & VM_READ, VM_READ);
        assert_eq!(text.flags & VM_EXEC, VM_EXEC);

        let mut loaded = vec![0u8; payload.len()];
        addr_space
            .read_user_bytes(vaddr, &mut loaded)
            .expect("loaded segment bytes should be readable");
        assert_eq!(loaded, payload);

        let mut bss = [0xffu8; 16];
        addr_space
            .read_user_bytes(vaddr + payload.len(), &mut bss)
            .expect("bss tail should be readable");
        assert_eq!(bss, [0u8; 16]);
    }
    {
        let thd = task.thd_ctx.lock().unwrap();
        let ctx = thd.as_ref().expect("thread context should exist");
        assert_eq!(ctx.uctx.ip, vaddr as u64);
    }
}

#[test]
// AGENT
fn do_exec_loads_updated_regular_file_contents() {
    let kernel = Kernel::new(N_FRAMES);
    kernel.proc_init();
    let old_payload = b"old path-backed payload";
    let new_payload = b"updated path-backed payload";
    let old_elf = elf_with_load_payload(
        TEST_EXEC_LOAD_OFFSET,
        TEST_EXEC_ENTRY,
        old_payload,
        PAGE_SZ,
        0x5,
    );
    let new_elf = elf_with_load_payload(
        TEST_EXEC_LOAD_OFFSET,
        TEST_EXEC_ENTRY,
        new_payload,
        PAGE_SZ,
        0x5,
    );
    kernel
        .install_file("/bin/live", old_elf, true)
        .expect("regular executable file should install");
    kernel
        .write_file_at("/bin/live", 0, &new_elf)
        .expect("regular file write should update shared contents");
    let task = kernel.cur_task(0).expect("init should be current");

    kernel
        .do_exec(1, "/bin/live", vec![String::from("live")], Vec::new())
        .expect("exec should load the updated regular file");

    let addr_space = task.process.addr_space.lock().unwrap();
    let mut loaded = vec![0u8; new_payload.len()];
    addr_space
        .read_user_bytes(TEST_EXEC_ENTRY, &mut loaded)
        .expect("updated payload should be readable");
    assert_eq!(loaded, new_payload);
    assert_ne!(loaded, old_payload);
}

#[test]
// AGENT
fn cloned_thread_observes_exec_token_from_shared_address_space() {
    let kernel = Kernel::new(N_FRAMES);
    kernel.proc_init();
    install_test_exec(&kernel, "/bin/next");
    let task = kernel.cur_task(0).expect("init should be current");
    let old_token = task.vm_token();
    let thread_task = kernel
        .tasks
        .clone_thread(&task, (USR_STK_OFF + USR_STK_SZ) as u64, 0xabc, 0);

    assert!(Arc::ptr_eq(
        &task.process.addr_space,
        &thread_task.process.addr_space
    ));
    assert_eq!(thread_task.vm_token(), old_token);

    kernel
        .do_exec(1, "/bin/next", vec![String::from("next")], Vec::new())
        .expect("exec should replace the shared address-space image");

    let new_token = task.vm_token();
    assert_ne!(new_token, old_token);
    assert_eq!(thread_task.vm_token(), new_token);
}

#[test]
// AGENT
fn fork_from_cloned_thread_uses_shared_process_state_and_thread_context() {
    let kernel = Kernel::new(N_FRAMES);
    kernel.proc_init();
    let task = kernel.cur_task(0).expect("init should be current");
    let thread_task = kernel
        .tasks
        .clone_thread(&task, (USR_STK_OFF + USR_STK_SZ) as u64, 0xabc, 0);

    *task.process.cwd.lock().unwrap() = String::from("/leader/after-clone");
    task.process
        .debug_fds
        .lock()
        .unwrap()
        .push(String::from("leader-fd"));
    write_user_c_string(&kernel, &task, 0x5600, "/tmp/thread-shared-fd");
    let fd = kernel
        .dispatch_syscall(SYS_OPEN, 0x5600, O_CREAT, 0, 0, 0, 0)
        .expect("open should add a process-shared fd");
    let epfd = kernel
        .dispatch_syscall(SYS_EPOLL_CREATE, 4, 0, 0, 0, 0, 0)
        .expect("epoll_create should add a process-shared epoll instance");
    let act = UserSigAction {
        sa_handler: 0x7777,
        sa_sigaction: 0,
        sa_mask: 0,
        sa_flags: 0,
    };
    kernel
        .dispatch_syscall(
            SYS_SIGACTION,
            SIGUSR1 as usize,
            &act as *const UserSigAction as usize,
            0,
            0,
            0,
            0,
        )
        .expect("sigaction should update process-shared dispositions");

    {
        let mut thd = thread_task.thd_ctx.lock().unwrap();
        let ctx = thd.as_mut().expect("thread context should exist");
        ctx.uctx.set_ip(0x4444);
        ctx.uctx.r[3] = 0x9999;
        ctx.clear_tid = 0x3333;
        ctx.smask = 1u64 << SIGUSR1;
    }
    *thread_task.sig_mask.lock().unwrap() = 1u64 << SIGUSR1;

    let child = kernel
        .do_fork(thread_task.id())
        .expect("fork from cloned thread should create a child process");
    let child_task = kernel
        .tasks
        .find(child)
        .expect("child should be registered");

    assert!(!Arc::ptr_eq(&task.process, &child_task.process));
    assert_eq!(
        child_task
            .process
            .parent
            .lock()
            .unwrap()
            .as_ref()
            .expect("child should be parented to process leader")
            .id(),
        task.id()
    );
    assert_eq!(
        &*child_task.process.cwd.lock().unwrap(),
        "/leader/after-clone"
    );
    assert_eq!(
        child_task.process.debug_fds.lock().unwrap().as_slice(),
        &[String::from("leader-fd")]
    );
    assert!(child_task.get_file(fd).is_some());
    assert!(child_task
        .process
        .ep_inst
        .lock()
        .unwrap()
        .contains_key(&epfd));
    assert_eq!(
        child_task
            .process
            .sig_state
            .lock()
            .unwrap()
            .get_action(SIGUSR1)
            .handler,
        0x7777
    );
    assert_eq!(*child_task.sig_mask.lock().unwrap(), 1u64 << SIGUSR1);
    {
        let thd = child_task.thd_ctx.lock().unwrap();
        let ctx = thd.as_ref().expect("child context should exist");
        assert_eq!(ctx.uctx.ip, 0x4444);
        assert_eq!(ctx.uctx.r[0], 0);
        assert_eq!(ctx.uctx.r[3], 0x9999);
        assert_eq!(ctx.uctx.r[N_REGS - 2], 0xabc);
        assert_eq!(ctx.clear_tid, 0x3333);
        assert_eq!(ctx.smask, 1u64 << SIGUSR1);
    }
}

#[test]
// AGENT
fn do_exec_failure_preserves_old_image_and_cloexec_fds() {
    let kernel = Kernel::new(N_FRAMES);
    kernel.proc_init();
    install_test_exec(&kernel, "/bin/too-big");
    let task = kernel.cur_task(0).expect("init should be current");
    *task.process.exec_path.lock().unwrap() = String::from("/bin/old");
    {
        let mut addr_space = task.process.addr_space.lock().unwrap();
        addr_space
            .map_region(
                VmRegion::new(0x5400_0000, PAGE_SZ, VM_READ | VM_WRITE),
                &kernel.pool,
            )
            .expect("old mapping should be created");
        addr_space
            .write_user_bytes(0x5400_0000, b"/tmp/exec-failure-close\0", &kernel.pool)
            .expect("open path should live in the old mapping");
    }
    let close_fd = kernel
        .dispatch_syscall(SYS_OPEN, 0x5400_0000, O_CREAT | O_CLOEXEC, 0, 0, 0, 0)
        .expect("open should create a cloexec fd");
    {
        let mut thd = task.thd_ctx.lock().unwrap();
        let ctx = thd.as_mut().expect("thread context should exist");
        ctx.uctx.set_ip(0x1111);
        ctx.uctx.set_sp(0x2222);
        ctx.clear_tid = 123;
    }
    let old_token = task.vm_token();
    let free_before = kernel.pool.free_count();

    let err = kernel
        .do_exec(1, "/bin/too-big", vec!["x".repeat(USR_STK_SZ)], Vec::new())
        .expect_err("oversized initial stack should fail before commit");

    assert_eq!(err, "e2big");
    assert_eq!(kernel.pool.free_count(), free_before);
    assert_eq!(&*task.process.exec_path.lock().unwrap(), "/bin/old");
    assert_eq!(task.vm_token(), old_token);
    match task
        .get_file(close_fd)
        .expect("cloexec fd should survive failed exec")
    {
        FLike::File(f) => assert!(f.cloexec),
        _ => panic!("expected regular file"),
    }
    {
        let addr_space = task.process.addr_space.lock().unwrap();
        assert!(addr_space.vm_map.find(0x5400_0000).is_some());
        assert!(addr_space.vm_map.find(0x0040_0000).is_none());
        assert_eq!(addr_space.rss_pages(), 1);
    }
    {
        let thd = task.thd_ctx.lock().unwrap();
        let ctx = thd.as_ref().expect("thread context should exist");
        assert_eq!(ctx.uctx.ip, 0x1111);
        assert_eq!(*ctx.uctx.r.last().unwrap(), 0x2222);
        assert_eq!(ctx.clear_tid, 123);
    }
}

#[test]
// AGENT
fn do_exec_rejects_unregistered_exec_file_without_commit() {
    let kernel = Kernel::new(N_FRAMES);
    kernel.proc_init();
    let task = kernel.cur_task(0).expect("init should be current");
    let (close_fd, old_token, free_before) = seed_failed_exec_state(&kernel, &task, 0x5500_0000);

    let err = kernel
        .do_exec(1, "/bin/missing", vec![String::from("missing")], Vec::new())
        .expect_err("unregistered exec image should fail");

    assert_eq!(err, "enoent");
    assert_failed_exec_state_preserved(
        &kernel,
        &task,
        close_fd,
        old_token,
        free_before,
        0x5500_0000,
    );
}

#[test]
// AGENT
fn do_exec_rejects_non_executable_file_without_commit() {
    let kernel = Kernel::new(N_FRAMES);
    kernel.proc_init();
    kernel
        .install_file("/bin/plain", test_exec_elf(), false)
        .expect("plain file should install");
    let task = kernel.cur_task(0).expect("init should be current");
    let (close_fd, old_token, free_before) = seed_failed_exec_state(&kernel, &task, 0x5600_0000);

    let err = kernel
        .do_exec(1, "/bin/plain", vec![String::from("plain")], Vec::new())
        .expect_err("non-executable regular file should fail");

    assert_eq!(err, "eacces");
    assert_failed_exec_state_preserved(
        &kernel,
        &task,
        close_fd,
        old_token,
        free_before,
        0x5600_0000,
    );
}

#[test]
// AGENT
fn do_exec_rejects_directory_without_commit() {
    let kernel = Kernel::new(N_FRAMES);
    kernel.proc_init();
    kernel
        .install_directory("/bin/dir")
        .expect("directory node should install");
    let task = kernel.cur_task(0).expect("init should be current");
    let (close_fd, old_token, free_before) = seed_failed_exec_state(&kernel, &task, 0x5700_0000);

    let err = kernel
        .do_exec(1, "/bin/dir", vec![String::from("dir")], Vec::new())
        .expect_err("directory exec should fail");

    assert_eq!(err, "eisdir");
    assert_failed_exec_state_preserved(
        &kernel,
        &task,
        close_fd,
        old_token,
        free_before,
        0x5700_0000,
    );
}

#[test]
// AGENT
fn do_exec_rejects_invalid_elf_without_commit() {
    let kernel = Kernel::new(N_FRAMES);
    kernel.proc_init();
    kernel
        .install_file("/bin/bad", vec![0; 64], true)
        .expect("invalid executable file should install");
    let task = kernel.cur_task(0).expect("init should be current");
    let (close_fd, old_token, free_before) = seed_failed_exec_state(&kernel, &task, 0x5800_0000);

    let err = kernel
        .do_exec(1, "/bin/bad", vec![String::from("bad")], Vec::new())
        .expect_err("invalid ELF should fail");

    assert_eq!(err, "bad_magic");
    assert_failed_exec_state_preserved(
        &kernel,
        &task,
        close_fd,
        old_token,
        free_before,
        0x5800_0000,
    );
}

#[test]
// AGENT
fn syscall_exec_reads_user_memory_and_commits_do_exec() {
    let kernel = Kernel::new(N_FRAMES);
    kernel.proc_init();
    install_test_exec(&kernel, "/bin/next");
    let task = kernel.cur_task(0).expect("init should be current");
    write_user_c_string(&kernel, &task, 0x6000, "/tmp/sys-exec-close");
    let close_fd = kernel
        .dispatch_syscall(SYS_OPEN, 0x6000, O_CREAT | O_CLOEXEC, 0, 0, 0, 0)
        .expect("open should create a cloexec fd");
    let old_token = task.vm_token();

    const USER_BASE: usize = 0x1000_0000;
    const PATH: usize = USER_BASE;
    const ARGV: usize = USER_BASE + 0x100;
    const ENVP: usize = USER_BASE + 0x200;
    const ARG0: usize = USER_BASE + PAGE_SZ;
    const ARG1: usize = USER_BASE + PAGE_SZ + 0x100;
    const ENV0: usize = USER_BASE + PAGE_SZ * 2;
    {
        let mut addr_space = task.process.addr_space.lock().unwrap();
        addr_space
            .map_region(
                VmRegion::new(USER_BASE, PAGE_SZ * 3, VM_READ | VM_WRITE),
                &kernel.pool,
            )
            .expect("user argument region should map");
        addr_space
            .write_user_bytes(PATH, b"/bin/next\0", &kernel.pool)
            .expect("path should be written");
        addr_space
            .write_user_bytes(ARG0, b"next\0", &kernel.pool)
            .expect("argv[0] should be written");
        addr_space
            .write_user_bytes(ARG1, b"--flag\0", &kernel.pool)
            .expect("argv[1] should be written");
        addr_space
            .write_user_bytes(ENV0, b"A=B\0", &kernel.pool)
            .expect("envp[0] should be written");
        addr_space
            .write_user_bytes(ARGV, &usize_array_bytes(&[ARG0, ARG1, 0]), &kernel.pool)
            .expect("argv vector should be written");
        addr_space
            .write_user_bytes(ENVP, &usize_array_bytes(&[ENV0, 0]), &kernel.pool)
            .expect("envp vector should be written");
    }

    kernel
        .dispatch_syscall(SYS_EXEC, PATH, ARGV, ENVP, 0, 0, 0)
        .expect("exec syscall should commit via do_exec");

    assert_eq!(&*task.process.exec_path.lock().unwrap(), "/bin/next");
    assert!(task.get_file(close_fd).is_none());
    assert_ne!(task.vm_token(), old_token);
    {
        let addr_space = task.process.addr_space.lock().unwrap();
        assert!(addr_space.vm_map.find(USER_BASE).is_none());
        assert!(addr_space.vm_map.find(0x0040_0000).is_some());
        assert!(addr_space.vm_map.find(USR_STK_OFF).is_some());
    }
}

#[test]
// AGENT
fn syscall_exec_faults_on_unmapped_user_path_without_commit() {
    let kernel = Kernel::new(N_FRAMES);
    kernel.proc_init();
    let task = kernel.cur_task(0).expect("init should be current");
    *task.process.exec_path.lock().unwrap() = String::from("/bin/old");
    let old_token = task.vm_token();

    let err = kernel
        .dispatch_syscall(SYS_EXEC, 0x2000_0000, 0, 0, 0, 0, 0)
        .expect_err("unmapped user path should fault");

    assert_eq!(err, "efault");
    assert_eq!(&*task.process.exec_path.lock().unwrap(), "/bin/old");
    assert_eq!(task.vm_token(), old_token);
}

#[test]
// AGENT
fn fork_returns_eagain_when_process_table_is_full() {
    let kernel = Kernel::new(N_FRAMES);
    kernel.proc_init();
    for _ in kernel.tasks.count()..N_PROC {
        kernel.tasks.spawn("filler");
    }

    let err = kernel
        .do_fork(1)
        .expect_err("fork should fail when process table is full");

    assert_eq!(err, "eagain");
    assert_eq!(kernel.tasks.count(), N_PROC);
}

#[test]
// AGENT
fn concurrent_fork_respects_process_table_limit() {
    let tasks = Arc::new(TaskTable::new());
    let root = tasks.spawn_root();
    for _ in tasks.count()..(N_PROC - 1) {
        tasks.spawn("filler");
    }

    let workers = 8;
    let barrier = Arc::new(Barrier::new(workers));
    let handles: Vec<_> = (0..workers)
        .map(|_| {
            let tasks = tasks.clone();
            let root = root.clone();
            let barrier = barrier.clone();
            thread::spawn(move || {
                barrier.wait();
                tasks.fork_task(&root).map(|task| task.id())
            })
        })
        .collect();

    let results: Vec<_> = handles
        .into_iter()
        .map(|handle| handle.join().expect("fork worker should not panic"))
        .collect();
    let successes = results.iter().filter(|result| result.is_ok()).count();

    assert_eq!(successes, 1);
    assert!(results
        .iter()
        .filter_map(|result| result.as_ref().err())
        .all(|err| *err == "eagain"));
    assert_eq!(tasks.count(), N_PROC);
    assert_eq!(root.n_children(), 1);
}

#[test]
// AGENT
fn default_signal_action_terminates_current_task() {
    let kernel = Kernel::new(N_FRAMES);
    kernel.proc_init();
    let child = kernel.do_fork(1).expect("fork should create child task");

    kernel
        .dispatch_syscall(SYS_KILL, 1, SIGUSR1 as usize, 0, 0, 0, 0)
        .expect("kill should enqueue and deliver the signal");

    let init = kernel
        .tasks
        .find(1)
        .expect("init task should still be reaped later");
    assert!(init.done());
    assert_eq!(
        *init.process.exit_reason.lock().unwrap(),
        Some(ExitReason::Signal(SIGUSR1 as u8))
    );
    assert_eq!(
        kernel.cur_task(0).expect("child should be scheduled").id(),
        child
    );
}

#[test]
// AGENT
fn custom_signal_handler_updates_context_and_sigreturn_restores_it() {
    let kernel = Kernel::new(N_FRAMES);
    kernel.proc_init();
    let task = kernel.cur_task(0).expect("init should be current");
    {
        let mut thd = task.thd_ctx.lock().unwrap();
        let ctx = thd.as_mut().expect("thread context should exist");
        ctx.uctx.set_ip(0x1234);
        ctx.uctx.r[3] = 0x7777;
    }

    let act = UserSigAction {
        sa_handler: 0x5555,
        sa_sigaction: 0,
        sa_mask: 1u64 << SIGUSR1,
        sa_flags: 0,
    };
    let act_addr = &act as *const UserSigAction as usize;
    kernel
        .dispatch_syscall(SYS_SIGACTION, SIGUSR1 as usize, act_addr, 0, 0, 0, 0)
        .expect("sigaction should install handler");

    kernel
        .dispatch_syscall(SYS_KILL, 1, SIGUSR1 as usize, 0, 0, 0, 0)
        .expect("kill should enter signal handler");

    {
        let thd = task.thd_ctx.lock().unwrap();
        let ctx = thd.as_ref().expect("thread context should exist");
        assert_eq!(ctx.uctx.ip, 0x5555);
        assert_eq!(ctx.uctx.r[0], SIGUSR1 as u64);
        assert_eq!(ctx.uctx.r[1], u64::MAX);
        assert_eq!(ctx.uctx.r[2], 0x1234);
        assert_eq!(ctx.sig_frames.len(), 1);
        assert_ne!(*task.sig_mask.lock().unwrap() & (1u64 << SIGUSR1), 0);
    }

    kernel
        .dispatch_syscall(SYS_SIGRETURN, 0, 0, 0, 0, 0, 0)
        .expect("sigreturn should restore interrupted context");

    let thd = task.thd_ctx.lock().unwrap();
    let ctx = thd.as_ref().expect("thread context should exist");
    assert_eq!(ctx.uctx.ip, 0x1234);
    assert_eq!(ctx.uctx.r[3], 0x7777);
    assert_eq!(ctx.sig_frames.len(), 0);
    assert_eq!(*task.sig_mask.lock().unwrap(), 0);
}

#[test]
fn forked_task_enters_run_queue_and_receives_cpu_after_slice() {
    let kernel = Kernel::new(N_FRAMES);
    kernel.proc_init();
    let child = kernel.do_fork(1).expect("fork should create child task");

    assert_eq!(kernel.run_queue.len(), 1);
    assert_eq!(kernel.cur_task(0).expect("init should run first").id(), 1);

    for _ in 0..10 {
        kernel.schedule_tick(0);
    }

    let current = kernel
        .cur_task(0)
        .expect("scheduler should pick runnable child");
    assert_eq!(current.id(), child);
    assert_eq!(current.sched_state(), TaskRunState::Running);
}

#[test]
fn single_current_task_keeps_running_across_ticks() {
    let kernel = Kernel::new(N_FRAMES);
    kernel.proc_init();

    for _ in 0..25 {
        kernel.schedule_tick(0);
    }

    assert_eq!(
        kernel.cur_task(0).expect("init should remain current").id(),
        1
    );
    assert_eq!(kernel.run_queue.len(), 0);
}

#[test]
fn exiting_current_task_switches_to_next_runnable_task() {
    let kernel = Kernel::new(N_FRAMES);
    kernel.proc_init();
    let child = kernel.do_fork(1).expect("fork should create child task");

    kernel
        .dispatch_syscall(SYS_EXIT, 0, 0, 0, 0, 0, 0)
        .expect("exit should succeed");

    let current = kernel
        .cur_task(0)
        .expect("child should run after init exits");
    assert_eq!(current.id(), child);
    assert_eq!(current.sched_state(), TaskRunState::Running);
}

#[test]
// AGENT
fn exit_without_current_task_returns_esrch() {
    let kernel = Kernel::new(N_FRAMES);
    kernel.proc_init();
    kernel.set_cur(0, None);
    kernel.run_queue.clear_current();

    let err = kernel
        .dispatch_syscall(SYS_EXIT, 0, 0, 0, 0, 0, 0)
        .expect_err("exit without a current task should fail explicitly");

    assert_eq!(err, "esrch");
}

#[test]
// AGENT
fn wait4_reaps_child_and_writes_exit_status() {
    const STATUS_ADDR: usize = 0x7000;

    let kernel = Kernel::new(N_FRAMES);
    kernel.proc_init();
    let parent = kernel.cur_task(0).expect("init should be current");
    let child_id = kernel
        .do_fork(parent.id())
        .expect("fork should create child");
    let child = kernel
        .tasks
        .find(child_id)
        .expect("child should be registered");

    kernel.run_queue.remove(child_id);
    kernel.run_queue.set_current(child_id);
    kernel.set_cur(0, Some(child.clone()));
    kernel
        .dispatch_syscall(SYS_EXIT, 7, 0, 0, 0, 0, 0)
        .expect("child exit should succeed");

    {
        let mut addr_space = parent.process.addr_space.lock().unwrap();
        addr_space
            .map_region(
                VmRegion::new(STATUS_ADDR, PAGE_SZ, VM_READ | VM_WRITE),
                &kernel.pool,
            )
            .expect("status page should map");
    }
    kernel.run_queue.set_current(parent.id());
    kernel.set_cur(0, Some(parent.clone()));

    let waited = kernel
        .dispatch_syscall(SYS_WAIT4, child_id, STATUS_ADDR, 0, 0, 0, 0)
        .expect("wait4 should reap the exited child");

    assert_eq!(waited, child_id);
    assert!(kernel.tasks.find(child_id).is_none());
    assert!(!parent
        .process
        .subtasks
        .lock()
        .unwrap()
        .iter()
        .any(|task| task.id() == child_id));

    let mut status = [0u8; 4];
    parent
        .process
        .addr_space
        .lock()
        .unwrap()
        .read_user_bytes(STATUS_ADDR, &mut status)
        .expect("wait status should be readable");
    assert_eq!(u32::from_ne_bytes(status), 7 << 8);
}

#[test]
// AGENT
fn exit_releases_process_resources_before_wait_reaps_zombie() {
    const STATUS_ADDR: usize = 0x8000;
    const CHILD_MAPPING: usize = 0x5900_0000;

    let kernel = Kernel::new(N_FRAMES);
    kernel.proc_init();
    let parent = kernel.cur_task(0).expect("init should be current");
    let child_id = kernel
        .do_fork(parent.id())
        .expect("fork should create child");
    let child = kernel
        .tasks
        .find(child_id)
        .expect("child should be registered");
    let thread_task =
        kernel
            .tasks
            .clone_thread(&child, (USR_STK_OFF + USR_STK_SZ) as u64, 0xabc, 0xdead);
    let thread_id = thread_task.id();

    {
        let mut addr_space = child.process.addr_space.lock().unwrap();
        addr_space
            .map_region(
                VmRegion::new(CHILD_MAPPING, PAGE_SZ, VM_READ | VM_WRITE),
                &kernel.pool,
            )
            .expect("child mapping should be created");
    }
    child
        .process
        .debug_fds
        .lock()
        .unwrap()
        .push(String::from("debug-fd"));
    let sem = kernel
        .get_sem(0x5150, 1, 0)
        .expect("semaphore should be created");
    child.process.sem_ctx.lock().unwrap().add(sem);
    child
        .process
        .shm_ctx
        .lock()
        .unwrap()
        .add(kernel.get_shm(0x6160, 1));

    kernel.run_queue.remove(child_id);
    kernel.run_queue.set_current(child_id);
    kernel.set_cur(0, Some(child.clone()));
    write_user_c_string(&kernel, &child, CHILD_MAPPING, "/tmp/child-owned-fd");
    let _fd = kernel
        .dispatch_syscall(SYS_OPEN, CHILD_MAPPING, O_CREAT, 0, 0, 0, 0)
        .expect("child open should create a file descriptor");
    let _epfd = kernel
        .dispatch_syscall(SYS_EPOLL_CREATE, 4, 0, 0, 0, 0, 0)
        .expect("child epoll instance should be created");
    child.process.sig_state.lock().unwrap().sig_raise(SIGUSR1);
    child.send_sig(SIGUSR1 as i32, -1);

    let futex = child.get_futex();
    let futex_word = Arc::new(AtomicU32::new(1));
    let uaddr = futex_word.as_ref() as *const AtomicU32 as usize;
    let waiter_futex = futex.clone();
    let waiter_word = futex_word.clone();
    let waiter = thread::spawn(move || {
        waiter_futex.wait(uaddr, 1, waiter_word.as_ref(), Some(Duration::from_secs(5)))
    });
    let wait_until = Instant::now() + Duration::from_secs(1);
    while Instant::now() < wait_until {
        if futex.pending_at(uaddr) == 1 {
            break;
        }
        thread::sleep(Duration::from_millis(1));
    }
    assert_eq!(futex.pending_at(uaddr), 1);

    let rss_before_exit = child.process.addr_space.lock().unwrap().rss_pages();
    let free_before_exit = kernel.pool.free_count();
    assert!(rss_before_exit > 0);
    assert!(child.kstk.lock().unwrap().is_some());
    assert!(child.thd_ctx.lock().unwrap().is_some());
    assert!(thread_task.thd_ctx.lock().unwrap().is_some());

    kernel
        .dispatch_syscall(SYS_EXIT, 11, 0, 0, 0, 0, 0)
        .expect("child exit should succeed");

    assert!(waiter
        .join()
        .expect("futex waiter should not panic")
        .is_ok());
    assert!(child.done());
    assert_eq!(child.wait_status(), 11 << 8);
    assert_eq!(child.process.addr_space.lock().unwrap().rss_pages(), 0);
    assert_eq!(kernel.pool.free_count(), free_before_exit + rss_before_exit);
    assert!(child.process.debug_fds.lock().unwrap().is_empty());
    assert!(child.process.files.lock().unwrap().is_empty());
    assert!(child.process.ep_inst.lock().unwrap().is_empty());
    assert!(child.process.sig_queue.lock().unwrap().is_empty());
    assert_eq!(child.process.sig_state.lock().unwrap().pending, 0);
    assert!(child.process.sem_ctx.lock().unwrap().arrays.is_empty());
    assert!(child.process.shm_ctx.lock().unwrap().ids.is_empty());
    assert_eq!(*child.sig_mask.lock().unwrap(), 0);
    assert!(child.kstk.lock().unwrap().is_none());
    assert!(child.thd_ctx.lock().unwrap().is_none());
    assert!(thread_task.thd_ctx.lock().unwrap().is_none());
    assert_eq!(futex.pending_at(uaddr), 0);
    assert!(kernel.tasks.find(child_id).is_some());
    assert!(kernel.tasks.find(thread_id).is_some());

    {
        let mut addr_space = parent.process.addr_space.lock().unwrap();
        addr_space
            .map_region(
                VmRegion::new(STATUS_ADDR, PAGE_SZ, VM_READ | VM_WRITE),
                &kernel.pool,
            )
            .expect("status page should map");
    }
    kernel.run_queue.set_current(parent.id());
    kernel.set_cur(0, Some(parent.clone()));

    let waited = kernel
        .dispatch_syscall(SYS_WAIT4, child_id, STATUS_ADDR, 0, 0, 0, 0)
        .expect("wait4 should reap the exited child");

    assert_eq!(waited, child_id);
    assert!(kernel.tasks.find(child_id).is_none());
    assert!(kernel.tasks.find(thread_id).is_none());
    let mut status = [0u8; 4];
    parent
        .process
        .addr_space
        .lock()
        .unwrap()
        .read_user_bytes(STATUS_ADDR, &mut status)
        .expect("wait status should be readable");
    assert_eq!(u32::from_ne_bytes(status), 11 << 8);
}

#[test]
// AGENT
fn wait4_ignores_unrelated_zombies() {
    let kernel = Kernel::new(N_FRAMES);
    kernel.proc_init();
    let orphan = kernel.tasks.spawn("orphan");
    orphan.exit_proc(ExitReason::Code(3));

    let err = kernel
        .dispatch_syscall(SYS_WAIT4, usize::MAX, 0, 1, 0, 0, 0)
        .expect_err("wait4 should not reap a task that is not our child");

    assert_eq!(err, "echild");
    assert!(kernel.tasks.find(orphan.id()).is_some());
}

#[test]
fn futex_wait_returns_eagain_when_value_changed() {
    let kernel = Kernel::new(N_FRAMES);
    kernel.proc_init();
    let futex_word = AtomicU32::new(0);
    let uaddr = &futex_word as *const AtomicU32 as usize;

    let err = kernel
        .dispatch_syscall(SYS_FUTEX, uaddr, 0, 1, 0, 0, 0)
        .expect_err("wait should not sleep when the futex word differs");

    assert_eq!(err, "eagain");
}

#[test]
fn futex_wait_sleeps_until_wake() {
    let kernel = Arc::new(Kernel::new(N_FRAMES));
    kernel.proc_init();
    let futex_word = Arc::new(AtomicU32::new(1));
    let timeout = Arc::new([1usize, 0usize]);
    let uaddr = futex_word.as_ref() as *const AtomicU32 as usize;
    let timeout_addr = timeout.as_ptr() as usize;

    let waiter_kernel = kernel.clone();
    let waiter_word = futex_word.clone();
    let waiter = thread::spawn(move || {
        let uaddr = waiter_word.as_ref() as *const AtomicU32 as usize;
        waiter_kernel
            .dispatch_syscall(SYS_FUTEX, uaddr, 0, 1, timeout_addr, 0, 0)
            .expect("wait should be woken before the timeout")
    });

    thread::sleep(Duration::from_millis(25));
    futex_word.store(0, Ordering::SeqCst);
    let woken = kernel
        .dispatch_syscall(SYS_FUTEX, uaddr, 1, 1, 0, 0, 0)
        .expect("wake should succeed");

    assert_eq!(woken, 1);
    assert_eq!(waiter.join().expect("waiter thread should finish"), 0);
}

#[test]
fn futex_wake_zero_wakes_nobody() {
    let kernel = Kernel::new(N_FRAMES);
    kernel.proc_init();
    let futex_word = AtomicU32::new(1);
    let uaddr = &futex_word as *const AtomicU32 as usize;

    let woken = kernel
        .dispatch_syscall(SYS_FUTEX, uaddr, 1, 0, 0, 0, 0)
        .expect("wake with count zero should succeed");

    assert_eq!(woken, 0);
}

#[test]
fn futex_requeue_wakes_and_moves_waiters() {
    let kernel = Arc::new(Kernel::new(N_FRAMES));
    kernel.proc_init();
    let src = Arc::new(AtomicU32::new(1));
    let dst = Arc::new(AtomicU32::new(0));
    let timeout = Arc::new([1usize, 0usize]);
    let src_addr = src.as_ref() as *const AtomicU32 as usize;
    let dst_addr = dst.as_ref() as *const AtomicU32 as usize;
    let timeout_addr = timeout.as_ptr() as usize;

    let first_kernel = kernel.clone();
    let first_src = src.clone();
    let first = thread::spawn(move || {
        let src_addr = first_src.as_ref() as *const AtomicU32 as usize;
        first_kernel
            .dispatch_syscall(SYS_FUTEX, src_addr, 0, 1, timeout_addr, 0, 0)
            .expect("first waiter should be woken")
    });

    let second_kernel = kernel.clone();
    let second_src = src.clone();
    let second = thread::spawn(move || {
        let src_addr = second_src.as_ref() as *const AtomicU32 as usize;
        second_kernel
            .dispatch_syscall(SYS_FUTEX, src_addr, 0, 1, timeout_addr, 0, 0)
            .expect("second waiter should be requeued then woken")
    });

    thread::sleep(Duration::from_millis(25));
    let affected = kernel
        .dispatch_syscall(SYS_FUTEX, src_addr, 3, 1, 1, dst_addr, 0)
        .expect("requeue should succeed");
    // AGENT: FUTEX_REQUEUE returns the number of waiters directly woken, not
    // the number moved to the destination futex.
    assert_eq!(affected, 1);

    let woken = kernel
        .dispatch_syscall(SYS_FUTEX, dst_addr, 1, 1, 0, 0, 0)
        .expect("wake on destination should find the requeued waiter");
    assert_eq!(woken, 1);
    assert_eq!(first.join().expect("first waiter should finish"), 0);
    assert_eq!(second.join().expect("second waiter should finish"), 0);
}

#[test]
fn futex_wake_op_updates_uaddr2_and_conditionally_wakes_both_queues() {
    const FUTEX_OP_ADD: usize = 1;
    const FUTEX_OP_CMP_EQ: usize = 0;

    let kernel = Arc::new(Kernel::new(N_FRAMES));
    kernel.proc_init();
    let src = Arc::new(AtomicU32::new(1));
    let dst = Arc::new(AtomicU32::new(0));
    let timeout = Arc::new([1usize, 0usize]);
    let src_addr = src.as_ref() as *const AtomicU32 as usize;
    let dst_addr = dst.as_ref() as *const AtomicU32 as usize;
    let timeout_addr = timeout.as_ptr() as usize;

    let first_kernel = kernel.clone();
    let first_src = src.clone();
    let first = thread::spawn(move || {
        let src_addr = first_src.as_ref() as *const AtomicU32 as usize;
        first_kernel
            .dispatch_syscall(SYS_FUTEX, src_addr, 0, 1, timeout_addr, 0, 0)
            .expect("source waiter should be woken")
    });

    let second_kernel = kernel.clone();
    let second_dst = dst.clone();
    let second = thread::spawn(move || {
        let dst_addr = second_dst.as_ref() as *const AtomicU32 as usize;
        second_kernel
            .dispatch_syscall(SYS_FUTEX, dst_addr, 0, 0, timeout_addr, 0, 0)
            .expect("destination waiter should be conditionally woken")
    });

    thread::sleep(Duration::from_millis(25));
    let encoded = (FUTEX_OP_ADD << 28) | (FUTEX_OP_CMP_EQ << 24) | (1 << 12);
    let woken = kernel
        .dispatch_syscall(SYS_FUTEX, src_addr, 5, 1, 1, dst_addr, encoded)
        .expect("wake-op should succeed");

    assert_eq!(dst.load(Ordering::SeqCst), 1);
    assert_eq!(woken, 2);
    assert_eq!(first.join().expect("source waiter should finish"), 0);
    assert_eq!(second.join().expect("destination waiter should finish"), 0);
}

#[test]
fn futex_wake_op_sign_extends_oparg_and_cmparg() {
    const FUTEX_OP_ADD: usize = 1;
    const FUTEX_OP_CMP_EQ: usize = 0;

    let kernel = Kernel::new(N_FRAMES);
    kernel.proc_init();
    let src = AtomicU32::new(0);
    let dst = AtomicU32::new(u32::MAX);
    let src_addr = &src as *const AtomicU32 as usize;
    let dst_addr = &dst as *const AtomicU32 as usize;
    let encoded = (FUTEX_OP_ADD << 28) | (FUTEX_OP_CMP_EQ << 24) | (0xFFF << 12) | 0xFFF;

    let woken = kernel
        .dispatch_syscall(SYS_FUTEX, src_addr, 5, 0, 0, dst_addr, encoded)
        .expect("wake-op should accept signed 12-bit operands");

    assert_eq!(woken, 0);
    assert_eq!(dst.load(Ordering::SeqCst), u32::MAX.wrapping_sub(1));
}

#[test]
fn futex_wake_op_invalid_cmp_does_not_wake_waiters() {
    const FUTEX_OP_ADD: usize = 1;
    const FUTEX_OP_CMP_INVALID: usize = 6;

    let kernel = Arc::new(Kernel::new(N_FRAMES));
    kernel.proc_init();
    let src = Arc::new(AtomicU32::new(1));
    let dst = AtomicU32::new(0);
    let timeout = Arc::new([0usize, 100_000_000usize]);
    let src_addr = src.as_ref() as *const AtomicU32 as usize;
    let dst_addr = &dst as *const AtomicU32 as usize;
    let timeout_addr = timeout.as_ptr() as usize;

    let waiter_kernel = kernel.clone();
    let waiter_src = src.clone();
    let waiter = thread::spawn(move || {
        let src_addr = waiter_src.as_ref() as *const AtomicU32 as usize;
        waiter_kernel.dispatch_syscall(SYS_FUTEX, src_addr, 0, 1, timeout_addr, 0, 0)
    });

    thread::sleep(Duration::from_millis(25));
    let encoded = (FUTEX_OP_ADD << 28) | (FUTEX_OP_CMP_INVALID << 24) | (1 << 12);
    let err = kernel
        .dispatch_syscall(SYS_FUTEX, src_addr, 5, 1, 0, dst_addr, encoded)
        .expect_err("invalid wake-op comparison should fail");

    assert_eq!(err, "einval");
    assert_eq!(dst.load(Ordering::SeqCst), 1);
    assert_eq!(
        waiter.join().expect("waiter thread should finish"),
        Err("timeout")
    );
}

#[test]
fn futex_cmp_requeue_returns_eagain_when_source_value_changed() {
    let kernel = Kernel::new(N_FRAMES);
    kernel.proc_init();
    let src = AtomicU32::new(1);
    let dst = AtomicU32::new(0);
    let src_addr = &src as *const AtomicU32 as usize;
    let dst_addr = &dst as *const AtomicU32 as usize;

    let err = kernel
        .dispatch_syscall(SYS_FUTEX, src_addr, 9, 1, 1, dst_addr, 0)
        .expect_err("cmp-requeue should fail when the source word differs");

    assert_eq!(err, "eagain");
}

#[test]
fn futex_cmp_requeue_wakes_and_moves_after_compare() {
    let kernel = Arc::new(Kernel::new(N_FRAMES));
    kernel.proc_init();
    let src = Arc::new(AtomicU32::new(1));
    let dst = Arc::new(AtomicU32::new(0));
    let timeout = Arc::new([1usize, 0usize]);
    let src_addr = src.as_ref() as *const AtomicU32 as usize;
    let dst_addr = dst.as_ref() as *const AtomicU32 as usize;
    let timeout_addr = timeout.as_ptr() as usize;

    let first_kernel = kernel.clone();
    let first_src = src.clone();
    let first = thread::spawn(move || {
        let src_addr = first_src.as_ref() as *const AtomicU32 as usize;
        first_kernel
            .dispatch_syscall(SYS_FUTEX, src_addr, 0, 1, timeout_addr, 0, 0)
            .expect("first waiter should be woken")
    });

    let second_kernel = kernel.clone();
    let second_src = src.clone();
    let second = thread::spawn(move || {
        let src_addr = second_src.as_ref() as *const AtomicU32 as usize;
        second_kernel
            .dispatch_syscall(SYS_FUTEX, src_addr, 0, 1, timeout_addr, 0, 0)
            .expect("second waiter should be requeued then woken")
    });

    thread::sleep(Duration::from_millis(25));
    let affected = kernel
        .dispatch_syscall(SYS_FUTEX, src_addr, 9, 1, 1, dst_addr, 1)
        .expect("cmp-requeue should succeed when the source word matches");
    assert_eq!(affected, 2);

    let woken = kernel
        .dispatch_syscall(SYS_FUTEX, dst_addr, 1, 1, 0, 0, 0)
        .expect("wake on destination should find the cmp-requeued waiter");
    assert_eq!(woken, 1);
    assert_eq!(first.join().expect("first waiter should finish"), 0);
    assert_eq!(second.join().expect("second waiter should finish"), 0);
}
