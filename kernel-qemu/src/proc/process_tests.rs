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
    proc_init_push_at_writes_user_stack(pool);
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
