// AGENT: boot-time process regressions that need the QEMU FramePool, Sv39
// page-table walker, and physical user-copy path to be initialized.
use super::*;

// AGENT: expose ProcInit stack construction checks to the optional QEMU boot
// selftest path.
pub fn run_all(pool: &FramePool) {
    capset_inherit_keeps_only_allowed_bits();
    capset_raise_ambient_requires_owned_inheritable_cap();
    capset_drop_cap_clears_ambient();
    spawn_root_creates_single_pid_one_init();
    spawn_root_rejects_nonempty_task_table();
    register_rejects_duplicate_pid_without_replacing_task();
    pgid_group_keeps_zombie_members_until_reap();
    reap_rejects_live_process();
    clone_thread_copies_caller_context_and_shares_process();
    reap_zombie_process_removes_thread_group_once();
    proc_init_push_at_writes_user_stack(pool);
    shm_segment_maps_shared_physical_page(pool);
}

// AGENT: capability inheritance keeps only the mask-approved bits and clamps
// dependent sets so they cannot contain capabilities the child no longer owns.
fn capset_inherit_keeps_only_allowed_bits() {
    let kept = 1u64 << CAP_KILL;
    let dropped = 1u64 << 63;
    let effective_without_base = 1u64 << CAP_SETUID;
    let ambient_without_base = 1u64 << CAP_NET_RAW;
    let parent = CapSet {
        bits: kept | dropped,
        effective: kept | dropped | effective_without_base,
        ambient: kept | dropped | ambient_without_base,
    };

    let child = CapSet::inherit(&parent);

    assert_eq!(child.bits, kept);
    assert_eq!(child.effective, kept);
    assert_eq!(child.ambient, kept);
}

// AGENT: raising ambient capabilities is limited to capabilities the process
// owns and the inheritance mask allows to cross a boundary.
fn capset_raise_ambient_requires_owned_inheritable_cap() {
    let owned_inheritable = 1u64 << CAP_KILL;
    let owned_not_inheritable = 1u64 << 63;
    let mut caps = CapSet {
        bits: owned_inheritable | owned_not_inheritable,
        effective: owned_inheritable | owned_not_inheritable,
        ambient: 0,
    };

    assert!(!caps.raise_ambient(CAP_SETUID));
    assert!(!caps.raise_ambient(63));
    assert!(!caps.raise_ambient(64));
    assert_eq!(caps.ambient, 0);

    assert!(caps.raise_ambient(CAP_KILL));
    assert_eq!(caps.ambient, owned_inheritable);
}

// AGENT: dropping a capability clears every dependent set, including ambient.
fn capset_drop_cap_clears_ambient() {
    let dropped = 1u64 << CAP_KILL;
    let kept = 1u64 << CAP_SETUID;
    let mut caps = CapSet {
        bits: dropped | kept,
        effective: dropped | kept,
        ambient: dropped | kept,
    };

    caps.drop_cap(CAP_KILL);

    assert_eq!(caps.bits, kept);
    assert_eq!(caps.effective, kept);
    assert_eq!(caps.ambient, kept);

    caps.drop_cap(64);

    assert_eq!(caps.bits, kept);
    assert_eq!(caps.effective, kept);
    assert_eq!(caps.ambient, kept);
}

// AGENT: init must be the first singleton task because pid 1 is special for
// signal protection and orphan reparenting.
fn spawn_root_creates_single_pid_one_init() {
    let table = TaskTable::new();
    let init = table.spawn_root().expect("first root spawn should succeed");

    assert_eq!(init.id(), Pid::INIT);
    assert_eq!(init.process_pid(), Pid::INIT);
    assert_eq!(table.root.lock().unwrap().as_ref().map(|t| t.id()), Some(1));
    assert_eq!(table.spawn_root().err(), Some("eexist"));
    assert_eq!(table.count(), 1);
}

// AGENT: spawn_root must not silently overwrite root after another standalone
// task has already consumed pid 1.
fn spawn_root_rejects_nonempty_task_table() {
    let table = TaskTable::new();
    let first = table.spawn("worker").expect("standalone spawn should work");

    assert_eq!(first.id(), Pid::INIT);
    assert_eq!(table.spawn_root().err(), Some("ebusy"));
    assert!(table.root.lock().unwrap().is_none());
    assert_eq!(table.count(), 1);
}

