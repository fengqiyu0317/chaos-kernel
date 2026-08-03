// AGENT: exec syscall regressions that exercise real Sv39 usercopy limits and
// prove every pre-commit error preserves the old process image.
use super::*;
use crate::trap::TrapFrame;

const OLD_MAPPING: usize = 0x5000_0000;
const PATH_PAGE: usize = 0x5100_0000;
const CROSS_PAGE: usize = 0x5200_0000;
const POINTER_PAGE: usize = 0x5300_0000;
const LARGE_ARG_BASE: usize = 0x5400_0000;
const LARGE_ENV_BASE: usize = 0x5500_0000;
const BAD_ELF_INPUT: usize = 0x5600_0000;
const OLD_BYTES: &[u8] = b"old-exec-image";

// AGENT: run copy-in errno and transactional rollback checks after the real
// QEMU frame pool/direct map have been installed.
pub fn run_all(pool: &FramePool) {
    exec_copyin_failures_preserve_old_image(pool);
    process_identity_syscalls_require_current(pool);
    getpid_and_gettid_distinguish_process_from_thread(pool);
    process_group_and_session_syscalls_enforce_transitions(pool);
}

// AGENT: reject every process identity/session query or mutation when CPU0 has
// no syscall caller, even if a registered process remains queryable by pid.
fn process_identity_syscalls_require_current(pool: &FramePool) {
    let kernel = Kernel::new(pool.clone());
    kernel.proc_init();
    kernel.set_cur(0, None);

    assert_eq!(sys_getpid(&kernel), Err("esrch"));
    assert_eq!(sys_gettid(&kernel), Err("esrch"));
    assert_eq!(sys_getppid(&kernel), Err("esrch"));
    assert_eq!(sys_getpgid(&kernel, 0), Err("esrch"));
    assert_eq!(sys_getpgid(&kernel, INIT_PID), Err("esrch"));
    assert_eq!(sys_getsid(&kernel, 0), Err("esrch"));
    assert_eq!(sys_getsid(&kernel, INIT_PID), Err("esrch"));
    assert_eq!(sys_setpgid(&kernel, 0, 0), Err("esrch"));
    assert_eq!(sys_setsid(&kernel), Err("esrch"));
}

// AGENT: prove getpid is process-wide while gettid follows the selected Task,
// including a second thread that shares init's Process and parent identity.
fn getpid_and_gettid_distinguish_process_from_thread(pool: &FramePool) {
    let kernel = Kernel::new(pool.clone());
    kernel.proc_init();
    let init = kernel.cur_task(0).expect("init should be current");

    assert_eq!(sys_getpid(&kernel), Ok(INIT_PID));
    assert_eq!(sys_gettid(&kernel), Ok(init.id()));
    assert_eq!(sys_getppid(&kernel), Ok(0));

    let sibling = kernel
        .tasks
        .clone_thread(&init, 0x7000_0000, 0x7100_0000)
        .expect("shared-process thread should clone");
    assert_ne!(sibling.id(), init.process.pid());
    kernel.set_cur(0, Some(sibling.clone()));

    assert_eq!(sys_getpid(&kernel), Ok(init.process.pid()));
    assert_eq!(sys_gettid(&kernel), Ok(sibling.id()));
    assert_eq!(sys_getppid(&kernel), Ok(0));
}

