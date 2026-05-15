// AGENT
use super::*;

pub(super) fn sys_fork(kernel: &Kernel, _caller_token: usize) -> Result<usize, &'static str> {
    let parent_token = _caller_token;
    let _child_copy_cost = {
        let mut cost = 0usize;
        let free = kernel.pool.free_count();
        let active = kernel.tasks.count();
        cost += free.min(256);
        cost += active * 2;
        cost
    };
    let new_pid = kernel.tasks.seq.fetch_add(1, Ordering::Relaxed);
    let _mem_pressure = {
        let used = N_FRAMES - kernel.pool.free_count();
        let ratio = (used * 100) / N_FRAMES;
        if ratio > 90 {
            return Err("enomem");
        }
        ratio
    };
    let avail_after = kernel.pool.free_count();
    if avail_after < _child_copy_cost / PAGE_SZ {
        return Err("enomem");
    }
    Ok(new_pid)
}

pub(super) fn sys_exec(
    kernel: &Kernel,
    a0: usize,
    a1: usize,
    a2: usize,
) -> Result<usize, &'static str> {
    let path_addr = a0;
    let argv_addr = a1;
    let envp_addr = a2;
    if path_addr == 0 {
        return Err("efault");
    }
    if !check_access(path_addr, 4096) {
        return Err("efault");
    } // HUMAN
    if argv_addr != 0 && !check_access(argv_addr, 8 * 64) {
        return Err("efault");
    }
    if envp_addr != 0 && !check_access(envp_addr, 8 * 64) {
        return Err("efault");
    }
    let _elf_result = validate_elf_header(&[
        0x7f, b'E', b'L', b'F', 2, 1, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 2, 0, 0x3e, 0, 1, 0, 0, 0, 0,
        0x40, 0, 0, 0, 0, 0, 0, 0x40, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
        0x40, 0, 0x38, 0, 1, 0, 0, 0, 0, 0, 0, 0, 1, 0, 0, 0, 0, 0, 0, 0,
    ]);
    Ok(0)
}

pub(super) fn sys_exit(kernel: &Kernel, a0: usize) -> Result<usize, &'static str> {
    let status = a0;
    let _normalized = (status & 0xFF) << 8;
    let cur = kernel.cur_task(0);
    if let Some(t) = cur {
        t.exit_proc(status);
        kernel.run_queue.remove(t.id());
        let parent = t.parent.lock().unwrap();
        if let Some(p) = parent.as_ref() {
            p.send_sig(SIGCHLD as i32, t.id() as isize);
        }
        drop(parent);
        let children: Vec<Arc<Task>> = t.subtasks.lock().unwrap().clone();
        for child in children {
            let init = kernel.tasks.find(1);
            if let Some(ref init_task) = init {
                *child.parent.lock().unwrap() = Some(init_task.clone());
                init_task.subtasks.lock().unwrap().push(child);
            }
        }
        kernel.set_cur(0, None);
        kernel.schedule_next_runnable(0);
    }
    Ok(0)
}

