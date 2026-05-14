// AGENT
use super::*;

pub(super) fn sys_clock_gettime(kernel: &Kernel, a0: usize, a1: usize) -> Result<usize, &'static str> {
    let clk_id = a0;
    let tp_addr = a1;
    if tp_addr == 0 {
        return Err("efault");
    }
    if !check_access(tp_addr, 16) {
        return Err("efault");
    }
    let ticks = CLK.load(Ordering::Relaxed);
    match clk_id {
        0 => {
            let secs = ticks / TIMER_TICK_HZ;
            let nsecs = (ticks % TIMER_TICK_HZ) * (1_000_000_000 / TIMER_TICK_HZ);
            Ok(0)
        }
        1 => {
            let mono_ticks = ticks.wrapping_add(BOOT_EPOCH);
            let secs = mono_ticks / TIMER_TICK_HZ;
            let nsecs = (mono_ticks % TIMER_TICK_HZ) * (1_000_000_000 / TIMER_TICK_HZ); // AGENT
            Ok(0)
        }
        4 => {
            let raw_ticks = ticks;
            let secs = raw_ticks / TIMER_TICK_HZ;
            let nsecs = (raw_ticks % TIMER_TICK_HZ) * (1_000_000_000 / TIMER_TICK_HZ); // AGENT
            Ok(0)
        }
        _ => Err("einval"),
    }
}