// AGENT: exercise the syscall-level parent, exec, process-group, and session
// rules around the authoritative JobControl state rather than only its helper.
fn process_group_and_session_syscalls_enforce_transitions(pool: &FramePool) {
    let kernel = Kernel::new(pool.clone());
    kernel.proc_init();
    let parent = kernel.cur_task(0).expect("init should be current");
    let parent_pid = parent.process.pid();
    let child = kernel
        .tasks
        .fork_process(&parent)
        .expect("job-control child should fork");
    let child_pid = child.process.pid();

    assert_eq!(sys_getpgid(&kernel, 0), Ok(parent_pid));
    assert_eq!(sys_getsid(&kernel, 0), Ok(parent_pid));
    assert_eq!(sys_getpgid(&kernel, child_pid), Ok(parent_pid));
    assert_eq!(sys_getsid(&kernel, child_pid), Ok(parent_pid));
    assert_eq!(sys_getpgid(&kernel, usize::MAX), Err("esrch"));
    assert_eq!(sys_getsid(&kernel, usize::MAX), Err("esrch"));

    assert_eq!(sys_setpgid(&kernel, 0, 0), Err("eperm"));
    assert_eq!(sys_setpgid(&kernel, child_pid, usize::MAX), Err("einval"));
    assert_eq!(sys_setpgid(&kernel, usize::MAX, 0), Err("esrch"));
    assert_eq!(sys_setpgid(&kernel, child_pid, child_pid + 1), Err("eperm"));

    assert_eq!(sys_setpgid(&kernel, child_pid, 0), Ok(0));
    assert_eq!(sys_getpgid(&kernel, child_pid), Ok(child_pid));
    assert_eq!(sys_getsid(&kernel, child_pid), Ok(parent_pid));
    assert_eq!(sys_setpgid(&kernel, child_pid, parent_pid), Ok(0));

    child.process.did_exec.store(true, Ordering::SeqCst);
    assert_eq!(sys_setpgid(&kernel, child_pid, child_pid), Err("eacces"));
    child.process.did_exec.store(false, Ordering::SeqCst);

    let unrelated = kernel
        .tasks
        .spawn()
        .expect("unrelated process should spawn in another session");
    assert_eq!(
        sys_setpgid(&kernel, unrelated.process.pid(), 0),
        Err("esrch")
    );

    kernel.set_cur(0, Some(child.clone()));
    assert_eq!(sys_getpid(&kernel), Ok(child_pid));
    assert_eq!(sys_gettid(&kernel), Ok(child.id()));
    assert_eq!(sys_getppid(&kernel), Ok(parent_pid));
    assert_eq!(sys_setsid(&kernel), Ok(child_pid));
    assert_eq!(sys_getpgid(&kernel, 0), Ok(child_pid));
    assert_eq!(sys_getsid(&kernel, 0), Ok(child_pid));
    assert_eq!(sys_setsid(&kernel), Err("eperm"));

    kernel.set_cur(0, Some(parent));
    assert_eq!(sys_getpgid(&kernel, child_pid), Ok(child_pid));
    assert_eq!(sys_getsid(&kernel, child_pid), Ok(child_pid));
    assert_eq!(sys_setpgid(&kernel, child_pid, parent_pid), Err("eperm"));
}