// AGENT: duplicate pid registration must fail before replacing the published
// task-table entry or corrupting process-group membership.
fn register_rejects_duplicate_pid_without_replacing_task() {
    let table = TaskTable::new();
    let first = table.spawn("worker").expect("standalone spawn should work");
    let duplicate = Task::make(first.id(), "duplicate");

    assert_eq!(table.register(&duplicate, Pid(first.id())), Err("eexist"));
    assert!(Arc::ptr_eq(&table.find(first.id()).unwrap(), &first));

    let group = table.pgid_group(first.id() as Pgid);
    assert_eq!(group.len(), 1);
    assert!(Arc::ptr_eq(&group[0], &first));
}

// AGENT: process-group lookup reports membership, including zombies that remain
// present until wait/reap removes them from the table.
fn pgid_group_keeps_zombie_members_until_reap() {
    let table = TaskTable::new();
    let task = table.spawn("worker").expect("standalone spawn should work");
    let pgid = *task.process.pgid.lock().unwrap();

    assert!(task.exit_proc(ExitReason::Code(0)));

    let group = table.pgid_group(pgid);
    assert_eq!(group.len(), 1);
    assert!(Arc::ptr_eq(&group[0], &task));
}

// AGENT: reap is a zombie-only operation; a mistaken live-process id must not
// delete the task table entry.
fn reap_rejects_live_process() {
    let table = TaskTable::new();
    let task = table.spawn("worker").expect("standalone spawn should work");

    assert_eq!(table.reap(task.id()), Err("ebusy"));
    assert!(table.find(task.id()).is_some());
    assert_eq!(table.count(), 1);
}

// AGENT: multi-threaded zombies are collected at process granularity, while all
// same-process task-table entries disappear in the single reap step.
fn reap_zombie_process_removes_thread_group_once() {
    let table = TaskTable::new();
    let parent = table.spawn_root().expect("root spawn should work");
    let child = table.spawn("child").expect("child spawn should work");
    child.link_parent(&parent);
    parent.link_child(&child);
    let thread = table
        .clone_thread(&child, 0x8000_0000, 0, 0)
        .expect("thread clone should succeed");

    assert!(child.exit_proc(ExitReason::Code(7)));
    assert_eq!(table.zombie_tasks(), vec![child.id()]);
    assert_eq!(table.reap(thread.id()), Ok(()));

    assert!(table.find(child.id()).is_none());
    assert!(table.find(thread.id()).is_none());
    assert!(parent.process.subtasks.lock().unwrap().is_empty());
}

// AGENT: clone_thread starts from the caller thread context, then applies the
// clone-specific return value, user stack, TLS, clear-child-tid, and signal mask.
fn clone_thread_copies_caller_context_and_shares_process() {
    let table = TaskTable::new();
    let task = table.spawn("worker").expect("standalone spawn should work");
    let stack_top = 0x8000_0000;
    let tls = 0xabc;
    let clear_tid = 0xdead;
    let sig_mask = 0x24;

    {
        let mut thd = task.thd_ctx.lock().unwrap();
        let ctx = thd.as_mut().expect("source context should exist");
        ctx.uctx.set_ip(0x401000);
        ctx.uctx.r[0] = 99;
        ctx.uctx.r[3] = 0x7777;
        ctx.uctx.set_sp(0x9000_0000);
        ctx.clear_tid = 0x1111;
        ctx.smask = 0x11;
    }
    *task.sig_mask.lock().unwrap() = sig_mask;

    let thread = table
        .clone_thread(&task, stack_top, tls, clear_tid)
        .expect("thread clone should succeed");

    assert!(Arc::ptr_eq(&task.process, &thread.process));
    assert!(task.process.threads.lock().unwrap().contains(&thread.id()));
    assert_eq!(*thread.sig_mask.lock().unwrap(), sig_mask);
    let thd = thread.thd_ctx.lock().unwrap();
    let ctx = thd.as_ref().expect("cloned context should exist");
    assert_eq!(ctx.uctx.ip, 0x401000);
    assert_eq!(ctx.uctx.r[0], 0);
    assert_eq!(ctx.uctx.r[3], 0x7777);
    assert_eq!(ctx.uctx.r[N_REGS - 1], stack_top);
    assert_eq!(ctx.uctx.r[N_REGS - 2], tls);
    assert_eq!(ctx.clear_tid, clear_tid);
    assert_eq!(ctx.smask, sig_mask);
}

