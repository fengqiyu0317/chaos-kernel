// AGENT
use super::*;

pub(super) fn sys_epoll_create(kernel: &Kernel, a0: usize) -> Result<usize, &'static str> {
    let size = a0;
    if size == 0 {
        return Err("einval");
    }
    let _backing = size.checked_mul(std::mem::size_of::<EpEvent>());
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
    let event_sz = std::mem::size_of::<EpEvent>();
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
    if task.get_file(fd).is_none() {
        return Err("eperm");
    }

    let ev = if ev_addr == 0 {
        EpEvent {
            events: 0,
            data: EpData { ptr: 0 },
        }
    } else {
        // AGENT: EpEvent is an explicit C-layout kernel ABI struct.
        unsafe { std::ptr::read_unaligned(ev_addr as *const EpEvent) }
    };

    // AGENT: mutate the registered epoll instance in place under Task::ep_inst.
    task.with_ep_mut(epfd, |inst| inst.control(op, fd, &ev))?;
    Ok(0)
}

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
    let event_sz = std::mem::size_of::<EpEvent>();
    let total_buf = max_events.checked_mul(event_sz).ok_or("einval")?;
    if !check_access(events_addr, total_buf) {
        return Err("efault");
    }

    let task = kernel.cur_task(0).ok_or("esrch")?;
    let deadline = if timeout > 0 {
        Some(std::time::Instant::now() + Duration::from_millis(timeout as u64))
    } else {
        None
    };

    loop {
        let registrations: Vec<(usize, EpEvent)> = {
            let ep = task.ep_inst.lock().unwrap();
            let inst = ep.get(&epfd).ok_or("eperm")?;
            inst.events
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
                std::ptr::write_unaligned(dst, out);
            }
            nready += 1;
        }

        {
            let ep = task.ep_inst.lock().unwrap();
            let inst = ep.get(&epfd).ok_or("eperm")?;
            let mut ready = inst.ready.lock().unwrap();
            *ready = ready_fds;
        }

        if nready > 0 {
            return Ok(nready);
        }
        if timeout == 0 {
            return Ok(0);
        }
        if let Some(deadline) = deadline {
            if std::time::Instant::now() >= deadline {
                return Ok(0);
            }
        }
        thread::yield_now();
    }
}