// AGENT: retain one complete old-image snapshot across pathname, pointer-array,
// aggregate-size, and malformed-ELF failures reached through sys_exec itself.
fn exec_copyin_failures_preserve_old_image(pool: &FramePool) {
    let kernel = Kernel::new(pool.clone());
    kernel.proc_init();
    kernel
        .install_directory("/bin")
        .expect("exec syscall fixture should install /bin");
    kernel
        .install_exec_file("/bin/bad-elf", b"not an ELF".to_vec())
        .expect("exec syscall fixture should install malformed ELF");
    let task = kernel.cur_task(0).expect("init should be current");

    map_user_region(&task, pool, OLD_MAPPING, PAGE_SZ);
    write_user_bytes(&task, pool, OLD_MAPPING, OLD_BYTES);
    let old_token = task
        .process
        .addr_space
        .lock()
        .unwrap()
        .vm_token()
        .expect("old exec syscall image should own an Sv39 root");
    let mut old_frame = TrapFrame::new();
    old_frame.regs[2] = 0x7000_0000;
    old_frame.regs[10] = 0x55;
    old_frame.sepc = 0x401000;
    task.install_user_trap_frame(old_frame.clone())
        .expect("old exec syscall frame should install");
    let cloexec_fd = task
        .add_file(FLike::Tty(TtyDevice))
        .expect("exec syscall rollback fd should allocate");
    task.set_cloexec(cloexec_fd, true)
        .expect("exec syscall rollback fd should become close-on-exec");
    assert!(task.process.sig_state.lock().unwrap().set_action(
        SIGUSR1,
        SigAction {
            handler: 0x402000,
            mask: 0x1234,
        },
    ));
    *task.process.exec_path.lock().unwrap() = "/bin/old-image".to_string();

    // AGENT: a completely unmapped pathname must fail before touching argv.
    assert_exec_error_preserves(
        &kernel,
        &task,
        pool,
        0x5700_0000,
        0,
        0,
        "efault",
        old_token,
        &old_frame,
        cloexec_fd,
    );

    // AGENT: 4096 readable non-NUL bytes hit the pathname-specific limit.
    map_user_region(&task, pool, PATH_PAGE, PAGE_SZ);
    write_user_bytes(&task, pool, PATH_PAGE, &vec![b'x'; PAGE_SZ]);
    assert_exec_error_preserves(
        &kernel,
        &task,
        pool,
        PATH_PAGE,
        0,
        0,
        "enametoolong",
        old_token,
        &old_frame,
        cloexec_fd,
    );

    // AGENT: pathname bytes remain UTF-8-only at the current VFS boundary.
    write_user_bytes(&task, pool, PATH_PAGE, &[0xff, 0]);
    assert_exec_error_preserves(
        &kernel, &task, pool, PATH_PAGE, 0, 0, "einval", old_token, &old_frame, cloexec_fd,
    );

    // AGENT: place argv four bytes before an unmapped page so native-width
    // pointer copy-in crosses the live Sv39 boundary and returns EFAULT.
    map_user_region(&task, pool, CROSS_PAGE, PAGE_SZ);
    write_user_bytes(&task, pool, CROSS_PAGE, b"/bin/bad-elf\0");
    assert_exec_error_preserves(
        &kernel,
        &task,
        pool,
        CROSS_PAGE,
        CROSS_PAGE + PAGE_SZ - mem::size_of::<usize>() / 2,
        0,
        "efault",
        old_token,
        &old_frame,
        cloexec_fd,
    );

    // AGENT: argv and envp share one 128-entry cap; the 129th non-NULL pointer
    // is rejected even though every entry aliases the same tiny string.
    map_user_region(&task, pool, POINTER_PAGE, PAGE_SZ);
    let alias_string = POINTER_PAGE + PAGE_SZ - 2;
    write_user_bytes(&task, pool, alias_string, b"x\0");
    let mut pointers = vec![0u8; 130 * mem::size_of::<usize>()];
    for index in 0..129 {
        let start = index * mem::size_of::<usize>();
        pointers[start..start + mem::size_of::<usize>()]
            .copy_from_slice(&alias_string.to_ne_bytes());
    }
    write_user_bytes(&task, pool, POINTER_PAGE, &pointers);
    assert_exec_error_preserves(
        &kernel,
        &task,
        pool,
        CROSS_PAGE,
        POINTER_PAGE,
        0,
        "e2big",
        old_token,
        &old_frame,
        cloexec_fd,
    );

    // AGENT: make argv consume 32 KiB including NUL, then place envp's NUL one
    // byte beyond the shared remaining budget so the second array returns E2BIG.
    map_user_region(&task, pool, LARGE_ARG_BASE, PAGE_SZ * 8);
    map_user_region(&task, pool, LARGE_ENV_BASE, PAGE_SZ * 9);
    let mut large_arg = vec![b'a'; PAGE_SZ * 8];
    *large_arg.last_mut().unwrap() = 0;
    let mut large_env = vec![b'e'; PAGE_SZ * 8 + 1];
    *large_env.last_mut().unwrap() = 0;
    write_user_bytes(&task, pool, LARGE_ARG_BASE, &large_arg);
    write_user_bytes(&task, pool, LARGE_ENV_BASE, &large_env);
    write_pointer_array(&task, pool, POINTER_PAGE, &[LARGE_ARG_BASE]);
    let envp = POINTER_PAGE + 2 * mem::size_of::<usize>();
    write_pointer_array(&task, pool, envp, &[LARGE_ENV_BASE]);
    assert_exec_error_preserves(
        &kernel,
        &task,
        pool,
        CROSS_PAGE,
        POINTER_PAGE,
        envp,
        "e2big",
        old_token,
        &old_frame,
        cloexec_fd,
    );

    // AGENT: valid user pointers reach the ELF parser, whose ENOEXEC still
    // leaves every old process resource and frame-pool count unchanged.
    map_user_region(&task, pool, BAD_ELF_INPUT, PAGE_SZ);
    write_user_bytes(&task, pool, BAD_ELF_INPUT, b"/bin/bad-elf\0bad-elf\0");
    write_pointer_array(&task, pool, BAD_ELF_INPUT + 64, &[BAD_ELF_INPUT + 13]);
    assert_exec_error_preserves(
        &kernel,
        &task,
        pool,
        BAD_ELF_INPUT,
        BAD_ELF_INPUT + 64,
        0,
        "enoexec",
        old_token,
        &old_frame,
        cloexec_fd,
    );

    task.close_fd(cloexec_fd)
        .expect("preserved exec syscall fd should close");
    task.process.addr_space.lock().unwrap().release_all_pages();
}

