// AGENT: boot-time process regressions that need the QEMU FramePool, Sv39
// page-table walker, and physical user-copy path to be initialized.
use super::*;

// AGENT: expose ProcInit stack construction checks to the optional QEMU boot
// selftest path.
pub fn run_all(pool: &FramePool) {
    proc_init_push_at_writes_user_stack(pool);
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
    assert_eq!(addr_space.read_user_usize(auxv).unwrap(), AT_PAGESZ as usize);
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
