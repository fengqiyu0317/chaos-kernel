// AGENT
use super::*;

// AGENT: keep the legacy futex command ABI explicit so modifier bits are
// removed without accidentally truncating distinct commands such as
// FUTEX_CMP_REQUEUE (4) and FUTEX_WAIT_BITSET (9) to one ad-hoc low nibble.
const FUTEX_WAIT: u32 = 0;
const FUTEX_WAKE: u32 = 1;
const FUTEX_REQUEUE: u32 = 3;
const FUTEX_CMP_REQUEUE: u32 = 4;
const FUTEX_WAKE_OP: u32 = 5;
const FUTEX_WAIT_BITSET: u32 = 9;
const FUTEX_WAKE_BITSET: u32 = 10;
const FUTEX_PRIVATE_FLAG: u32 = 0x80;
const FUTEX_CLOCK_REALTIME: u32 = 0x100;
const FUTEX_CMD_MASK: u32 = !(FUTEX_PRIVATE_FLAG | FUTEX_CLOCK_REALTIME);
const FUTEX_BITSET_MATCH_ANY: u32 = u32::MAX;

// AGENT: futex syscall now reads futex words and timeout structures through the
// current task address space instead of directly dereferencing user pointers.
pub(super) fn sys_futex(
    kernel: &Kernel,
    a0: usize,
    a1: usize,
    a2: usize,
    a3: usize,
    a4: usize,
    a5: usize,
) -> Result<usize, &'static str> {
    let uaddr = a0;
    // AGENT: Linux exposes op, val, val2, and val3 as 32-bit syscall fields
    // even on RV64; truncate register garbage before interpreting the ABI.
    let op = a1 as u32;
    let val = a2 as u32;
    let timeout_addr = a3;
    let uaddr2 = a4;
    let val3 = a5 as u32;
    // AGENT TODO: operations without FUTEX_PRIVATE_FLAG still use the
    // process-owned bucket until the shared-key registry is implemented.
    let private = op & FUTEX_PRIVATE_FLAG != 0;
    let clock_realtime = op & FUTEX_CLOCK_REALTIME != 0;
    let futex_op = op & FUTEX_CMD_MASK;
    // AGENT: legacy FUTEX_CLOCK_REALTIME is valid only for the implemented
    // absolute WAIT_BITSET command; ordinary WAIT remains relative monotonic.
    if clock_realtime && futex_op != FUTEX_WAIT_BITSET {
        return Err("enosys");
    }
    if !matches!(
        futex_op,
        FUTEX_WAIT
            | FUTEX_WAKE
            | FUTEX_REQUEUE
            | FUTEX_CMP_REQUEUE
            | FUTEX_WAKE_OP
            | FUTEX_WAIT_BITSET
            | FUTEX_WAKE_BITSET
    ) {
        return Err("enosys");
    }
    // AGENT: Linux rejects a zero bitset before attempting either masked wait
    // or wake; no waiter can intersect an empty selection mask.
    if matches!(futex_op, FUTEX_WAIT_BITSET | FUTEX_WAKE_BITSET) && val3 == 0 {
        return Err("einval");
    }
    if !check_access(uaddr, 4) {
        return Err("efault");
    }
    if uaddr % mem::size_of::<u32>() != 0 {
        return Err("einval");
    }
    match futex_op {
        FUTEX_WAIT => {
            let current = kernel.cur_task(0).ok_or("esrch")?;
            // AGENT: convert FUTEX_WAIT's relative timespec once at the syscall
            // boundary so WaitToken and its timer wheel use only absolute ticks.
            let deadline = read_futex_deadline(
                kernel,
                &current,
                timeout_addr,
                FutexTimeoutKind::RelativeMonotonic,
            )?;
            wait_on_futex(
                kernel,
                &current,
                uaddr,
                val,
                deadline,
                FUTEX_BITSET_MATCH_ANY,
            )
        }
        FUTEX_WAKE => {
            let wake_count = val as usize;
            let current = kernel.cur_task(0).ok_or("esrch")?;
            Ok(current.process.futex.wake(uaddr, wake_count))
        }
        FUTEX_WAIT_BITSET => {
            let current = kernel.cur_task(0).ok_or("esrch")?;
            // AGENT: unlike ordinary WAIT, WAIT_BITSET consumes an absolute
            // deadline in the selected monotonic or realtime clock domain.
            let timeout_kind = if clock_realtime {
                FutexTimeoutKind::AbsoluteRealtime
            } else {
                FutexTimeoutKind::AbsoluteMonotonic
            };
            let deadline = read_futex_deadline(kernel, &current, timeout_addr, timeout_kind)?;
            wait_on_futex(kernel, &current, uaddr, val, deadline, val3)
        }
        FUTEX_WAKE_BITSET => {
            let current = kernel.cur_task(0).ok_or("esrch")?;
            Ok(current.process.futex.wake_bitset(uaddr, val as usize, val3))
        }
        FUTEX_REQUEUE => {
            if !check_access(uaddr2, 4) {
                return Err("efault");
            }
            if uaddr2 % mem::size_of::<u32>() != 0 {
                return Err("einval");
            }
            let requeue_count = checked_requeue_count(timeout_addr as u32)?;
            let wake_limit = checked_requeue_count(val)?;
            let current = kernel.cur_task(0).ok_or("esrch")?;
            // AGENT: shared futex keys require a live backing mapping, while a
            // private key is the address-space identity plus virtual address.
            if !private {
                let _ = read_user_u32(kernel, &current, uaddr2)?;
            }
            Ok(current
                .process
                .futex
                .requeue(uaddr, uaddr2, wake_limit, requeue_count))
        }
        FUTEX_WAKE_OP => {
            if !check_access(uaddr2, 4) {
                return Err("efault");
            }
            if uaddr2 % mem::size_of::<u32>() != 0 {
                return Err("einval");
            }
            let val2 = (timeout_addr as u32) as usize;
            let wake_count = val as usize;
            let current = kernel.cur_task(0).ok_or("esrch")?;
            let futex = current.process.futex.clone();
            futex.wake_op(
                uaddr,
                wake_count,
                uaddr2,
                val2,
                || futex_wake_op_apply(kernel, &current, uaddr2, val3),
                |old| futex_wake_op_cmp(old, val3),
            )
        }
        FUTEX_CMP_REQUEUE => {
            if !check_access(uaddr2, 4) {
                return Err("efault");
            }
            if uaddr2 % mem::size_of::<u32>() != 0 {
                return Err("einval");
            }
            let current = kernel.cur_task(0).ok_or("esrch")?;
            // AGENT: validate shared destinations separately from the compared
            // source word because cmp_requeue only reads uaddr itself.
            if !private {
                let _ = read_user_u32(kernel, &current, uaddr2)?;
            }
            let wake_count = checked_requeue_count(val)?;
            let requeue_count = checked_requeue_count(timeout_addr as u32)?;
            let futex = current.process.futex.clone();
            match futex.cmp_requeue(uaddr, uaddr2, wake_count, requeue_count, val3, || {
                read_user_u32(kernel, &current, uaddr)
            }) {
                Ok(n) => Ok(n),
                Err("changed") => Err("eagain"),
                Err(e) => Err(e),
            }
        }
        _ => Err("enosys"),
    }
}

