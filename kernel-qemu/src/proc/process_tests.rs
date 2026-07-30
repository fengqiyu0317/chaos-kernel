// AGENT: boot-time process regressions that need the QEMU FramePool, Sv39
// page-table walker, and physical user-copy path to be initialized.
use super::*;
use crate::trap::TrapFrame;

// AGENT: expose process, initial-stack, and shared ELF image regressions to the
// optional QEMU boot selftest path.
pub fn run_all(pool: &FramePool) {
    capset_inherit_keeps_only_allowed_bits();
    capset_raise_ambient_requires_owned_inheritable_cap();
    capset_drop_cap_clears_ambient();
    kernel_stack_uses_and_releases_frame_pool_run(pool);
    task_spawn_reports_kernel_stack_exhaustion();
    spawn_root_failure_does_not_consume_init_pid();
    task_slot_limit_reopens_only_after_reap(pool);
    spawn_root_creates_single_pid_one_init(pool);
    spawn_root_rejects_nonempty_task_table(pool);
    init_process_resolves_through_process_index(pool);
    process_index_keeps_task_and_process_identity_separate(pool);
    pgid_group_keeps_zombie_members_until_reap(pool);
    job_control_move_process_validates_group_transitions();
    job_control_start_new_session_validates_leader_rules();
    job_control_stays_authoritative_across_process_transitions(pool);
    reap_rejects_live_process(pool);
    reap_rejects_zombie_with_unreparented_children(pool);
    riscv_clone_accepts_only_the_fork_equivalent_subset(pool);
    fork_copies_complete_user_frame(pool);
    fork_splits_process_and_caller_task_inheritance(pool);
    fork_from_nonleader_attaches_process_parent(pool);
    reparent_children_uses_init_process(pool);
    reparented_zombie_notifies_init(pool);
    clone_thread_copies_caller_context_and_shares_process(pool);
    reap_zombie_process_removes_thread_group_once(pool);
    exiting_phase_blocks_clone_wait_and_reap(pool);
    nonleader_exit_keeps_leader_resources_and_parent_quiet(pool);
    leader_exit_keeps_remaining_thread_and_process(pool);
    exit_group_terminates_every_thread(pool);
    fatal_signal_terminates_every_thread(pool);
    riscv_exit_numbers_map_to_distinct_internal_calls();
    proc_init_push_at_writes_user_stack(pool);
    parse_elf_rejects_unsupported_or_invalid_entry();
    parse_elf_validates_program_header_layouts();
    elf_load_regions_coalesce_contiguous_permissions();
    prepared_user_image_normalizes_invalid_elf_to_enoexec(pool);
    failed_exec_preserves_old_process_image(pool);
    prepared_user_image_loads_elf_segment_and_stack(pool);
    prepared_user_image_preserves_no_access_segment(pool);
    prepared_user_image_loads_segments_sharing_a_page(pool);
    prepared_user_image_loads_out_of_order_segments(pool);
    resident_and_sv39_stay_consistent_across_transitions(pool);
    writable_non_cow_page_is_not_recovered_as_cow(pool);
    forked_writable_page_resolves_cow(pool);
    shm_segment_maps_shared_physical_page(pool);
    release_all_pages_drops_same_space_aliases(pool);
}

// AGENT: keep process-test file fixtures local instead of retaining a synthetic
// pathname compatibility constructor on FInstance.
fn standalone_regular_file() -> FInstance {
    let fs = FsInstance::new(0, FileStorage::standalone());
    let node = fs
        .install_regular_at(
            &fs.root(),
            ChildName::new("file").expect("test child name should be valid"),
            &[],
            false,
        )
        .expect("process file fixture should install");
    let mount = MountTable::new(fs).root();
    FInstance::new(mount, node)
}

// AGENT: preserve detailed parser diagnostics for focused parser tests while
// exposing malformed executable images to callers as Linux ENOEXEC.
fn prepared_user_image_normalizes_invalid_elf_to_enoexec(pool: &FramePool) {
    assert_eq!(
        prepare_user_image(b"not an ELF", Vec::new(), Vec::new(), pool).err(),
        Some("enoexec")
    );
}

// AGENT: lock the prepare-before-commit boundary: a malformed replacement ELF
// must not alter the old address space, trap frame, signal disposition, process
// identity, or descriptor-local FD_CLOEXEC state.
fn failed_exec_preserves_old_process_image(pool: &FramePool) {
    const OLD_MAPPING: usize = 0x6000_0000;
    const OLD_BYTES: &[u8] = b"old-image";

    let kernel = Kernel::new(pool.clone());
    kernel.proc_init();
    kernel
        .install_directory("/bin")
        .expect("exec rollback fixture should install /bin");
    kernel
        .install_exec_file("/bin/bad-elf", b"not an ELF".to_vec())
        .expect("exec rollback fixture should install malformed executable");
    let task = kernel.cur_task(0).expect("init should be current");
    let old_token = {
        let mut addr_space = task.process.addr_space.lock().unwrap();
        addr_space
            .map_region(
                VmRegion::new(OLD_MAPPING, PAGE_SZ, VM_READ | VM_WRITE),
                pool,
            )
            .expect("old image mapping should succeed");
        addr_space
            .write_user_bytes(OLD_MAPPING, OLD_BYTES, pool)
            .expect("old image bytes should be writable");
        addr_space
            .vm_token()
            .expect("old image should own an Sv39 root")
    };

    let mut old_frame = TrapFrame::new();
    old_frame.regs[2] = 0x7000_0000;
    old_frame.regs[10] = 0x55;
    old_frame.sepc = 0x401000;
    task.install_user_trap_frame(old_frame.clone())
        .expect("old trap frame should install");
    let cloexec_fd = task
        .add_file(FLike::Tty(TtyDevice))
        .expect("rollback fixture should allocate an fd");
    task.set_cloexec(cloexec_fd, true)
        .expect("rollback fixture fd should become close-on-exec");
    assert!(task.process.sig_state.lock().unwrap().set_action(
        SIGUSR1,
        SigAction {
            handler: 0x402000,
            mask: 0x1234,
        },
    ));
    *task.process.exec_path.lock().unwrap() = "/bin/old-image".to_string();

    assert_eq!(
        kernel.do_exec(
            task.id(),
            "/bin/bad-elf",
            vec!["bad-elf".to_string()],
            Vec::new(),
        ),
        Err("enoexec")
    );

    let mut old_bytes = [0u8; OLD_BYTES.len()];
    {
        let addr_space = task.process.addr_space.lock().unwrap();
        assert_eq!(addr_space.vm_token(), Ok(old_token));
        addr_space
            .read_user_bytes(OLD_MAPPING, &mut old_bytes)
            .expect("failed exec should preserve old user bytes");
    }
    assert_eq!(&old_bytes, OLD_BYTES);
    assert_eq!(task.snapshot_user_trap_frame(), Ok(old_frame));
    let preserved_fd = task
        .get_fd_entry(cloexec_fd)
        .expect("failed exec should preserve close-on-exec fd");
    assert!(preserved_fd.is_cloexec());
    let sig_state = task.process.sig_state.lock().unwrap();
    let action = sig_state
        .get_action(SIGUSR1)
        .expect("failed exec should preserve signal disposition");
    assert_eq!(action.handler, 0x402000);
    assert_eq!(action.mask, 0x1234);
    drop(sig_state);
    assert_eq!(
        task.process.exec_path.lock().unwrap().as_str(),
        "/bin/old-image"
    );
    assert!(!task.process.did_exec.load(Ordering::SeqCst));

    task.close_fd(cloexec_fd)
        .expect("rollback fixture fd should close");
    task.process.addr_space.lock().unwrap().release_all_pages();
}

// AGENT: prove KStk owns its zeroed direct-map pages through ordinary PgFrame
// handles and returns them after any PgFrame metadata allocations also drop.
fn kernel_stack_uses_and_releases_frame_pool_run(pool: &FramePool) {
    let pages = KSTK_SZ / PAGE_SZ;
    let free_before = pool.free_count();
    let stack = KStk::new(pool).expect("kernel stack frame run should allocate");
    let stack_base = stack.top() - KSTK_SZ;

    assert!(pool.free_count() <= free_before - pages);
    assert_eq!(stack_base % PAGE_SZ, 0);
    let bytes = unsafe { core::slice::from_raw_parts(stack_base as *const u8, KSTK_SZ) };
    assert!(bytes.iter().all(|byte| *byte == 0));

    drop(stack);
    assert_eq!(pool.free_count(), free_before);
}

