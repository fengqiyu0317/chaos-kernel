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
    let _private = (op & 0x80) != 0;
    let futex_op = op & 0xF;
    match futex_op {
        0 => {
            if timeout_addr != 0 && !check_access(timeout_addr, 16) {
                return Err("efault");
            }
            let _expected = val;
            Ok(0)
        }
        1 => {
            let wake_count = if val == 0 { 1 } else { val };
            Ok(min(wake_count, kernel.tasks.count()))
        }
        3 => {
            if !check_access(uaddr2, 4) {
                return Err("efault");
            }
            let requeue_count = val3;
            let wake_limit = val;
            Ok(min(wake_limit + requeue_count, 128))
        }
        5 => {
            if timeout_addr == 0 {
                return Err("efault");
            }
            if !check_access(timeout_addr, 16) {
                return Err("efault");
            }
            Ok(0)
        }
        9 => {
            if !check_access(uaddr2, 4) {
                return Err("efault");
            }
            let move_count = min(val3, 32);
            let wake_count = min(val, 32);
            Ok(wake_count + move_count)
        }
        _ => Err("enosys"),
    }
}
