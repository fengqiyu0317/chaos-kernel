// AGENT: boot-time process regressions that need the QEMU FramePool, Sv39
// page-table walker, and physical user-copy path to be initialized.
use super::*;

// AGENT: expose process, initial-stack, and shared ELF image regressions to the
// optional QEMU boot selftest path.
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
    parse_elf_rejects_unsupported_or_invalid_entry();
    parse_elf_validates_program_header_layouts();
    prepared_user_image_loads_elf_segment_and_stack(pool);
    prepared_user_image_loads_out_of_order_segments(pool);
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

// AGENT: exercise the shared ELF image builder used by both new_user_task and
// exec, including file bytes, zero-filled bss, final permissions, and argv.
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

    assert_eq!(image.thd_ctx.uctx.ip, segment_vaddr as u64);
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

    let sp = image.thd_ctx.uctx.r[N_REGS - 1] as usize;
    assert_eq!(image.addr_space.read_user_usize(sp).unwrap(), 1);
    let argv0 = image
        .addr_space
        .read_user_usize(sp + mem::size_of::<usize>())
        .unwrap();
    assert_user_cstr(&image.addr_space, argv0, "init");
    assert_eq!(image.addr_space.vm_map.brk, 0x0040_2000);

    image.addr_space.release_all_pages(pool);
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

// AGENT: exercise checked program-header traversal and the explicitly supported
// static, nonoverlapping Sv39 PT_LOAD layout contract.
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

    let mut overlapping = test_elf_with_load_segment(PAGE_SZ + 0x100, 0x0040_0100, b"left", 0x100);
    append_test_load_segment(
        &mut overlapping,
        PAGE_SZ * 2 + 0x900,
        0x0040_0900,
        b"right",
        0x100,
        0x5,
    );
    assert_eq!(parse_elf(&overlapping).unwrap_err(), "overlap");
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
    assert_eq!(image.thd_ctx.uctx.ip, high_vaddr as u64);
    image.addr_space.release_all_pages(pool);
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