// AGENT: propagate a missing four-page stack run as enomem without registering
// a partially constructed task or touching the synthetic pool's addresses.
fn task_spawn_reports_kernel_stack_exhaustion() {
    let pool = FramePool::new(KSTK_SZ / PAGE_SZ - 1, MEM_OFF);
    let table = TaskTable::new(pool);

    for _ in 0..=N_PROC {
        assert_eq!(table.spawn().err(), Some("enomem"));
    }
    assert_eq!(table.task_count(), 0);
}

// AGENT: keep pid 1, init identity, and task-table capacity uncommitted when
// fallible root-task construction cannot allocate its kernel stack.
fn spawn_root_failure_does_not_consume_init_pid() {
    let pool = FramePool::new(KSTK_SZ / PAGE_SZ - 1, MEM_OFF);
    let table = TaskTable::new(pool);

    assert_eq!(table.spawn_root().err(), Some("enomem"));
    assert_eq!(table.seq.load(Ordering::SeqCst), INIT_PID);
    assert_eq!(table.task_count(), 0);
    assert_eq!(table.process_count(), 0);
    assert!(table.init_process().is_none());
}

// AGENT: keep committed slots occupied until their task-table entries are
// actually reaped, while failed over-limit creation leaves no partial task.
fn task_slot_limit_reopens_only_after_reap(pool: &FramePool) {
    let table = TaskTable::new(pool.clone());
    let mut tasks = Vec::new();
    for _ in 0..N_PROC {
        tasks.push(table.spawn().expect("task table should fill to its limit"));
    }

    assert_eq!(table.task_count(), N_PROC);
    assert_eq!(table.spawn().err(), Some("eagain"));

    let reaped = tasks.pop().expect("full task table should have a victim");
    let reaped_pid = reaped.process.pid();
    assert!(reaped
        .process
        .begin_group_exit(ExitReason::Code(0))
        .is_some());
    reaped.process.finish_process_exit();
    assert_eq!(table.reap(reaped_pid), Ok(()));
    assert_eq!(table.task_count(), N_PROC - 1);

    let replacement = table
        .spawn()
        .expect("reaping one task should reopen exactly one slot");
    assert!(replacement.id() > reaped.id());
    assert_eq!(table.task_count(), N_PROC);
    assert_eq!(table.spawn().err(), Some("eagain"));
}

// AGENT: exercise map, protection, unmap, and release transitions while
// checking both directions of the resident-page/Sv39 invariant after each one.
fn resident_and_sv39_stay_consistent_across_transitions(pool: &FramePool) {
    let base = 0x1600_0000;
    let mut addr_space = AddrSpace::new();

    addr_space
        .map_region(VmRegion::new(base, PAGE_SZ * 3, VM_READ | VM_WRITE), pool)
        .expect("three-page consistency mapping should succeed");
    addr_space
        .check_page_table_consistency()
        .expect("new resident pages should match Sv39");

    addr_space
        .protect(base + PAGE_SZ, PAGE_SZ, VM_READ)
        .expect("middle-page protection should succeed");
    addr_space
        .check_page_table_consistency()
        .expect("protected resident page should match Sv39");
    assert!(addr_space
        .write_user_bytes(base + PAGE_SZ, b"x", pool)
        .is_err());

    assert_eq!(
        addr_space
            .unmap_range(base + PAGE_SZ, PAGE_SZ, pool)
            .expect("middle-page unmap should succeed"),
        1
    );
    addr_space
        .check_page_table_consistency()
        .expect("remaining resident pages should match Sv39");
    assert!(addr_space
        .read_user_bytes(base + PAGE_SZ, &mut [0u8; 1])
        .is_err());

    addr_space.release_all_pages();
    addr_space
        .check_page_table_consistency()
        .expect("released address space should have no orphan mappings");
}

// AGENT: reject a store-fault recovery request for an ordinary writable page so
// the trap path cannot retry a persistent non-COW fault without changing state.
fn writable_non_cow_page_is_not_recovered_as_cow(pool: &FramePool) {
    let addr = 0x1700_0000;
    let mut addr_space = AddrSpace::new();
    addr_space
        .map_region(VmRegion::new(addr, PAGE_SZ, VM_READ | VM_WRITE), pool)
        .expect("ordinary writable page should map");
    addr_space
        .check_page_table_consistency()
        .expect("ordinary leaf should include stable A/D state");

    assert_eq!(
        addr_space.handle_cow_fault(addr, pool),
        Err("segfault"),
        "a non-COW writable mapping must not be reported as a recovered fault"
    );
    addr_space
        .write_user_bytes(addr, b"ok", pool)
        .expect("ordinary kernel usercopy should remain writable");
    addr_space
        .check_page_table_consistency()
        .expect("rejected non-COW recovery should leave the mapping unchanged");
    addr_space.release_all_pages();
    addr_space
        .check_page_table_consistency()
        .expect("released non-COW address space should be empty");
}

