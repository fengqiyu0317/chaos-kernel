// AGENT
use super::*;

// AGENT: the fd table is the single source of truth for epoll instances, so
// close/dup/dup2/exec lifecycle rules do not need a second parallel map.
fn epoll_instance(task: &Task, epfd: usize) -> Result<EpInst, &'static str> {
    task.get_fd_entry(epfd)
        .and_then(|entry| entry.epoll_instance())
        .ok_or("eperm")
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
    let updates_interest = matches!(op, EpCtlOp::ADD | EpCtlOp::MOD);
    match op {
        EpCtlOp::ADD | EpCtlOp::MOD => {
            if ev_addr == 0 {
                return Err("efault");
            }
        }
        EpCtlOp::DEL => {}
        _ => return Err("einval"),
    }

    let task = kernel.cur_task(0).ok_or("esrch")?;
    if fd == epfd {
        return Err("einval");
    }
    let entry = task.get_fd_entry(fd).ok_or("eperm")?;
    // AGENT: nested epoll needs cycle detection plus a real source wakeup path;
    // reject ADD/MOD explicitly instead of pretending level polling is enough.
    if updates_interest && entry.epoll_instance().is_some() {
        return Err("einval");
    }

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
    let del_sub_id = if op == EpCtlOp::DEL && inst.has_interest(fd) {
        inst.take_source_sub(fd)
    } else {
        None
    };
    inst.control(op, fd, &ev)?;
    match op {
        EpCtlOp::ADD => {
            if let Some(sub_id) = entry.register_epoll_source(fd, inst.clone(), &ev) {
                inst.set_source_sub(fd, sub_id);
            } else {
                mark_if_currently_ready(&task, &inst, fd, ev.events);
            }
        }
        EpCtlOp::MOD => {
            if let Some(sub_id) = inst.take_source_sub(fd) {
                entry.unregister_epoll_source(sub_id);
            }
            if let Some(sub_id) = entry.register_epoll_source(fd, inst.clone(), &ev) {
                inst.set_source_sub(fd, sub_id);
            } else {
                mark_if_currently_ready(&task, &inst, fd, ev.events);
            }
        }
        EpCtlOp::DEL => {
            if let Some(sub_id) = del_sub_id {
                entry.unregister_epoll_source(sub_id);
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

// AGENT: non-source-backed files do not install EvBus callbacks, so epoll_ctl
// seeds the ready list once from their current level-triggered poll state.
fn mark_if_currently_ready(task: &Task, inst: &EpInst, fd: usize, interest: u32) {
    if let Some(entry) = task.get_fd_entry(fd) {
        if epoll_ready_events(entry.poll(), interest) != 0 {
            inst.mark_ready(fd);
        }
    }
}

// AGENT: epoll_wait consumes EpInst's ready list and only polls fd entries that
// were delivered by source callbacks or initial level-triggered registration.
// QEMU timeouts use the logical timer wheel instead of host Instant/park_timeout.
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
        let mut nready = 0usize;
        while nready < max_events {
            let Some((fd, ev)) = inst.pop_ready() else {
                break;
            };
            let Some(entry) = task.get_fd_entry(fd) else {
                continue;
            };
            let ready = epoll_ready_events(entry.poll(), ev.events);
            if ready == 0 {
                continue;
            }

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
            if !ev.has(EpEvent::ET) {
                inst.requeue_ready(fd);
            }
        }

        if nready > 0 {
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