// AGENT: distinguish the sole relative legacy timeout from the absolute clock
// domains used by FUTEX_WAIT_BITSET before converting all three to CLK ticks.
#[derive(Clone, Copy)]
enum FutexTimeoutKind {
    RelativeMonotonic,
    AbsoluteMonotonic,
    AbsoluteRealtime,
}

// AGENT: validate and copy one optional futex timespec, then normalize it to
// the absolute logical-tick deadline consumed by WaitToken and the timer wheel.
fn read_futex_deadline(
    kernel: &Kernel,
    task: &Task,
    timeout_addr: usize,
    kind: FutexTimeoutKind,
) -> Result<Option<usize>, &'static str> {
    if timeout_addr == 0 {
        return Ok(None);
    }
    let timeout_size = 2 * mem::size_of::<i64>();
    if !check_access(timeout_addr, timeout_size) {
        return Err("efault");
    }
    let ticks = duration_to_ticks(read_futex_timeout(kernel, task, timeout_addr)?);
    let deadline = match kind {
        FutexTimeoutKind::RelativeMonotonic => CLK.load(Ordering::Relaxed).saturating_add(ticks),
        FutexTimeoutKind::AbsoluteMonotonic => ticks,
        FutexTimeoutKind::AbsoluteRealtime => {
            ticks.saturating_sub(BOOT_EPOCH.saturating_mul(TIMER_TICK_HZ))
        }
    };
    Ok(Some(deadline))
}

// AGENT: share the compare/enqueue/error translation between WAIT and
// WAIT_BITSET while retaining the latter's explicit waiter selection mask.
fn wait_on_futex(
    kernel: &Kernel,
    task: &Task,
    uaddr: usize,
    expected: u32,
    deadline: Option<usize>,
    bitset: u32,
) -> Result<usize, &'static str> {
    let futex = task.process.futex.clone();
    match futex.wait_bitset(task.id(), uaddr, expected, deadline, bitset, || {
        read_user_u32(kernel, task, uaddr)
    }) {
        Ok(()) => Ok(0),
        Err("changed") => Err("eagain"),
        Err(e) => Err(e),
    }
}

// AGENT: Linux requeue operations explicitly reject wake and move counts that
// become negative int values after the legacy u32 syscall ABI conversion.
fn checked_requeue_count(value: u32) -> Result<usize, &'static str> {
    usize::try_from(i32::try_from(value).map_err(|_| "einval")?).map_err(|_| "einval")
}