// AGENT: construct a minimal init stack through real AddrSpace mappings and
// read it back through the same user-copy path syscalls will use.
fn proc_init_push_at_writes_user_stack(pool: &FramePool) {
    let top = USR_STK_OFF + USR_STK_SZ;
    let entry = 0x0040_1000;
    let mut addr_space = AddrSpace::new();
    addr_space
        .map_region(
            VmRegion::new(USR_STK_OFF, USR_STK_SZ, VM_READ | VM_WRITE | VM_GROWSDOWN),
            pool,
        )
        .expect("user stack should map");

    let init = ProcInit {
        args: vec!["init".to_string()],
        envs: vec!["A=B".to_string()],
        auxv: BTreeMap::from([(AT_PAGESZ, PAGE_SZ), (AT_ENTRY, entry)]),
    };

    assert_eq!(init.push_at(&mut addr_space, pool, top - 8), Err("einval"));

    let sp = init
        .push_at(&mut addr_space, pool, top)
        .expect("ProcInit should write the initial user stack");
    assert_eq!(sp & 0xF, 0);
    assert!(sp >= USR_STK_OFF);
    assert!(sp < top);

    let word = mem::size_of::<usize>();
    assert_eq!(addr_space.read_user_usize(sp).unwrap(), 1);
    let argv0 = addr_space.read_user_usize(sp + word).unwrap();
    assert_eq!(addr_space.read_user_usize(sp + word * 2).unwrap(), 0);
    let env0 = addr_space.read_user_usize(sp + word * 3).unwrap();
    assert_eq!(addr_space.read_user_usize(sp + word * 4).unwrap(), 0);

    assert_user_cstr(&addr_space, argv0, "init");
    assert_user_cstr(&addr_space, env0, "A=B");

    let auxv = sp + word * 5;
    assert_eq!(
        addr_space.read_user_usize(auxv).unwrap(),
        AT_PAGESZ as usize
    );
    assert_eq!(addr_space.read_user_usize(auxv + word).unwrap(), PAGE_SZ);
    assert_eq!(
        addr_space.read_user_usize(auxv + word * 2).unwrap(),
        AT_ENTRY as usize
    );
    assert_eq!(addr_space.read_user_usize(auxv + word * 3).unwrap(), entry);
    assert_eq!(addr_space.read_user_usize(auxv + word * 4).unwrap(), 0);
    assert_eq!(addr_space.read_user_usize(auxv + word * 5).unwrap(), 0);
}

// AGENT: shared-memory segments should map the same physical page into
// independent address spaces instead of allocating anonymous COW pages.
fn shm_segment_maps_shared_physical_page(pool: &FramePool) {
    let segment = ShmSegment::new(1, pool).expect("shm segment should allocate");
    let mut left = AddrSpace::new();
    let mut right = AddrSpace::new();
    let left_addr = 0x2000_0000;
    let right_addr = 0x3000_0000;
    let flags = VM_READ | VM_WRITE;

    left.map_shared_pages(
        VmRegion::new(left_addr, PAGE_SZ, flags),
        segment.pages(),
        pool,
    )
    .expect("left shared mapping should succeed");
    right
        .map_shared_pages(
            VmRegion::new(right_addr, PAGE_SZ, flags),
            segment.pages(),
            pool,
        )
        .expect("right shared mapping should succeed");

    left.write_user_bytes(left_addr + 17, b"shared", pool)
        .expect("shared mapping should be writable");

    let mut bytes = [0u8; 6];
    right
        .read_user_bytes(right_addr + 17, &mut bytes)
        .expect("shared mapping should be readable");
    assert_eq!(&bytes, b"shared");
}

// AGENT: read a known-length C string from user memory and verify its trailing
// NUL byte without relying on host-side string helpers.
fn assert_user_cstr(addr_space: &AddrSpace, addr: usize, expected: &str) {
    let mut bytes = vec![0u8; expected.len() + 1];
    addr_space
        .read_user_bytes(addr, &mut bytes)
        .expect("user string should be readable");
    assert_eq!(&bytes[..expected.len()], expected.as_bytes());
    assert_eq!(bytes[expected.len()], 0);
}
