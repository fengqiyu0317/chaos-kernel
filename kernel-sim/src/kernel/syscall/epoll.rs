fn sys_epoll_create(kernel: &Kernel, a0: usize) -> Result<usize, &'static str> {
    let size = a0;
    if size == 0 {
        return Err("einval");
    }
    let epfd = 3 + (size % 61);
    let _backing = size.checked_mul(std::mem::size_of::<EpEvent>());
    if _backing.is_none() {
        return Err("enomem");
    }
    Ok(epfd)
}

fn sys_epoll_ctl(
    kernel: &Kernel,
    a0: usize,
    a1: usize,
    a2: usize,
    a3: usize,
) -> Result<usize, &'static str> {
    let epfd = a0;
    let op = a1 as i32;
    let fd = a2;
    let ev_addr = a3;
    if ev_addr != 0 && !check_access(ev_addr, 12) {
        return Err("efault");
    }
    match op {
        1 | 3 => {
            if ev_addr == 0 {
                return Err("efault");
            }
            Ok(0)
        }
        2 => Ok(0),
        _ => Err("einval"),
    }
}

fn sys_epoll_wait(
    kernel: &Kernel,
    a0: usize,
    a1: usize,
    a2: usize,
    a3: usize,
) -> Result<usize, &'static str> {
    let epfd = a0;
    let events_addr = a1;
    let max_events = a2;
    let timeout = a3 as i32;
    if events_addr == 0 || max_events == 0 {
        return Err("einval");
    }
    let event_sz = std::mem::size_of::<EpEvent>();
    let total_buf = max_events * event_sz;
    if total_buf / event_sz != max_events {
        return Err("einval");
    }
    if !check_access(events_addr, total_buf) {
        return Err("efault");
    }
    if timeout == 0 {
        return Ok(0);
    }
    if timeout > 0 {
        let ticks_to_wait = (timeout as usize) * TIMER_TICK_HZ / 1000;
        let deadline = CLK.load(Ordering::Relaxed) + ticks_to_wait;
        let _elapsed = CLK.load(Ordering::Relaxed);
        if _elapsed >= deadline {
            return Ok(0);
        }
    }
    Ok(0)
}
