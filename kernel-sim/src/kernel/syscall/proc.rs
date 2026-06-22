// AGENT
use super::*;

pub(super) fn sys_fork(kernel: &Kernel, _caller_token: usize) -> Result<usize, &'static str> {
    let parent_id = kernel.cur_task(0).map(|task| task.id()).ok_or("esrch")?;
    // AGENT: keep syscall fork as a thin wrapper around the real fork path.
    kernel.do_fork(parent_id)
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
    let task = kernel.cur_task(0).ok_or("esrch")?;
    let task_id = task.id();
    let (path, args, envs) = {
        let addr_space = task.addr_space.lock().unwrap();
        let path = read_user_c_string(&addr_space, path_addr, 4096, "enametoolong")?;
        let args = read_user_string_array(&addr_space, argv_addr, 64, 4096)?;
        let envs = read_user_string_array(&addr_space, envp_addr, 64, 4096)?;
        (path, args, envs)
    };
    kernel.do_exec(task_id, &path, args, envs)?;
    Ok(0)
}

fn read_user_c_string(
    addr_space: &AddrSpace,
    addr: usize,
    max_len: usize,
    too_long: &'static str,
) -> Result<String, &'static str> {
    if addr == 0 {
        return Err("efault");
    }
    let mut bytes = Vec::new();
    for offset in 0..max_len {
        let cur = addr.checked_add(offset).ok_or("efault")?;
        let mut byte = [0u8; 1];
        addr_space.read_user_bytes(cur, &mut byte)?;
        if byte[0] == 0 {
            return String::from_utf8(bytes).map_err(|_| "einval");
        }
        bytes.push(byte[0]);
    }
    Err(too_long)
}

fn read_user_string_array(
    addr_space: &AddrSpace,
    array_addr: usize,
    max_items: usize,
    max_string_len: usize,
) -> Result<Vec<String>, &'static str> {
    if array_addr == 0 {
        return Ok(Vec::new());
    }
    let mut out = Vec::new();
    let word = std::mem::size_of::<usize>();
    for idx in 0..max_items {
        let ptr_addr = array_addr
            .checked_add(idx.checked_mul(word).ok_or("efault")?)
            .ok_or("efault")?;
        let ptr = addr_space.read_user_usize(ptr_addr)?;
        if ptr == 0 {
            return Ok(out);
        }
        out.push(read_user_c_string(
            addr_space,
            ptr,
            max_string_len,
            "e2big",
        )?);
    }
    Err("e2big")
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
            kernel.send_signal_to_task(p, SIGCHLD as i32, t.id() as isize);
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
