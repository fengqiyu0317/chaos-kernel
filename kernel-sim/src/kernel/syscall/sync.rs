// AGENT
use super::*;

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
    let op = a1;
    let val = a2;
    let timeout_addr = a3;
    let uaddr2 = a4;
    let val3 = a5;
    if !check_access(uaddr, 4) {
        return Err("efault");
    }
    if uaddr % std::mem::size_of::<u32>() != 0 {
        return Err("einval");
    }
    let _private = (op & 0x80) != 0;
    let futex_op = op & 0xF;
    match futex_op {
        0 => {
            if timeout_addr != 0 && !check_access(timeout_addr, 16) {
                return Err("efault");
            }
            let timeout = if timeout_addr == 0 {
                None
            } else {
                Some(read_futex_timeout(timeout_addr)?)
            };
            let current = kernel.cur_task(0).ok_or("esrch")?;
            let futex = current.get_futex();
            let word = unsafe { &*(uaddr as *const AtomicU32) };
            match futex.wait(uaddr, val as u32, word, timeout) {
                Ok(()) => Ok(0),
                Err("changed") => Err("eagain"),
                Err(e) => Err(e),
            }
        }
        1 => {
            let wake_count = val;
            let current = kernel.cur_task(0).ok_or("esrch")?;
            Ok(current.get_futex().wake(uaddr, wake_count))
        }
        3 => {
            if !check_access(uaddr2, 4) {
                return Err("efault");
            }
            if uaddr2 % std::mem::size_of::<u32>() != 0 {
                return Err("einval");
            }
            let requeue_count = timeout_addr;
            let wake_limit = val;
            let current = kernel.cur_task(0).ok_or("esrch")?;
            Ok(current
                .get_futex()
                .requeue(uaddr, uaddr2, wake_limit, requeue_count))
        }
        5 => {
            if !check_access(uaddr2, 4) {
                return Err("efault");
            }
            if uaddr2 % std::mem::size_of::<u32>() != 0 {
                return Err("einval");
            }
            let val2 = timeout_addr;
            let current = kernel.cur_task(0).ok_or("esrch")?;
            let futex = current.get_futex();
            futex.wake_op(
                uaddr,
                val,
                uaddr2,
                val2,
                || futex_wake_op_apply(uaddr2, val3),
                |old| futex_wake_op_cmp(old, val3),
            )
        }
        9 => {
            if !check_access(uaddr2, 4) {
                return Err("efault");
            }
            if uaddr2 % std::mem::size_of::<u32>() != 0 {
                return Err("einval");
            }
            let current = kernel.cur_task(0).ok_or("esrch")?;
            let futex = current.get_futex();
            let word = unsafe { &*(uaddr as *const AtomicU32) };
            match futex.cmp_requeue(uaddr, uaddr2, val, timeout_addr, word, val3 as u32) {
                Ok(n) => Ok(n),
                Err("changed") => Err("eagain"),
                Err(e) => Err(e),
            }
        }
        _ => Err("enosys"),
    }
}

fn read_futex_timeout(timeout_addr: usize) -> Result<Duration, &'static str> {
    let tv_sec = unsafe { std::ptr::read_unaligned(timeout_addr as *const usize) };
    let tv_nsec = unsafe {
        std::ptr::read_unaligned((timeout_addr + std::mem::size_of::<usize>()) as *const usize)
    };
    if tv_nsec >= 1_000_000_000 {
        return Err("einval");
    }
    let secs = u64::try_from(tv_sec).map_err(|_| "einval")?;
    let nanos = u32::try_from(tv_nsec).map_err(|_| "einval")?;
    Ok(Duration::new(secs, nanos))
}

fn futex_wake_op_apply(uaddr2: usize, encoded: usize) -> Result<u32, &'static str> {
    const FUTEX_OP_SET: usize = 0;
    const FUTEX_OP_ADD: usize = 1;
    const FUTEX_OP_OR: usize = 2;
    const FUTEX_OP_ANDN: usize = 3;
    const FUTEX_OP_XOR: usize = 4;
    const FUTEX_OP_OPARG_SHIFT: usize = 8;

    let op = (encoded >> 28) & 0xF;
    let op_kind = op & 0x7;
    let mut oparg = sign_extend_12((encoded >> 12) & 0xFFF);
    if op & FUTEX_OP_OPARG_SHIFT != 0 {
        if !(0..u32::BITS as i32).contains(&oparg) {
            return Err("einval");
        }
        oparg = 1i32 << oparg;
    }
    let word = unsafe { &*(uaddr2 as *const AtomicU32) };
    word.fetch_update(Ordering::SeqCst, Ordering::SeqCst, |old| {
        let new = match op_kind {
            FUTEX_OP_SET => oparg as u32,
            FUTEX_OP_ADD => old.wrapping_add(oparg as u32),
            FUTEX_OP_OR => old | oparg as u32,
            FUTEX_OP_ANDN => old & !(oparg as u32),
            FUTEX_OP_XOR => old ^ oparg as u32,
            _ => return None,
        };
        Some(new)
    })
    .map_err(|_| "einval")
}

fn futex_wake_op_cmp(old: u32, encoded: usize) -> Result<bool, &'static str> {
    const FUTEX_OP_CMP_EQ: usize = 0;
    const FUTEX_OP_CMP_NE: usize = 1;
    const FUTEX_OP_CMP_LT: usize = 2;
    const FUTEX_OP_CMP_LE: usize = 3;
    const FUTEX_OP_CMP_GT: usize = 4;
    const FUTEX_OP_CMP_GE: usize = 5;

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
        _ => Err("einval"),
    }
}

fn sign_extend_12(value: usize) -> i32 {
    let value = (value & 0xFFF) as i32;
    if value & 0x800 != 0 {
        value | !0xFFF
    } else {
        value
    }
}