// AGENT: futex words are user memory; route reads through the current
// address-space copy-in path instead of directly dereferencing user pointers.
fn read_user_u32(kernel: &Kernel, task: &Task, addr: usize) -> Result<u32, &'static str> {
    let mut bytes = [0u8; mem::size_of::<u32>()];
    task.process
        .addr_space
        .lock()
        .unwrap()
        .read_user_bytes(addr, &mut bytes, &kernel.pool)?;
    Ok(u32::from_ne_bytes(bytes))
}

// AGENT: copy the userspace timespec fields through AddrSpace so unmapped
// timeout pointers return efault and signed negative fields return einval.
fn read_futex_timeout(
    kernel: &Kernel,
    task: &Task,
    timeout_addr: usize,
) -> Result<Duration, &'static str> {
    let tv_nsec_addr = timeout_addr
        .checked_add(mem::size_of::<i64>())
        .ok_or("efault")?;
    let mut addr_space = task.process.addr_space.lock().unwrap();
    let tv_sec = addr_space.read_user_i64(timeout_addr, &kernel.pool)?;
    let tv_nsec = addr_space.read_user_i64(tv_nsec_addr, &kernel.pool)?;
    if tv_sec < 0 || !(0..1_000_000_000).contains(&tv_nsec) {
        return Err("einval");
    }
    Ok(Duration::new(tv_sec as u64, tv_nsec as u32))
}

// AGENT: decode FUTEX_WAKE_OP into one address-space-validated RISC-V atomic
// operation instead of splitting the user-word update into separate copies.
fn futex_wake_op_apply(
    kernel: &Kernel,
    task: &Task,
    uaddr2: usize,
    encoded: u32,
) -> Result<u32, &'static str> {
    const FUTEX_OP_SET: u32 = 0;
    const FUTEX_OP_ADD: u32 = 1;
    const FUTEX_OP_OR: u32 = 2;
    const FUTEX_OP_ANDN: u32 = 3;
    const FUTEX_OP_XOR: u32 = 4;
    const FUTEX_OP_OPARG_SHIFT: u32 = 8;

    let op = (encoded >> 28) & 0xF;
    let op_kind = op & 0x7;
    let mut oparg = sign_extend_12((encoded >> 12) & 0xFFF) as u32;
    if op & FUTEX_OP_OPARG_SHIFT != 0 {
        let shift = oparg as i32;
        if !(0..u32::BITS as i32).contains(&shift) {
            return Err("einval");
        }
        oparg = 1u32 << shift;
    }
    let operation = match op_kind {
        FUTEX_OP_SET => UserAtomicU32Op::Swap(oparg),
        FUTEX_OP_ADD => UserAtomicU32Op::Add(oparg),
        FUTEX_OP_OR => UserAtomicU32Op::Or(oparg),
        FUTEX_OP_ANDN => UserAtomicU32Op::And(!oparg),
        FUTEX_OP_XOR => UserAtomicU32Op::Xor(oparg),
        _ => return Err("enosys"),
    };
    task.process
        .addr_space
        .lock()
        .unwrap()
        .atomic_user_u32(uaddr2, operation, &kernel.pool)
}

// AGENT: decode the signed comparison half of FUTEX_WAKE_OP separately from
// the atomic mutation so the returned old value is compared exactly once.
fn futex_wake_op_cmp(old: u32, encoded: u32) -> Result<bool, &'static str> {
    const FUTEX_OP_CMP_EQ: u32 = 0;
    const FUTEX_OP_CMP_NE: u32 = 1;
    const FUTEX_OP_CMP_LT: u32 = 2;
    const FUTEX_OP_CMP_LE: u32 = 3;
    const FUTEX_OP_CMP_GT: u32 = 4;
    const FUTEX_OP_CMP_GE: u32 = 5;

    let cmp = (encoded >> 24) & 0xF;
    let cmparg = sign_extend_12(encoded & 0xFFF);
    let old = old as i32;
    match cmp {
        FUTEX_OP_CMP_EQ => Ok(old == cmparg),
        FUTEX_OP_CMP_NE => Ok(old != cmparg),
        FUTEX_OP_CMP_LT => Ok(old < cmparg),
        FUTEX_OP_CMP_LE => Ok(old <= cmparg),
        FUTEX_OP_CMP_GT => Ok(old > cmparg),
        FUTEX_OP_CMP_GE => Ok(old >= cmparg),
        _ => Err("enosys"),
    }
}

// AGENT: sign-extend one encoded futex operand or comparison argument from its
// 12-bit UAPI field into the signed 32-bit arithmetic domain.
fn sign_extend_12(value: u32) -> i32 {
    let value = (value & 0xFFF) as i32;
    if value & 0x800 != 0 {
        value | !0xFFF
    } else {
        value
    }
}
