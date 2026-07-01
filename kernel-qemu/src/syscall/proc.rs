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
        let addr_space = task.process.addr_space.lock().unwrap();
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
    let word = mem::size_of::<usize>();
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

pub(super) fn sys_exit(kernel: &Kernel, a0: usize) -> Result<SyscallOutcome, &'static str> {
    kernel.do_exit_current(0, a0)?;
    Ok(SyscallOutcome::NoReturn)
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
    if status_addr != 0 && !check_access_rw(status_addr, 4, true) {
        return Err("efault");
    }
    if rusage_addr != 0 && !check_access_rw(rusage_addr, 144, true) {
        return Err("efault");
    }
    let current = kernel.cur_task(0).ok_or("echild")?;
    let (pid, wait_status) = kernel.do_wait(current.id(), pid, options)?;
    if pid != 0 && status_addr != 0 {
        let status = (wait_status as u32).to_ne_bytes();
        current
            .process
            .addr_space
            .lock()
            .unwrap()
            .write_user_bytes(status_addr, &status, &kernel.pool)?;
    }
    Ok(pid)
}

pub(super) fn sys_getpid(kernel: &Kernel) -> Result<usize, &'static str> {
    let cur = kernel.cur_task(0);
    match cur {
        Some(t) => Ok(t.process_pid()),
        None => Ok(1),
    }
}

pub(super) fn sys_getppid(kernel: &Kernel) -> Result<usize, &'static str> {
    let cur = kernel.cur_task(0);
    match cur {
        Some(t) => {
            let parent = t.process.parent.lock().unwrap();
            match parent.as_ref() {
                Some(p) => Ok(p.process_pid()),
                None => Ok(0),
            }
        }
        None => Ok(0),
    }
}

// AGENT: validate POSIX-style process-group changes before asking TaskTable to
// perform the single authoritative membership update.
pub(super) fn sys_setpgid(kernel: &Kernel, a0: usize, a1: usize) -> Result<usize, &'static str> {
    let pid = a0;
    let pgid = a1;
    let cur = kernel.cur_task(0).ok_or("esrch")?;
    let caller_pid = cur.process_pid();
    let target_pid = if pid == 0 { caller_pid } else { pid };
    let new_pgid = if pgid == 0 { target_pid } else { pgid };
    if new_pgid > i32::MAX as usize {
        return Err("einval");
    }
    let target = kernel.tasks.find(target_pid).ok_or("esrch")?;
    if target_pid != caller_pid {
        let parent = target.process.parent.lock().unwrap();
        let is_child = parent
            .as_ref()
            .map(|p| p.process_pid() == caller_pid)
            .unwrap_or(false);
        drop(parent);
        if !is_child {
            return Err("esrch");
        }
        if target.process.did_exec.load(Ordering::SeqCst) {
            return Err("eacces");
        }
    }
    let caller_sid = cur.process_sid();
    let target_sid = target.process_sid();
    if caller_sid != target_sid {
        return Err("eperm");
    }
    if target.is_session_leader() {
        return Err("eperm");
    }
    kernel
        .tasks
        .move_process_to_group(&target, new_pgid as Pgid)?;
    Ok(0)
}

pub(super) fn sys_getpgid(kernel: &Kernel, a0: usize) -> Result<usize, &'static str> {
    let pid = a0;
    let cur = kernel.cur_task(0);
    let target = if pid == 0 {
        cur.as_ref().map(|t| t.process_pid()).unwrap_or(0)
    } else {
        pid
    };
    if target == 0 {
        return Err("esrch");
    }
    match kernel.tasks.find(target) {
        Some(t) => Ok(*t.process.pgid.lock().unwrap() as usize),
        None => Err("esrch"),
    }
}

// AGENT: create a new session through TaskTable so sid, pgid, and group
// membership are updated atomically from the syscall boundary.
pub(super) fn sys_setsid(kernel: &Kernel) -> Result<usize, &'static str> {
    let cur = kernel.cur_task(0);
    if let Some(t) = cur {
        kernel.tasks.start_new_session(&t)
    } else {
        Err("esrch")
    }
}
