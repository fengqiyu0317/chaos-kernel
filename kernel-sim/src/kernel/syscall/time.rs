// AGENT
use super::*;

#[repr(C)]
#[derive(Clone, Copy)]
struct ClockTimeSpec {
    tv_sec: usize,
    tv_nsec: usize,
}

fn ticks_to_timespec(ticks: usize) -> ClockTimeSpec {
    // AGENT: CLK is a 100Hz logical kernel clock, so convert ticks to timespec here.
    ClockTimeSpec {
        tv_sec: ticks / TIMER_TICK_HZ,
        tv_nsec: (ticks % TIMER_TICK_HZ) * (1_000_000_000 / TIMER_TICK_HZ),
    }
}

pub(super) fn sys_clock_gettime(kernel: &Kernel, a0: usize, a1: usize) -> Result<usize, &'static str> {
    let _ = kernel;
    let clk_id = a0;
    let tp_addr = a1;
    if tp_addr == 0 {
        return Err("efault");
    }
    if !check_access_rw(tp_addr, std::mem::size_of::<ClockTimeSpec>(), true) {
        return Err("efault");
    }
    let ticks = CLK.load(Ordering::Relaxed);
    let out = match clk_id {
        0 => {
            // AGENT: CLOCK_REALTIME is wall time; BOOT_EPOCH is seconds, not ticks.
            let mut realtime = ticks_to_timespec(ticks);
            realtime.tv_sec = realtime.tv_sec.wrapping_add(BOOT_EPOCH);
            realtime
        }
        // AGENT: CLOCK_MONOTONIC and CLOCK_MONOTONIC_RAW both expose uptime in this simulator.
        1 | 4 => ticks_to_timespec(ticks),
        _ => return Err("einval"),
    };
    // AGENT: timespec is a syscall ABI object; user buffers may be unaligned.
    unsafe { std::ptr::write_unaligned(tp_addr as *mut ClockTimeSpec, out); }
    Ok(0)
}