// AGENT: call sys_exec directly so an error cannot be confused with ordinary
// trap return-register mutation, then verify the complete process snapshot.
#[allow(clippy::too_many_arguments)]
fn assert_exec_error_preserves(
    kernel: &Kernel,
    task: &Task,
    pool: &FramePool,
    path: usize,
    argv: usize,
    envp: usize,
    expected: &'static str,
    old_token: usize,
    old_frame: &TrapFrame,
    cloexec_fd: usize,
) {
    let free_before = pool.free_count();
    assert_eq!(sys_exec(kernel, path, argv, envp).err(), Some(expected));
    assert_eq!(pool.free_count(), free_before);

    let mut old_bytes = [0u8; OLD_BYTES.len()];
    let addr_space = task.process.addr_space.lock().unwrap();
    assert_eq!(addr_space.vm_token(), Ok(old_token));
    addr_space
        .read_user_bytes(OLD_MAPPING, &mut old_bytes)
        .expect("failed syscall exec should preserve old mapping");
    drop(addr_space);
    assert_eq!(&old_bytes, OLD_BYTES);
    assert_eq!(task.snapshot_user_trap_frame().as_ref(), Ok(old_frame));
    assert!(task
        .get_fd_entry(cloexec_fd)
        .expect("failed syscall exec should preserve FD_CLOEXEC fd")
        .is_cloexec());
    let action = task
        .process
        .sig_state
        .lock()
        .unwrap()
        .get_action(SIGUSR1)
        .expect("failed syscall exec should preserve signal action")
        .clone();
    assert_eq!(action.handler, 0x402000);
    assert_eq!(action.mask, 0x1234);
    assert_eq!(
        task.process.exec_path.lock().unwrap().as_str(),
        "/bin/old-image"
    );
    assert!(!task.process.did_exec.load(Ordering::SeqCst));
}

// AGENT: map one page-aligned read/write userspace range for syscall copy-in.
fn map_user_region(task: &Task, pool: &FramePool, base: usize, len: usize) {
    task.process
        .addr_space
        .lock()
        .unwrap()
        .map_region(VmRegion::new(base, len, VM_READ | VM_WRITE), pool)
        .expect("exec syscall user region should map");
}

// AGENT: seed bytes through the same AddrSpace user-copy implementation used by
// sys_exec rather than accessing direct-map physical addresses in the test.
fn write_user_bytes(task: &Task, pool: &FramePool, addr: usize, bytes: &[u8]) {
    task.process
        .addr_space
        .lock()
        .unwrap()
        .write_user_bytes(addr, bytes, pool)
        .expect("exec syscall fixture bytes should be writable");
}

// AGENT: encode a terminated native-width userspace pointer array without
// depending on a host C ABI layout.
fn write_pointer_array(task: &Task, pool: &FramePool, addr: usize, values: &[usize]) {
    let mut bytes = vec![0u8; (values.len() + 1) * mem::size_of::<usize>()];
    for (index, value) in values.iter().enumerate() {
        let start = index * mem::size_of::<usize>();
        bytes[start..start + mem::size_of::<usize>()].copy_from_slice(&value.to_ne_bytes());
    }
    write_user_bytes(task, pool, addr, &bytes);
}