// AGENT: prove parent and child carry independent mapping-local COW state by
// resolving each side in turn while their page contents remain separated.
fn forked_writable_page_resolves_cow(pool: &FramePool) {
    let addr = 0x1800_0000;
    let mut parent = AddrSpace::new();
    parent
        .map_region(VmRegion::new(addr, PAGE_SZ, VM_READ | VM_WRITE), pool)
        .expect("parent page should map");
    parent
        .write_user_bytes(addr, b"parent", pool)
        .expect("parent page should be writable before fork");
    parent
        .check_page_table_consistency()
        .expect("parent mapping should start consistent");

    let mut child = AddrSpace::fork_from(&mut parent, pool).expect("address space should fork");
    parent
        .check_page_table_consistency()
        .expect("parent COW state should match Sv39");
    child
        .check_page_table_consistency()
        .expect("child COW state should match Sv39");
    child
        .write_user_bytes(addr, b"child!", pool)
        .expect("child write should resolve COW");
    child
        .check_page_table_consistency()
        .expect("resolved child COW state should match Sv39");

    let mut parent_bytes = [0u8; 6];
    let mut child_bytes = [0u8; 6];
    parent
        .read_user_bytes(addr, &mut parent_bytes)
        .expect("parent page should remain readable");
    child
        .read_user_bytes(addr, &mut child_bytes)
        .expect("child page should remain readable");
    assert_eq!(&parent_bytes, b"parent");
    assert_eq!(&child_bytes, b"child!");

    parent
        .write_user_bytes(addr, b"newpar", pool)
        .expect("parent should resolve its own remaining COW state");
    parent
        .check_page_table_consistency()
        .expect("resolved parent COW state should match Sv39");
    parent
        .read_user_bytes(addr, &mut parent_bytes)
        .expect("resolved parent page should remain readable");
    child
        .read_user_bytes(addr, &mut child_bytes)
        .expect("child page should remain isolated");
    assert_eq!(&parent_bytes, b"newpar");
    assert_eq!(&child_bytes, b"child!");

    parent.release_all_pages();
    child.release_all_pages();
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
fn spawn_root_creates_single_pid_one_init(pool: &FramePool) {
    let table = TaskTable::new(pool.clone());
    let init = table.spawn_root().expect("first root spawn should succeed");

    assert_eq!(init.id(), INIT_PID);
    assert_eq!(init.process.pid(), INIT_PID);
    assert_eq!(
        table.init_process().as_ref().map(|process| process.pid()),
        Some(INIT_PID)
    );
    assert_eq!(table.spawn_root().err(), Some("eexist"));
    assert_eq!(table.task_count(), 1);
    assert_eq!(table.process_count(), 1);
}

// AGENT: spawn_root must not silently overwrite root after another standalone
// task has already consumed pid 1.
fn spawn_root_rejects_nonempty_task_table(pool: &FramePool) {
    let table = TaskTable::new(pool.clone());
    let first = table.spawn().expect("standalone spawn should work");

    assert_eq!(first.id(), INIT_PID);
    assert_eq!(table.spawn_root().err(), Some("ebusy"));
    assert!(table.init_process().is_none());
    assert_eq!(table.task_count(), 1);
    assert_eq!(table.process_count(), 1);
}

// AGENT: the init role marker must not retain a Process after the authoritative
// process index reaps it, and later lookups must observe that removal.
fn init_process_resolves_through_process_index(pool: &FramePool) {
    let table = TaskTable::new(pool.clone());
    let init = table.spawn_root().expect("root spawn should work");
    let init_process = Arc::downgrade(&init.process);

    assert!(init.process.begin_group_exit(ExitReason::Code(0)).is_some());
    init.process.finish_process_exit();
    assert_eq!(table.reap(INIT_PID), Ok(()));
    assert!(table.init_process().is_none());

    drop(init);
    assert!(init_process.upgrade().is_none());
}

// AGENT: prove tid and pid indexes resolve distinct entity types while sharing
// the exact Process allocation owned by the registered Task.
fn process_index_keeps_task_and_process_identity_separate(pool: &FramePool) {
    let table = TaskTable::new(pool.clone());
    let first = table.spawn().expect("standalone spawn should work");
    assert!(Arc::ptr_eq(&table.find_task(first.id()).unwrap(), &first));
    assert!(Arc::ptr_eq(
        &table.find_process(first.process.pid()).unwrap(),
        &first.process
    ));
    assert!(Arc::ptr_eq(
        &table.process_of_tid(first.id()).unwrap(),
        &first.process
    ));

    let group = table.pgid_group(first.id() as i32);
    assert_eq!(group.len(), 1);
    assert!(Arc::ptr_eq(&group[0], &first.process));
}

// AGENT: process-group lookup reports membership, including zombies that remain
// present until wait/reap removes them from the table.
fn pgid_group_keeps_zombie_members_until_reap(pool: &FramePool) {
    let table = TaskTable::new(pool.clone());
    let task = table.spawn().expect("standalone spawn should work");
    let pgid = table
        .process_pgid(task.process.pid())
        .expect("spawned task should have job-control membership");

    assert!(task.process.begin_group_exit(ExitReason::Code(0)).is_some());
    task.process.finish_process_exit();

    let group = table.pgid_group(pgid);
    assert_eq!(group.len(), 1);
    assert!(Arc::ptr_eq(&group[0], &task.process));
}

// AGENT: exercise move_process's no-op, group-creation, same-session join,
// cross-session rejection, and empty-source cleanup branches directly on the
// authoritative bidirectional job-control indexes.
fn job_control_move_process_validates_group_transitions() {
    let mut job_control = JobControl::default();
    assert_eq!(job_control.move_process(99, 99), Err("esrch"));

    job_control
        .add_process(10, 10, 10)
        .expect("session leader should create its initial group");
    job_control
        .add_process(11, 10, 10)
        .expect("first child should inherit the session group");
    job_control
        .add_process(12, 10, 10)
        .expect("second child should inherit the session group");
    job_control
        .add_process(20, 20, 20)
        .expect("other session should create its own group");

    assert_eq!(job_control.move_process(11, 10), Ok(()));
    assert_eq!(job_control.membership(11), Some((10, 10)));
    assert_eq!(job_control.members(10), vec![10, 11, 12]);

    assert_eq!(job_control.move_process(11, 13), Err("eperm"));
    assert_eq!(job_control.membership(11), Some((10, 10)));
    assert_eq!(job_control.members(10), vec![10, 11, 12]);
    assert!(job_control.members(13).is_empty());

    assert_eq!(job_control.move_process(11, 11), Ok(()));
    assert_eq!(job_control.membership(11), Some((11, 10)));
    assert_eq!(job_control.members(10), vec![10, 12]);
    assert_eq!(job_control.members(11), vec![11]);

    assert_eq!(job_control.move_process(12, 11), Ok(()));
    assert_eq!(job_control.membership(12), Some((11, 10)));
    assert_eq!(job_control.members(10), vec![10]);
    assert_eq!(job_control.members(11), vec![11, 12]);

    assert_eq!(job_control.move_process(12, 20), Err("eperm"));
    assert_eq!(job_control.membership(12), Some((11, 10)));
    assert_eq!(job_control.members(11), vec![11, 12]);
    assert_eq!(job_control.members(20), vec![20]);

    assert_eq!(job_control.move_process(11, 10), Ok(()));
    assert_eq!(job_control.members(11), vec![12]);
    assert_eq!(job_control.move_process(12, 10), Ok(()));
    assert_eq!(job_control.membership(12), Some((10, 10)));
    assert!(job_control.members(11).is_empty());
    assert_eq!(job_control.members(10), vec![10, 11, 12]);
}

// AGENT: prove setsid-style transitions reject current leaders and surviving
// PGID collisions, preserve state on failure, and delete an emptied old group
// before publishing the caller as the sole member of its new session.
fn job_control_start_new_session_validates_leader_rules() {
    let mut job_control = JobControl::default();
    assert_eq!(job_control.start_new_session(99), Err("esrch"));

    job_control
        .add_process(1, 1, 1)
        .expect("session leader should create its initial group");
    job_control
        .add_process(2, 1, 1)
        .expect("first child should inherit the session group");
    job_control
        .add_process(3, 1, 1)
        .expect("second child should inherit the session group");

    assert_eq!(job_control.start_new_session(1), Err("eperm"));
    assert_eq!(job_control.membership(1), Some((1, 1)));
    assert_eq!(job_control.members(1), vec![1, 2, 3]);

    job_control
        .move_process(2, 2)
        .expect("child should create a group in the inherited session");
    job_control
        .move_process(3, 2)
        .expect("sibling should join the existing same-session group");
    job_control
        .move_process(2, 1)
        .expect("group leader may leave while another member keeps the group alive");
    assert_eq!(job_control.membership(2), Some((1, 1)));
    assert_eq!(job_control.members(2), vec![3]);

    assert_eq!(job_control.start_new_session(2), Err("eperm"));
    assert_eq!(job_control.membership(2), Some((1, 1)));
    assert_eq!(job_control.members(1), vec![1, 2]);
    assert_eq!(job_control.members(2), vec![3]);

    assert_eq!(job_control.start_new_session(3), Ok(()));
    assert!(job_control.members(2).is_empty());
    assert_eq!(job_control.membership(3), Some((3, 3)));
    assert_eq!(job_control.members(3), vec![3]);

    assert_eq!(job_control.start_new_session(2), Ok(()));
    assert_eq!(job_control.membership(2), Some((2, 2)));
    assert_eq!(job_control.members(1), vec![1]);
    assert_eq!(job_control.members(2), vec![2]);
    assert_eq!(job_control.start_new_session(2), Err("eperm"));
}

// AGENT: prove fork, setpgid-style moves, setsid, and reap update the single
// job-control registry without relying on mirrored Process fields.
fn job_control_stays_authoritative_across_process_transitions(pool: &FramePool) {
    let table = TaskTable::new(pool.clone());
    let parent = table.spawn_root().expect("root spawn should work");
    let parent_pid = parent.process.pid();

    let moved_child = table
        .fork_process(&parent)
        .expect("first child fork should work");
    let moved_pid = moved_child.process.pid();
    assert_eq!(table.process_pgid(moved_pid), Some(parent_pid as i32));
    assert_eq!(table.process_sid(moved_pid), Some(parent_pid));

    table
        .move_process_to_group(&moved_child.process, moved_pid as i32)
        .expect("child should create a group in its inherited session");
    assert_eq!(table.process_pgid(moved_pid), Some(moved_pid as i32));
    assert_eq!(table.process_sid(moved_pid), Some(parent_pid));

    let session_child = table
        .fork_process(&parent)
        .expect("second child fork should work");
    let session_pid = session_child.process.pid();
    assert_eq!(
        table.start_new_session(&session_child.process),
        Ok(session_pid)
    );
    assert_eq!(table.process_pgid(session_pid), Some(session_pid as i32));
    assert_eq!(table.process_sid(session_pid), Some(session_pid));

    assert!(moved_child
        .process
        .begin_group_exit(ExitReason::Code(0))
        .is_some());
    moved_child.process.finish_process_exit();
    assert_eq!(table.reap(moved_pid), Ok(()));
    assert_eq!(table.process_pgid(moved_pid), None);
    assert!(table.pgid_group(moved_pid as i32).is_empty());
}

// AGENT: reap is a zombie-only operation; a mistaken live-process id must not
// delete the task table entry.
fn reap_rejects_live_process(pool: &FramePool) {
    let table = TaskTable::new(pool.clone());
    let task = table.spawn().expect("standalone spawn should work");

    assert_eq!(table.reap(task.process.pid()), Err("ebusy"));
    assert!(table.find_task(task.id()).is_some());
    assert!(table.find_process(task.process.pid()).is_some());
    assert_eq!(table.task_count(), 1);
}

// AGENT: reap must not hide a skipped exit-time reparent transition by silently
// detaching a zombie's remaining children from the process family.
fn reap_rejects_zombie_with_unreparented_children(pool: &FramePool) {
    let table = TaskTable::new(pool.clone());
    let init = table.spawn_root().expect("root spawn should work");
    let parent = table
        .fork_process(&init)
        .expect("parent process should fork");
    let child = table
        .fork_process(&parent)
        .expect("child process should fork");
    let parent_pid = parent.process.pid();

    assert!(parent
        .process
        .begin_group_exit(ExitReason::Code(3))
        .is_some());
    parent.process.finish_process_exit();

    assert_eq!(table.reap(parent_pid), Err("ebusy"));
    assert!(table.find_process(parent_pid).is_some());
    assert!(parent
        .process
        .children_snapshot()
        .iter()
        .any(|process| Arc::ptr_eq(process, &child.process)));
    assert!(child
        .process
        .parent()
        .is_some_and(|linked| Arc::ptr_eq(&linked, &parent.process)));
}

// AGENT: connect RV64 clone/wait4 numbers to the migrated namespace while
// preserving syscall 57 as close and rejecting unsupported clone side effects
// before any child identity, table entry, family link, or run-queue slot exists.
fn riscv_clone_accepts_only_the_fork_equivalent_subset(pool: &FramePool) {
    use crate::syscall_abi::{
        decode_from_trap_frame, map_riscv_nr, INTERNAL_SYS_CLONE, INTERNAL_SYS_CLOSE,
        INTERNAL_SYS_WAIT4, RISCV_SYS_CLONE, RISCV_SYS_CLOSE, RISCV_SYS_WAIT4,
    };

    assert_eq!(map_riscv_nr(RISCV_SYS_CLONE), Some(INTERNAL_SYS_CLONE));
    assert_eq!(map_riscv_nr(RISCV_SYS_WAIT4), Some(INTERNAL_SYS_WAIT4));
    assert_eq!(map_riscv_nr(RISCV_SYS_CLOSE), Some(INTERNAL_SYS_CLOSE));
    assert_eq!(INTERNAL_SYS_CLONE, SYS_CLONE);
    assert_eq!(INTERNAL_SYS_WAIT4, SYS_WAIT4);

    let mut request_frame = TrapFrame::new();
    request_frame.regs[10..16].copy_from_slice(&[SIGCHLD as usize, 0, 0, 0, 0, 0x55]);
    request_frame.regs[17] = RISCV_SYS_CLONE;
    let request = decode_from_trap_frame(&request_frame);
    assert_eq!(request.internal_nr, Some(INTERNAL_SYS_CLONE));
    assert_eq!(request.args, [SIGCHLD as usize, 0, 0, 0, 0, 0x55]);

    let kernel = Kernel::new(pool.clone());
    kernel.proc_init();
    let parent = kernel.cur_task(0).expect("init should be current");
    let initial_seq = kernel.tasks.seq.load(Ordering::SeqCst);

    assert_eq!(
        kernel.dispatch_syscall_without_signal_delivery(SYS_CLONE, 0, 0, 0, 0, 0, 0),
        Err("einval")
    );
    for [child_stack, parent_tid, tls, child_tid] in
        [[1, 0, 0, 0], [0, 1, 0, 0], [0, 0, 1, 0], [0, 0, 0, 1]]
    {
        assert_eq!(
            kernel.dispatch_syscall_without_signal_delivery(
                SYS_CLONE,
                SIGCHLD as usize,
                child_stack,
                parent_tid,
                tls,
                child_tid,
                0,
            ),
            Err("enotsup")
        );
    }
    assert_eq!(
        kernel.dispatch_syscall_without_signal_delivery(
            SYS_CLONE,
            SIGCHLD as usize | 0x100,
            0,
            0,
            0,
            0,
            0,
        ),
        Err("enotsup")
    );
    assert_eq!(kernel.tasks.task_count(), 1);
    assert_eq!(kernel.tasks.process_count(), 1);
    assert_eq!(kernel.tasks.seq.load(Ordering::SeqCst), initial_seq);
    assert!(parent.process.children_snapshot().is_empty());
    assert!(kernel.run_queue.pick_next().is_none());

    let child_id = kernel
        .dispatch_syscall_without_signal_delivery(SYS_CLONE, SIGCHLD as usize, 0, 0, 0, 0, 0)
        .expect("fork-equivalent clone should succeed");
    let child = kernel
        .tasks
        .find_task(child_id)
        .expect("clone child should be registered");
    assert_eq!(kernel.tasks.task_count(), 2);
    assert_eq!(kernel.tasks.process_count(), 2);
    assert_eq!(child.sched_state(), TaskRunState::Runnable);
    assert_eq!(kernel.run_queue.pick_next(), Some(child_id));
    assert_eq!(
        child
            .snapshot_user_trap_frame()
            .expect("clone child should retain a user frame")
            .regs[10],
        0
    );
    assert!(parent
        .process
        .children_snapshot()
        .iter()
        .any(|process| Arc::ptr_eq(process, &child.process)));
}

// AGENT: the Kernel fork path copies every architectural user register and
// return CSR from the live caller frame while changing only child-side a0.
fn fork_copies_complete_user_frame(pool: &FramePool) {
    let kernel = Kernel::new(pool.clone());
    kernel.proc_init();
    let parent = kernel.cur_task(0).expect("init should be current");
    let mut source = TrapFrame::new();
    for index in 1..source.regs.len() {
        source.regs[index] = 0x1000 + index;
    }
    source.sstatus = 0x20;
    source.sepc = 0x401004;

    let child_id = kernel
        .do_fork_from_frame(&parent, &source)
        .expect("child fork should succeed");
    let child = kernel
        .tasks
        .find_task(child_id)
        .expect("forked child should be registered");
    let child_frame = child
        .snapshot_user_trap_frame()
        .expect("child frame should exist");
    for index in 0..source.regs.len() {
        let expected = if index == 10 { 0 } else { source.regs[index] };
        assert_eq!(child_frame.regs[index], expected);
    }
    assert_eq!(child_frame.sstatus, source.sstatus);
    assert_eq!(child_frame.sepc, source.sepc);
}

// AGENT: pin the fork ownership split: Process derives process-wide inherited
// state and resets pending/runtime state, while Task derives caller-thread state.
fn fork_splits_process_and_caller_task_inheritance(pool: &FramePool) {
    let table = TaskTable::new(pool.clone());
    let parent = table.spawn_root().expect("root spawn should work");
    *parent.process.exec_path.lock().unwrap() = "/bin/fork-parent".to_string();
    assert!(parent.process.set_signal_action(
        SIGUSR1,
        SigAction {
            handler: 0x5000,
            mask: 0x24,
        },
    ));
    parent
        .process
        .sig_queue
        .lock()
        .unwrap()
        .push_back((SIGUSR2 as i32, parent.id() as isize));
    parent.process.did_exec.store(true, Ordering::SeqCst);
    *parent.sig_mask.lock().unwrap() = 0x48;
    parent.sig_frames.lock().unwrap().push(SigFrame {
        saved_frame: TrapFrame::for_user_entry(0x401000, 0x8000_0000),
        saved_mask: 0x12,
    });
    {
        let mut sched = parent.sched.lock().unwrap();
        sched.policy = SchedulePolicy::with_prio(PRIO_MIN + 3);
        sched.slice_left = 1;
    }

    let child = table
        .fork_process(&parent)
        .expect("fork should derive process and caller-task state");

    assert!(!Arc::ptr_eq(&parent.process, &child.process));
    assert_eq!(
        child.process.exec_path.lock().unwrap().as_str(),
        "/bin/fork-parent"
    );
    let child_action = child
        .process
        .sig_state
        .lock()
        .unwrap()
        .get_action(SIGUSR1)
        .expect("child should inherit SIGUSR1 disposition")
        .clone();
    assert_eq!(child_action.handler, 0x5000);
    assert_eq!(child_action.mask, 0x24);
    assert!(child.process.sig_queue.lock().unwrap().is_empty());
    assert!(!child.process.did_exec.load(Ordering::SeqCst));

    assert_eq!(*child.sig_mask.lock().unwrap(), 0x48);
    let child_sig_frames = child.sig_frames.lock().unwrap();
    assert_eq!(child_sig_frames.len(), 1);
    assert_eq!(child_sig_frames[0].saved_frame.sepc, 0x401000);
    assert_eq!(child_sig_frames[0].saved_mask, 0x12);
    drop(child_sig_frames);

    let child_sched = child.sched.lock().unwrap();
    assert_eq!(child_sched.policy.prio, PRIO_MIN + 3);
    assert_eq!(child_sched.slice_left, child_sched.policy.time_slice());
}

// AGENT: fork from a cloned thread must attach the child Process to the shared
// parent Process, never to the calling thread as a family identity.
fn fork_from_nonleader_attaches_process_parent(pool: &FramePool) {
    let table = TaskTable::new(pool.clone());
    let parent = table.spawn_root().expect("root spawn should work");
    let thread = table
        .clone_thread(&parent, 0x8000_0000, 0x123)
        .expect("thread clone should succeed");
    let child = table
        .fork_process(&thread)
        .expect("fork from nonleader should succeed");

    let linked_parent = child
        .process
        .parent()
        .expect("child process should retain a live parent link");
    assert!(Arc::ptr_eq(&linked_parent, &parent.process));
    assert!(Arc::ptr_eq(&thread.process, &parent.process));
    assert!(!Arc::ptr_eq(&child.process, &parent.process));
    let children = parent.process.children_snapshot();
    assert_eq!(children.len(), 1);
    assert!(Arc::ptr_eq(&children[0], &child.process));
}

// AGENT: orphan adoption must move Process children to init and replace their
// weak parent links without retaining the exiting process as an owner.
fn reparent_children_uses_init_process(pool: &FramePool) {
    let table = TaskTable::new(pool.clone());
    let init = table.spawn_root().expect("root spawn should work");
    let parent = table
        .fork_process(&init)
        .expect("parent process fork should work");
    let child = table
        .fork_process(&parent)
        .expect("child process fork should work");

    let adopted_zombie_pids = table.reparent_children_to_init(&parent.process);

    assert!(adopted_zombie_pids.is_empty());
    assert!(parent.process.has_no_children());
    let adopted_parent = child
        .process
        .parent()
        .expect("orphan should be adopted by init");
    assert!(Arc::ptr_eq(&adopted_parent, &init.process));
    let init_children = init.process.children_snapshot();
    assert!(init_children
        .iter()
        .any(|process| Arc::ptr_eq(process, &parent.process)));
    assert!(init_children
        .iter()
        .any(|process| Arc::ptr_eq(process, &child.process)));
}

// AGENT: a zombie that is adopted after its original parent exits must publish
// fresh child-exit readiness and SIGCHLD to init because its earlier exit only
// notified the old parent.
fn reparented_zombie_notifies_init(pool: &FramePool) {
    let kernel = Kernel::new(pool.clone());
    kernel.proc_init();
    let init = kernel.cur_task(0).expect("init should be current");
    assert!(init.process.set_signal_action(
        SIGCHLD,
        SigAction {
            handler: 0x5000,
            mask: 0,
        },
    ));

    let intermediate = kernel
        .tasks
        .fork_process(&init)
        .expect("intermediate process should fork");
    let exiting_parent = kernel
        .tasks
        .fork_process(&intermediate)
        .expect("exiting parent should fork");
    let zombie = kernel
        .tasks
        .fork_process(&exiting_parent)
        .expect("future zombie should fork");
    let zombie_pid = zombie.process.pid();

    kernel.exit_thread_group(0, &zombie, ExitReason::Code(7));
    assert!(zombie.process.is_zombie());

    init.process.ev.lock().unwrap().clear(EvFlag::CHILD_QUIT);
    init.process.sig_queue.lock().unwrap().clear();
    let child_wait_woken = Arc::new(AtomicBool::new(false));
    let wake_observer = child_wait_woken.clone();
    init.process.ev.lock().unwrap().sub(
        EvFlag::CHILD_QUIT,
        Box::new(move |_| {
            wake_observer.store(true, Ordering::SeqCst);
            true
        }),
    );

    kernel.exit_thread_group(0, &exiting_parent, ExitReason::Code(8));

    let adopted_parent = zombie
        .process
        .parent()
        .expect("zombie should be adopted by init");
    assert!(Arc::ptr_eq(&adopted_parent, &init.process));
    assert!(child_wait_woken.load(Ordering::SeqCst));
    assert!(init
        .process
        .sig_queue
        .lock()
        .unwrap()
        .iter()
        .any(|(signo, sender)| { *signo == SIGCHLD as i32 && *sender == zombie_pid as isize }));
    assert_eq!(
        kernel.do_wait(init.id(), zombie_pid as isize, 1),
        Ok((zombie_pid, 7 << 8))
    );
}

// AGENT: multi-threaded zombies are collected at process granularity, while all
// same-process task-table entries disappear in the single reap step.
fn reap_zombie_process_removes_thread_group_once(pool: &FramePool) {
    let table = TaskTable::new(pool.clone());
    let parent = table.spawn_root().expect("root spawn should work");
    let child = table
        .fork_process(&parent)
        .expect("child fork should succeed");
    let thread = table
        .clone_thread(&child, 0x8000_0000, 0)
        .expect("thread clone should succeed");

    let child_pid = child.process.pid();
    let child_process = Arc::downgrade(&child.process);
    assert!(child
        .process
        .begin_group_exit(ExitReason::Code(7))
        .is_some());
    child.process.finish_process_exit();
    assert_eq!(
        table
            .zombie_processes()
            .iter()
            .map(|process| process.pid())
            .collect::<Vec<_>>(),
        vec![child_pid]
    );
    assert_eq!(table.reap(thread.id()), Err("esrch"));
    assert_eq!(table.reap(child_pid), Ok(()));

    assert!(table.find_task(child.id()).is_none());
    assert!(table.find_task(thread.id()).is_none());
    assert!(table.find_process(child_pid).is_none());
    assert!(parent.process.has_no_children());
    drop(thread);
    drop(child);
    assert!(child_process.upgrade().is_none());
}

// AGENT: prove Exiting closes thread admission immediately but remains invisible
// to wait/reap until the separate Zombie commit publishes completed teardown.
fn exiting_phase_blocks_clone_wait_and_reap(pool: &FramePool) {
    let table = TaskTable::new(pool.clone());
    let task = table.spawn().expect("standalone spawn should work");
    let pid = task.process.pid();

    assert_eq!(
        task.process.begin_group_exit(ExitReason::Code(3)),
        Some(vec![task.id()])
    );
    assert!(task.process.is_terminating());
    assert!(!task.process.is_zombie());
    assert_eq!(task.process.zombie_wait_status(), None);
    assert_eq!(table.reap(pid), Err("ebusy"));
    assert_eq!(
        table.clone_thread(&task, 0x8000_0000, 0).err(),
        Some("eexist")
    );

    task.process.finish_process_exit();
    assert!(!task.process.is_terminating());
    assert!(task.process.is_zombie());
    assert_eq!(task.process.zombie_wait_status(), Some(3 << 8));
    assert_eq!(table.reap(pid), Ok(()));
}

// AGENT: exercise nonleader exit followed by final-thread exit: shared
// resources survive only the first step, while final exit releases zombie-only
// redundant paths, subscriptions, and saved signal-frame backing storage.
fn nonleader_exit_keeps_leader_resources_and_parent_quiet(pool: &FramePool) {
    let kernel = Kernel::new(pool.clone());
    kernel.proc_init();
    let parent = kernel.cur_task(0).expect("init should be current");
    assert!(parent.process.set_signal_action(
        SIGCHLD,
        SigAction {
            handler: 0x5000,
            mask: 0,
        },
    ));
    let leader = kernel
        .tasks
        .fork_process(&parent)
        .expect("child process should fork");
    let thread = kernel
        .tasks
        .clone_thread(&leader, 0x8000_0000, 0)
        .expect("child thread should clone");
    let child_pid = leader.process.pid();
    let mapped_addr = 0x1900_0000;
    *leader.process.exec_path.lock().unwrap() = "/thread-exit".to_string();
    leader
        .process
        .ev
        .lock()
        .unwrap()
        .sub(EvFlag::CHILD_QUIT, Box::new(|_| false));
    leader.sig_frames.lock().unwrap().push(SigFrame {
        saved_frame: TrapFrame::new(),
        saved_mask: 1,
    });
    leader
        .process
        .addr_space
        .lock()
        .unwrap()
        .map_region(
            VmRegion::new(mapped_addr, PAGE_SZ, VM_READ | VM_WRITE),
            pool,
        )
        .expect("shared child page should map");
    let fd = leader
        .add_file(FLike::File(FHandle::new(standalone_regular_file())))
        .expect("shared child fd should install");
    // AGENT: process-associated locks survive one non-last thread exit and are
    // released only when the final thread enters process-wide teardown.
    let exit_lock = leader
        .get_fd_entry(fd)
        .expect("exit lock fd should remain installed")
        .record_lock_request(
            FlockArg {
                lock_type: F_RDLCK,
                whence: SEEK_SET,
                start: 0,
                len: 0,
                pid: 0,
            },
            true,
        )
        .expect("exit lock should normalize");
    kernel
        .record_locks
        .set_nonblocking(child_pid, exit_lock)
        .expect("exit lock should install");

    leader.set_sched_state(TaskRunState::Runnable);
    kernel.run_queue.enqueue(&leader);
    assert_eq!(
        kernel.exit_current_thread(0, &thread, ExitReason::Code(5)),
        Ok(())
    );

    assert!(thread.done());
    assert!(!leader.done());
    assert!(!leader.process.is_terminating());
    assert_eq!(leader.process.thread_ids(), vec![leader.id()]);
    assert!(kernel.tasks.find_task(thread.id()).is_none());
    assert!(kernel.tasks.find_task(leader.id()).is_some());
    assert!(kernel.tasks.find_process(child_pid).is_some());
    assert_eq!(kernel.cur_task(0).map(|task| task.id()), Some(parent.id()));
    assert!(leader.get_fd_entry(fd).is_some());
    assert!(kernel.record_locks.process_has_locks(child_pid));
    assert_eq!(
        leader.process.exec_path.lock().unwrap().as_str(),
        "/thread-exit"
    );
    assert_eq!(leader.process.ev.lock().unwrap().cb_len(), 1);
    assert_eq!(leader.sig_frames.lock().unwrap().len(), 1);
    assert!(leader
        .process
        .addr_space
        .lock()
        .unwrap()
        .mapped_region(mapped_addr)
        .is_some());
    assert_eq!(parent.process.ev.lock().unwrap().ev & EvFlag::CHILD_QUIT, 0);
    assert!(parent.process.sig_queue.lock().unwrap().is_empty());
    assert_eq!(
        kernel.do_wait(parent.id(), child_pid as isize, 1),
        Ok((0, 0))
    );

    assert_eq!(
        kernel.exit_current_thread(0, &leader, ExitReason::Code(9)),
        Ok(())
    );

    assert!(leader.done());
    assert!(leader.process.is_zombie());
    assert!(!kernel.record_locks.process_has_locks(child_pid));
    assert!(leader.get_fd_entry(fd).is_none());
    assert!(leader.process.exec_path.lock().unwrap().is_empty());
    assert_eq!(leader.process.ev.lock().unwrap().cb_len(), 0);
    assert_ne!(leader.process.ev.lock().unwrap().ev & EvFlag::PROC_QUIT, 0);
    assert_eq!(
        leader.process.sig_state.lock().unwrap().actions.capacity(),
        0
    );
    assert_eq!(leader.sig_frames.lock().unwrap().capacity(), 0);
    assert!(leader
        .process
        .addr_space
        .lock()
        .unwrap()
        .mapped_region(mapped_addr)
        .is_none());
    assert_ne!(parent.process.ev.lock().unwrap().ev & EvFlag::CHILD_QUIT, 0);
    assert!(parent
        .process
        .sig_queue
        .lock()
        .unwrap()
        .iter()
        .any(|(signo, sender)| *signo == SIGCHLD as i32 && *sender == child_pid as isize));
    assert_eq!(
        kernel.do_wait(parent.id(), child_pid as isize, 1),
        Ok((child_pid, 9 << 8))
    );
    assert_eq!(kernel.tasks.reap(child_pid), Ok(()));
}

// AGENT: prove a thread-group leader is only a Task for thread-exit purposes; its
// Process identity remains registered while a nonleader sibling is still alive.
fn leader_exit_keeps_remaining_thread_and_process(pool: &FramePool) {
    let kernel = Kernel::new(pool.clone());
    let leader = kernel
        .tasks
        .spawn_root()
        .expect("init process leader should spawn");
    let thread = kernel
        .tasks
        .clone_thread(&leader, 0x8000_0000, 0)
        .expect("thread clone should succeed");
    thread.set_sched_state(TaskRunState::Runnable);
    kernel.run_queue.enqueue(&thread);

    assert_eq!(
        kernel.exit_current_thread(0, &leader, ExitReason::Code(4)),
        Ok(())
    );

    assert!(leader.done());
    assert!(!thread.done());
    assert!(!leader.process.is_terminating());
    assert_eq!(leader.process.thread_ids(), vec![thread.id()]);
    assert!(kernel.tasks.find_task(leader.id()).is_none());
    assert!(kernel.tasks.find_task(thread.id()).is_some());
    assert!(kernel.tasks.find_process(leader.process.pid()).is_some());
    assert!(kernel.cur_task(0).is_none());
}

// AGENT: ensure exit_group marks and detaches every runnable sibling while
// retaining their zombie task records for the later process-level reap.
fn exit_group_terminates_every_thread(pool: &FramePool) {
    let kernel = Kernel::new(pool.clone());
    kernel.proc_init();
    let init = kernel.cur_task(0).expect("init should be current");
    let leader = kernel
        .tasks
        .fork_process(&init)
        .expect("child process should fork");
    let first = kernel
        .tasks
        .clone_thread(&leader, 0x8000_0000, 0)
        .expect("first thread should clone");
    let second = kernel
        .tasks
        .clone_thread(&leader, 0x8100_0000, 0)
        .expect("second thread should clone");
    for thread in [&first, &second] {
        thread.set_sched_state(TaskRunState::Runnable);
        kernel.run_queue.enqueue(thread);
    }
    kernel.exit_thread_group(0, &leader, ExitReason::Code(11));

    assert!(leader.done());
    assert!(first.done());
    assert!(second.done());
    assert!(leader.process.is_zombie());
    assert_eq!(leader.process.zombie_wait_status(), Some(11 << 8));
    assert_eq!(leader.process.thread_count(), 3);
    assert_eq!(kernel.tasks.task_count(), 4);
    assert!(kernel.run_queue.pick_next().is_none());
    assert_eq!(kernel.cur_task(0).map(|task| task.id()), Some(init.id()));
}

// AGENT: default-fatal signal delivery must use group exit so every sibling is
// terminal and the process wait status records the terminating signal.
fn fatal_signal_terminates_every_thread(pool: &FramePool) {
    let kernel = Kernel::new(pool.clone());
    kernel.proc_init();
    let init = kernel.cur_task(0).expect("init should be current");
    let leader = kernel
        .tasks
        .fork_process(&init)
        .expect("child process should fork");
    let thread = kernel
        .tasks
        .clone_thread(&leader, 0x8000_0000, 0)
        .expect("thread clone should succeed");
    thread.set_sched_state(TaskRunState::Runnable);
    kernel.run_queue.enqueue(&thread);
    kernel.send_signal_to_task(&leader, SIGUSR1 as i32, -1);
    let pending = leader
        .take_deliverable_signal()
        .expect("default-fatal signal should be deliverable");
    assert_eq!(
        pending.action.resolve(pending.signo),
        SignalDeliveryAction::Terminate
    );
    kernel.exit_thread_group(0, &leader, ExitReason::Signal(pending.signo as u8));

    assert!(leader.done());
    assert!(thread.done());
    assert!(leader.process.is_zombie());
    assert_eq!(leader.process.zombie_wait_status(), Some(SIGUSR1 as usize));
    assert!(kernel.run_queue.pick_next().is_none());
    assert_eq!(kernel.cur_task(0).map(|task| task.id()), Some(init.id()));
}

// AGENT: pin Linux/RISC-V exit 93 and exit_group 94 to separate x86_64-style
// internal syscall ids so dispatch cannot collapse their lifecycle semantics.
fn riscv_exit_numbers_map_to_distinct_internal_calls() {
    use crate::syscall_abi::{
        map_riscv_nr, INTERNAL_SYS_EXIT, INTERNAL_SYS_EXIT_GROUP, RISCV_SYS_EXIT,
        RISCV_SYS_EXIT_GROUP,
    };

    assert_eq!(map_riscv_nr(RISCV_SYS_EXIT), Some(INTERNAL_SYS_EXIT));
    assert_eq!(
        map_riscv_nr(RISCV_SYS_EXIT_GROUP),
        Some(INTERNAL_SYS_EXIT_GROUP)
    );
    assert_ne!(INTERNAL_SYS_EXIT, INTERNAL_SYS_EXIT_GROUP);
}

// AGENT: clone_thread inherits caller-local context and scheduling policy, then
// applies the clone-specific return value, user stack, TLS, and signal mask.
fn clone_thread_copies_caller_context_and_shares_process(pool: &FramePool) {
    let table = TaskTable::new(pool.clone());
    let task = table.spawn().expect("standalone spawn should work");
    let stack_top = 0x8000_0000;
    let tls = 0xabc;
    let sig_mask = 0x24;

    let mut source = TrapFrame::new();
    for index in 1..source.regs.len() {
        source.regs[index] = 0x2000 + index;
    }
    source.regs[2] = 0x9000_0000;
    source.regs[10] = 99;
    source.sstatus = 0x20;
    source.sepc = 0x401000;
    task.install_user_trap_frame(source.clone())
        .expect("source frame should install");
    *task.sig_mask.lock().unwrap() = sig_mask;
    task.sched.lock().unwrap().policy = SchedulePolicy::with_prio(PRIO_MIN + 4);

    let thread = table
        .clone_thread(&task, stack_top, tls)
        .expect("thread clone should succeed");

    assert!(Arc::ptr_eq(&task.process, &thread.process));
    assert!(task.process.thread_ids().contains(&thread.id()));
    assert_eq!(*thread.sig_mask.lock().unwrap(), sig_mask);
    let thread_sched = thread.sched.lock().unwrap();
    assert_eq!(thread_sched.policy.prio, PRIO_MIN + 4);
    assert_eq!(thread_sched.slice_left, thread_sched.policy.time_slice());
    drop(thread_sched);
    let cloned = thread
        .snapshot_user_trap_frame()
        .expect("cloned frame should exist");
    for index in 0..source.regs.len() {
        let expected = match index {
            2 => stack_top as usize,
            4 => tls as usize,
            10 => 0,
            _ => source.regs[index],
        };
        assert_eq!(cloned.regs[index], expected);
    }
    assert_eq!(cloned.sepc, source.sepc);
    assert_eq!(cloned.sstatus, source.sstatus);
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

// AGENT: exercise the ELF image builder used by exec, including file bytes,
// zero-filled bss, final permissions, and argv.
fn prepared_user_image_loads_elf_segment_and_stack(pool: &FramePool) {
    let segment_offset = PAGE_SZ + 0x234;
    let segment_vaddr = 0x0040_1234;
    let payload = b"init";
    let elf = test_elf_with_load_segment(segment_offset, segment_vaddr, payload, 16);
    let mut image = prepare_user_image(
        &elf,
        vec!["init".to_string()],
        vec!["A=B".to_string()],
        pool,
    )
    .expect("shared ELF image builder should succeed");

    assert_eq!(image.user_entry.entry, segment_vaddr);
    let mut loaded = [0u8; 4];
    image
        .addr_space
        .read_user_bytes(segment_vaddr, &mut loaded)
        .expect("ELF payload should be readable");
    assert_eq!(&loaded, payload);
    let mut bss = [0xffu8; 12];
    image
        .addr_space
        .read_user_bytes(segment_vaddr + payload.len(), &mut bss)
        .expect("ELF bss should be readable");
    assert_eq!(bss, [0u8; 12]);
    assert!(image
        .addr_space
        .write_user_bytes(segment_vaddr, b"x", pool)
        .is_err());
    let sigtramp = image
        .addr_space
        .mapped_region(USER_SIGTRAMP)
        .expect("prepared user image should contain the signal restorer page");
    assert_eq!(sigtramp.flags, VM_READ | VM_EXEC);
    let mut sigtramp_code = [0u8; USER_SIGTRAMP_CODE.len()];
    image
        .addr_space
        .read_user_bytes(USER_SIGTRAMP, &mut sigtramp_code)
        .expect("signal restorer code should be readable");
    assert_eq!(sigtramp_code, USER_SIGTRAMP_CODE);
    assert!(image
        .addr_space
        .write_user_bytes(USER_SIGTRAMP, b"x", pool)
        .is_err());

    let sp = image.user_entry.stack_pointer;
    assert_eq!(image.addr_space.read_user_usize(sp).unwrap(), 1);
    let argv0 = image
        .addr_space
        .read_user_usize(sp + mem::size_of::<usize>())
        .unwrap();
    assert_user_cstr(&image.addr_space, argv0, "init");
    assert_eq!(image.addr_space.brk(), 0x0040_2000);

    image.addr_space.release_all_pages();
}

// AGENT: keep a PT_LOAD with no PF_R/PF_W/PF_X bits inaccessible after its
// payload is copied through the loader's temporary writable mapping.
fn prepared_user_image_preserves_no_access_segment(pool: &FramePool) {
    let code_vaddr = 0x0040_0000;
    let no_access_vaddr = 0x0060_0000;
    let mut elf = test_elf_with_load_segment(PAGE_SZ, code_vaddr, b"code", 4);
    append_test_load_segment(&mut elf, PAGE_SZ * 2, no_access_vaddr, b"secret", 16, 0);

    let mut image = prepare_user_image(&elf, Vec::new(), Vec::new(), pool)
        .expect("loader should populate a no-access PT_LOAD before protecting it");
    assert_eq!(
        image
            .addr_space
            .mapped_region(no_access_vaddr)
            .expect("no-access PT_LOAD should remain mapped")
            .flags,
        0
    );

    let mut byte = [0u8; 1];
    assert!(image
        .addr_space
        .read_user_bytes(no_access_vaddr, &mut byte)
        .is_err());
    assert!(image
        .addr_space
        .write_user_bytes(no_access_vaddr, b"x", pool)
        .is_err());

    image.addr_space.release_all_pages();
}

// AGENT: keep foreign-machine, unsupported ET_DYN, and invalid-entry ELF
// images from reaching the RISC-V address-space construction path.
fn parse_elf_rejects_unsupported_or_invalid_entry() {
    const PH_OFF: usize = 64;
    let segment_offset = PAGE_SZ;
    let segment_vaddr = 0x0040_0000;
    let payload = b"code";

    let mut foreign_machine =
        test_elf_with_load_segment(segment_offset, segment_vaddr, payload, payload.len());
    write_test_u16(&mut foreign_machine, 18, 0x3E);
    assert_eq!(parse_elf(&foreign_machine).unwrap_err(), "bad_machine");

    let mut dynamic =
        test_elf_with_load_segment(segment_offset, segment_vaddr, payload, payload.len());
    write_test_u16(&mut dynamic, 16, 3);
    assert_eq!(parse_elf(&dynamic).unwrap_err(), "not_exec");

    let mut unmapped_entry =
        test_elf_with_load_segment(segment_offset, segment_vaddr, payload, payload.len());
    write_test_u64(
        &mut unmapped_entry,
        24,
        (segment_vaddr + payload.len()) as u64,
    );
    assert_eq!(parse_elf(&unmapped_entry).unwrap_err(), "bad_entry");

    let mut non_executable =
        test_elf_with_load_segment(segment_offset, segment_vaddr, payload, payload.len());
    write_test_u32(&mut non_executable, PH_OFF + 4, 0x4);
    assert_eq!(parse_elf(&non_executable).unwrap_err(), "bad_entry");
}

// AGENT: exercise checked program-header traversal for the supported static
// Sv39 PT_LOAD layout contract.
fn parse_elf_validates_program_header_layouts() {
    const PH_OFF: usize = 64;

    let mut interpreted = test_elf_with_load_segment(PAGE_SZ, 0x0040_0000, b"code", 4);
    write_test_u32(&mut interpreted, PH_OFF, 3);
    assert_eq!(parse_elf(&interpreted).unwrap_err(), "enotsup");

    let noncanonical_vaddr = (1usize << 38) - PAGE_SZ;
    let outside_user =
        test_elf_with_load_segment(PAGE_SZ, noncanonical_vaddr, b"code", PAGE_SZ * 2);
    assert_eq!(parse_elf(&outside_user).unwrap_err(), "bad_phdr");

    let mut truncated_table = test_elf_with_load_segment(PAGE_SZ, 0x0040_0000, b"code", 4);
    write_test_u64(&mut truncated_table, 32, u64::MAX);
    assert_eq!(parse_elf(&truncated_table).unwrap_err(), "ph_overflow");

    let mut overlapping = test_elf_with_load_segment(PAGE_SZ + 0x100, 0x0040_0100, b"left", 0x300);
    append_test_load_segment(
        &mut overlapping,
        PAGE_SZ * 2 + 0x200,
        0x0040_0200,
        b"right",
        0x100,
        0x6,
    );
    assert_eq!(parse_elf(&overlapping).unwrap_err(), "bad_phdr");
}

// AGENT: keep long and adjacent same-permission PT_LOAD mappings compact so
// normalization cost and output size depend on segment boundaries, not pages.
fn elf_load_regions_coalesce_contiguous_permissions() {
    let base = 0x0040_0000;
    let first_pages = 8;
    let second_pages = 4;
    let mut elf = test_elf_with_load_segment(PAGE_SZ, base, b"code", first_pages * PAGE_SZ);
    append_test_load_segment(
        &mut elf,
        PAGE_SZ * 2,
        base + first_pages * PAGE_SZ,
        b"tail",
        second_pages * PAGE_SZ,
        0x5,
    );

    let parsed = parse_elf(&elf).expect("adjacent PT_LOAD segments should parse");
    let regions = normalize_elf_load_regions(&parsed.load_segments)
        .expect("adjacent same-permission segments should normalize");
    assert_eq!(
        regions,
        vec![ElfLoadRegion {
            base,
            len: (first_pages + second_pages) * PAGE_SZ,
            flags: 0x5,
        }]
    );
}

// AGENT: allow two PT_LOAD segments to share one virtual page, load both byte
// ranges, keep BSS zeroed, and union RX/RW into the page's final permissions.
fn prepared_user_image_loads_segments_sharing_a_page(pool: &FramePool) {
    let page_vaddr = 0x0040_0000;
    let left_vaddr = page_vaddr + 0x100;
    let right_vaddr = page_vaddr + 0x900;
    let mut elf = test_elf_with_load_segment(PAGE_SZ + 0x100, left_vaddr, b"left", 0x180);
    append_test_load_segment(
        &mut elf,
        PAGE_SZ * 2 + 0x900,
        right_vaddr,
        b"right",
        0x180,
        0x6,
    );

    let parsed = parse_elf(&elf).expect("shared-page PT_LOAD segments should parse");
    assert_eq!(parsed.load_segments.len(), 2);
    let regions = normalize_elf_load_regions(&parsed.load_segments)
        .expect("shared-page PT_LOAD segments should normalize");
    assert_eq!(regions.len(), 1);
    assert_eq!(regions[0].base, page_vaddr);
    assert_eq!(regions[0].len, PAGE_SZ);
    assert_eq!(regions[0].flags, 0x7);

    let mut image = prepare_user_image(&elf, Vec::new(), Vec::new(), pool)
        .expect("shared-page PT_LOAD segments should load");
    let mut left = [0u8; 4];
    let mut right = [0u8; 5];
    let mut left_bss = [0xffu8; 8];
    let mut right_bss = [0xffu8; 8];
    image
        .addr_space
        .read_user_bytes(left_vaddr, &mut left)
        .unwrap();
    image
        .addr_space
        .read_user_bytes(right_vaddr, &mut right)
        .unwrap();
    image
        .addr_space
        .read_user_bytes(left_vaddr + left.len(), &mut left_bss)
        .unwrap();
    image
        .addr_space
        .read_user_bytes(right_vaddr + right.len(), &mut right_bss)
        .unwrap();
    assert_eq!(&left, b"left");
    assert_eq!(&right, b"right");
    assert_eq!(left_bss, [0u8; 8]);
    assert_eq!(right_bss, [0u8; 8]);
    image
        .addr_space
        .write_user_bytes(right_vaddr, b"R", pool)
        .expect("RW segment should make the shared page writable");
    assert_eq!(
        image.addr_space.mapped_region(page_vaddr).unwrap().flags,
        VM_READ | VM_WRITE | VM_EXEC
    );
    image.addr_space.release_all_pages();
}

// AGENT: prove that program-header order does not control virtual-address order
// and that both nonoverlapping PT_LOAD segments reach the real image builder.
fn prepared_user_image_loads_out_of_order_segments(pool: &FramePool) {
    let high_vaddr = 0x0060_0000;
    let low_vaddr = 0x0040_0000;
    let mut elf = test_elf_with_load_segment(PAGE_SZ, high_vaddr, b"high", 8);
    append_test_load_segment(&mut elf, PAGE_SZ * 2, low_vaddr, b"low", PAGE_SZ, 0x6);

    let parsed = parse_elf(&elf).expect("out-of-order nonoverlapping segments should parse");
    assert_eq!(parsed.load_segments.len(), 2);
    assert_eq!(parsed.load_segments[0].vaddr, high_vaddr);
    assert_eq!(parsed.load_segments[1].vaddr, low_vaddr);

    let mut image = prepare_user_image(&elf, Vec::new(), Vec::new(), pool)
        .expect("out-of-order nonoverlapping segments should load");
    let mut high = [0u8; 4];
    let mut low = [0u8; 3];
    image
        .addr_space
        .read_user_bytes(high_vaddr, &mut high)
        .unwrap();
    image
        .addr_space
        .read_user_bytes(low_vaddr, &mut low)
        .unwrap();
    assert_eq!(&high, b"high");
    assert_eq!(&low, b"low");
    assert_eq!(image.user_entry.entry, high_vaddr);
    image.addr_space.release_all_pages();
}

// AGENT: build a compact ELF64 fixture with one PT_LOAD segment for the QEMU
// process-image selftest without depending on host-side ELF tooling.
fn test_elf_with_load_segment(
    offset: usize,
    vaddr: usize,
    payload: &[u8],
    mem_size: usize,
) -> Vec<u8> {
    const PH_OFF: usize = 64;
    const PH_SIZE: usize = 56;
    let mut data = vec![0u8; PH_OFF];
    data[0..4].copy_from_slice(b"\x7fELF");
    data[4] = 2;
    data[5] = 1;
    data[6] = 1;
    write_test_u16(&mut data, 16, 2);
    write_test_u16(&mut data, 18, 0xF3);
    write_test_u32(&mut data, 20, 1);
    write_test_u64(&mut data, 24, vaddr as u64);
    write_test_u64(&mut data, 32, PH_OFF as u64);
    write_test_u16(&mut data, 52, 64);
    write_test_u16(&mut data, 54, PH_SIZE as u16);
    write_test_u16(&mut data, 56, 0);
    append_test_load_segment(&mut data, offset, vaddr, payload, mem_size, 0x5);
    data
}

// AGENT: append one ELF64 PT_LOAD entry and its payload to a synthetic image so
// process selftests can cover multi-segment ordering and overlap behavior.
fn append_test_load_segment(
    data: &mut Vec<u8>,
    offset: usize,
    vaddr: usize,
    payload: &[u8],
    mem_size: usize,
    flags: u32,
) {
    const PH_OFF: usize = 64;
    const PH_SIZE: usize = 56;
    let ph_num = u16::from_le_bytes([data[56], data[57]]) as usize;
    let base = PH_OFF + ph_num * PH_SIZE;
    let required_len = (base + PH_SIZE).max(offset + payload.len());
    data.resize(required_len, 0);

    write_test_u32(data, base, 1);
    write_test_u32(data, base + 4, flags);
    write_test_u64(data, base + 8, offset as u64);
    write_test_u64(data, base + 16, vaddr as u64);
    write_test_u64(data, base + 24, vaddr as u64);
    write_test_u64(data, base + 32, payload.len() as u64);
    write_test_u64(data, base + 40, mem_size as u64);
    write_test_u64(data, base + 48, PAGE_SZ as u64);
    data[offset..offset + payload.len()].copy_from_slice(payload);
    write_test_u16(data, 56, (ph_num + 1) as u16);
}

// AGENT: encode one little-endian u16 in the synthetic ELF fixture.
fn write_test_u16(data: &mut [u8], offset: usize, value: u16) {
    data[offset..offset + 2].copy_from_slice(&value.to_le_bytes());
}

// AGENT: encode one little-endian u32 in the synthetic ELF fixture.
fn write_test_u32(data: &mut [u8], offset: usize, value: u32) {
    data[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
}

// AGENT: encode one little-endian u64 in the synthetic ELF fixture.
fn write_test_u64(data: &mut [u8], offset: usize, value: u64) {
    data[offset..offset + 8].copy_from_slice(&value.to_le_bytes());
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

// AGENT: cover two virtual mappings that own the same physical frame inside one
// address space and verify bulk release drops both resident aliases.
fn release_all_pages_drops_same_space_aliases(pool: &FramePool) {
    let base = 0x3100_0000;
    let frame = pool
        .alloc_pg_frame()
        .expect("alias regression should allocate one shared frame");
    let owner = SharedPage::new(frame);
    let aliases = [owner.clone(), owner.clone()];
    let mut addr_space = AddrSpace::new();

    addr_space
        .map_shared_pages(
            VmRegion::new(base, PAGE_SZ * aliases.len(), VM_READ | VM_WRITE),
            &aliases,
            pool,
        )
        .expect("same-space aliases should map");
    drop(aliases);
    assert_eq!(owner.sharers(), 3);

    addr_space.release_all_pages();
    addr_space
        .check_page_table_consistency()
        .expect("alias release should leave no resident or Sv39 mappings");
    assert!(addr_space.mapped_region(base).is_none());
    assert_eq!(owner.sharers(), 1);
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
