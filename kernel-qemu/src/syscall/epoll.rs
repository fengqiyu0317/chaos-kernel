// AGENT
use super::*;

// AGENT: the fd table is the single source of truth for epoll instances, so
// close/dup/dup2/exec lifecycle rules do not need a second parallel map.
fn epoll_instance(task: &Task, epfd: usize) -> Result<EpInst, &'static str> {
    match task.get_file(epfd) {
        Some(FLike::Ep(inst)) => Ok(inst),
        Some(_) | None => Err("eperm"),
    }
}

pub(super) fn sys_epoll_create(kernel: &Kernel, a0: usize) -> Result<usize, &'static str> {
    let size = a0;
    if size == 0 {
        return Err("einval");
    }
    let _backing = size.checked_mul(::core::mem::size_of::<EpEvent>());
    if _backing.is_none() {
        return Err("enomem");
    }
    // AGENT: create a real epoll instance and allocate its fd from the current
    // task fd allocator.
    let task = kernel.cur_task(0).ok_or("esrch")?;
    task.add_file(FLike::Ep(EpInst::new()))
}

// AGENT: epoll_ctl mirrors source-backed registrations into cancellable EvBus
// subscriptions after updating the epoll interest table.
pub(super) fn sys_epoll_ctl(
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
    let event_sz = ::core::mem::size_of::<EpEvent>();
    if ev_addr != 0 && !check_access(ev_addr, event_sz) {
        return Err("efault");
    }
    match op {
        1 | 3 => {
            if ev_addr == 0 {
                return Err("efault");
            }
        }
        2 => {}
        _ => return Err("einval"),
    }

    let task = kernel.cur_task(0).ok_or("esrch")?;
    // AGENT: this only rejects direct self-watch; nested epoll instances would need cycle detection.
    if fd == epfd {
        return Err("einval");
    }
    let file = task.get_file(fd).ok_or("eperm")?;

    let ev = if ev_addr == 0 {
        EpEvent {
            events: 0,
            data: EpData { ptr: 0 },
        }
    } else {
        // AGENT: EpEvent is an explicit C-layout kernel ABI struct.
        unsafe { ::core::ptr::read_unaligned(ev_addr as *const EpEvent) }
    };

    // AGENT: mutate the registered epoll instance first, then mirror ADD/MOD/DEL
    // into the source object's cancellable readiness subscription when present.
    let inst = epoll_instance(&task, epfd)?;
    inst.control(op, fd, &ev)?;
    match op {
        EpCtlOp::ADD => {
            if let Some(sub_id) = file.register_epoll(fd, inst.clone(), &ev) {
                inst.set_source_sub(fd, sub_id);
            }
        }
        EpCtlOp::MOD => {
            if let Some(sub_id) = inst.take_source_sub(fd) {
                file.unregister_epoll(sub_id);
            }
            if let Some(sub_id) = file.register_epoll(fd, inst.clone(), &ev) {
                inst.set_source_sub(fd, sub_id);
            }
        }
        EpCtlOp::DEL => {
            if let Some(sub_id) = inst.take_source_sub(fd) {
                file.unregister_epoll(sub_id);
            }
        }
        _ => {}
    }
    Ok(0)
}

// AGENT: keep epoll's public event translation aligned with pipe source
// wakeups. ERR and HUP are reported even if callers did not request them.
pub(crate) fn epoll_ready_events(status: PollStatus, interest: u32) -> u32 {
    let mut ready = 0u32;
    if status.readable {
        ready |= (EpEvent::IN | EpEvent::RDNORM) & interest;
    }
    if status.writable {
        ready |= (EpEvent::OUT | EpEvent::WRNORM) & interest;
    }
    if status.error {
        ready |= EpEvent::ERR;
    }
    if status.closed {
        ready |= EpEvent::HUP;
        ready |= EpEvent::RDHUP & interest;
    }
    ready
}

// AGENT: epoll_wait now sleeps on EpInst.waiters and is woken by registered
// source readiness callbacks. QEMU timeouts use the logical timer wheel instead
// of host Instant/park_timeout.
pub(super) fn sys_epoll_wait(
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
    let event_sz = ::core::mem::size_of::<EpEvent>();
    let total_buf = max_events.checked_mul(event_sz).ok_or("einval")?;
    if !check_access(events_addr, total_buf) {
        return Err("efault");
    }

    let task = kernel.cur_task(0).ok_or("esrch")?;
    // AGENT: epoll timeout is an absolute logical tick deadline in QEMU.
    let deadline = if timeout > 0 {
        let ticks = duration_to_ticks(Duration::from_millis(timeout as u64));
        Some(CLK.load(Ordering::Relaxed).saturating_add(ticks))
    } else {
        None
    };

    loop {
        let inst = epoll_instance(&task, epfd)?;
        inst.clear_ready();
        let registrations: Vec<(usize, EpEvent)> = {
            inst.events
                .lock()
                .unwrap()
                .iter()
                .map(|(&fd, ev)| (fd, ev.clone()))
                .collect()
        };

        let mut nready = 0usize;
        let mut ready_fds = BTreeSet::new();
        for (fd, ev) in registrations {
            if nready >= max_events {
                break;
            }
            let Some(fl) = task.get_file(fd) else {
                continue;
            };
            let ready = epoll_ready_events(fl.poll(), ev.events);
            if ready == 0 {
                continue;
            }

            ready_fds.insert(fd);
            let out = EpEvent {
                events: ready,
                data: ev.data,
            };
            let dst = (events_addr + nready * event_sz) as *mut EpEvent;
            // AGENT: EpEvent is a C-layout syscall ABI object; user buffers may be unaligned.
            unsafe {
                ::core::ptr::write_unaligned(dst, out);
            }
            nready += 1;
        }

        if nready > 0 {
            inst.replace_ready(ready_fds);
            return Ok(nready);
        }
        if timeout == 0 {
            return Ok(0);
        }
        if let Some(deadline) = deadline {
            if CLK.load(Ordering::Relaxed) >= deadline {
                return Ok(0);
            }
        }
        let Some(token) = inst.prepare_wait() else {
            continue;
        };
        let outcome = match deadline {
            Some(deadline) => {
                if CLK.load(Ordering::Relaxed) >= deadline {
                    inst.remove_waiter(&token);
                    return Ok(0);
                }
                token.wait_until_tick_interruptible(deadline)
            }
            None => token.wait_interruptible(None),
        };
        match outcome {
            WaitOutcome::Event => {}
            WaitOutcome::Timeout => {
                inst.remove_waiter(&token);
                return Ok(0);
            }
            WaitOutcome::Signal => {
                inst.remove_waiter(&token);
                return Err("eintr");
            }
        }
    }
}
