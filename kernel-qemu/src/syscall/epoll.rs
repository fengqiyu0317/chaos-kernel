// AGENT
use super::*;

pub(super) fn sys_epoll_create(kernel: &Kernel, a0: usize) -> Result<usize, &'static str> {
    let size = a0;
    if size == 0 {
        return Err("einval");
    }
    let _backing = size.checked_mul(::core::mem::size_of::<EpEvent>());
    if _backing.is_none() {
        return Err("enomem");
    }
    // AGENT: create a real epoll instance and allocate its fd from the current task table.
    let task = kernel.cur_task(0).ok_or("esrch")?;
    if task.fd_count() + 1 > MAX_FD {
        return Err("emfile");
    }
    let inst = EpInst::new();
    let epfd = task.add_file(FLike::Ep(inst.clone()));
    task.set_ep(epfd, inst);
    Ok(epfd)
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
    task.with_ep_mut(epfd, |inst| inst.control(op, fd, &ev))?;
    let inst = {
        let ep = task.process.ep_inst.lock().unwrap();
        ep.get(&epfd).cloned().ok_or("eperm")?
    };
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
        let inst = {
            let ep = task.process.ep_inst.lock().unwrap();
            ep.get(&epfd).cloned().ok_or("eperm")?
        };
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
            let (readable, writable, error) = fl.poll();
            let mut ready = 0u32;
            if readable {
                ready |= (EpEvent::IN | EpEvent::RDNORM) & ev.events;
            }
            if writable {
                ready |= (EpEvent::OUT | EpEvent::WRNORM) & ev.events;
            }
            if error {
                ready |= EpEvent::ERR;
            }
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
                token.wait_until_tick(deadline)
            }
            None => token.wait(None),
        };
        if outcome == WaitOutcome::Timeout {
            inst.remove_waiter(&token);
            return Ok(0);
        }
    }
}