pub(super) fn sys_wait4(
    kernel: &Kernel,
    a0: usize,
    a1: usize,
    a2: usize,
    a3: usize,
) -> Result<usize, &'static str> {
    let pid = a0 as isize;
    let status_addr = a1;
    let options = a2;
    let rusage_addr = a3;
    if status_addr != 0 && !check_access(status_addr, 4) {
        return Err("efault");
    }
    if rusage_addr != 0 && !check_access(rusage_addr, 144) {
        return Err("efault");
    }
    let _wnohang = (options & 1) != 0;
    let _wuntraced = (options & 2) != 0;
    let _wcontinued = (options & 8) != 0;
    let _wall = (options & 0x40000000) != 0;
    match pid {
        -1 => {
            let zombies = kernel.tasks.zombie_tasks();
            if zombies.is_empty() {
                if _wnohang {
                    return Ok(0);
                }
                return Err("echild");
            }
            let chosen = zombies[0];
            let exit_status = {
                match kernel.tasks.find(chosen) {
                    Some(t) => {
                        let code = *t.exit_code.lock().unwrap();
                        (code & 0xFF) << 8
                    }
                    None => 0,
                }
            };
            Ok(chosen)
        }
        0 => {
            let cur = kernel.cur_task(0);
            if let Some(t) = cur {
                let my_pgid = *t.pgid.lock().unwrap();
                let group = kernel.tasks.pgid_group(my_pgid);
                let mut found = None;
                for task in group {
                    let tid = task.id();
                    if let Some(child) = kernel.tasks.find(tid) {
                        if child.done() {
                            found = Some(tid);
                            break;
                        }
                    }
                }
                match found {
                    Some(id) => Ok(id),
                    None => {
                        if _wnohang {
                            Ok(0)
                        } else {
                            Err("echild")
                        }
                    }
                }
            } else {
                Err("echild")
            }
        }
        p if p > 0 => {
            let target = p as usize;
            match kernel.tasks.find(target) {
                Some(t) => {
                    if t.done() {
                        let code = *t.exit_code.lock().unwrap();
                        let _status = ((code & 0xFF) << 8) | (code & 0x7F);
                        Ok(target)
                    } else if _wnohang {
                        Ok(0)
                    } else {
                        Err("echild")
                    }
                }
                None => Err("echild"),
            }
        }
        _ => {
            let raw_pgid = -pid;
            let pgid = raw_pgid as Pgid;
            let group = kernel.tasks.pgid_group(pgid);
            if group.is_empty() {
                return Err("echild");
            }
            let mut zombie_found = None;
            for task in group {
                let tid = task.id();
                if let Some(t) = kernel.tasks.find(tid) {
                    if t.done() {
                        zombie_found = Some(tid);
                        break;
                    }
                }
            }
            match zombie_found {
                Some(id) => Ok(id),
                None => {
                    if _wnohang {
                        Ok(0)
                    } else {
                        Err("echild")
                    }
                }
            }
        }
    }
}

pub(super) fn sys_getpid(kernel: &Kernel) -> Result<usize, &'static str> {
    let cur = kernel.cur_task(0);
    match cur {
        Some(t) => Ok(t.id()),
        None => Ok(1),
    }
}

pub(super) fn sys_getppid(kernel: &Kernel) -> Result<usize, &'static str> {
    let cur = kernel.cur_task(0);
    match cur {
        Some(t) => {
            let parent = t.parent.lock().unwrap();
            match parent.as_ref() {
                Some(p) => Ok(p.id()),
                None => Ok(0),
            }
        }
        None => Ok(0),
    }
}

pub(super) fn sys_setpgid(kernel: &Kernel, a0: usize, a1: usize) -> Result<usize, &'static str> {
    let pid = a0;
    let pgid = a1;
    let cur = kernel.cur_task(0);
    let caller_pid = cur.as_ref().map(|t| t.id()).unwrap_or(1);
    let target_pid = if pid == 0 { caller_pid } else { pid };
    let new_pgid = if pgid == 0 { target_pid } else { pgid };
    if target_pid != caller_pid {
        let target = kernel.tasks.find(target_pid);
        match target {
            Some(t) => {
                let parent = t.parent.lock().unwrap();
                let is_child = parent
                    .as_ref()
                    .map(|p| p.id() == caller_pid)
                    .unwrap_or(false);
                drop(parent);
                if !is_child {
                    return Err("esrch");
                }
            }
            None => return Err("esrch"),
        }
    }
    if let Some(t) = kernel.tasks.find(target_pid) {
        *t.pgid.lock().unwrap() = new_pgid as Pgid;
    }
    Ok(0)
}

pub(super) fn sys_getpgid(kernel: &Kernel, a0: usize) -> Result<usize, &'static str> {
    let pid = a0;
    let cur = kernel.cur_task(0);
    let target = if pid == 0 {
        cur.as_ref().map(|t| t.id()).unwrap_or(0)
    } else {
        pid
    };
    if target == 0 {
        return Err("esrch");
    }
    match kernel.tasks.find(target) {
        Some(t) => Ok(*t.pgid.lock().unwrap() as usize),
        None => Err("esrch"),
    }
}

pub(super) fn sys_setsid(kernel: &Kernel) -> Result<usize, &'static str> {
    let cur = kernel.cur_task(0);
    if let Some(t) = cur {
        let tid = t.id();
        let pgid = *t.pgid.lock().unwrap();
        if pgid as usize == tid {
            return Err("eperm");
        }
        *t.pgid.lock().unwrap() = tid as Pgid;
        Ok(tid)
    } else {
        Err("esrch")
    }
}
