// AGENT
use super::*;

pub(super) fn sys_kill(kernel: &Kernel, a0: usize, a1: usize) -> Result<usize, &'static str> {
    let pid = a0 as isize;
    let sig = a1;
    if sig > NSIG as usize {
        return Err("einval");
    }
    if sig == SIGKILL as usize || sig == SIGSTOP as usize {
        let target_pid = if pid < 0 {
            (-pid) as usize
        } else {
            pid as usize
        };
        if target_pid <= 1 {
            return Err("eperm");
        }
    }
    match pid {
        0 => {
            let cur = kernel.cur_task(0);
            if let Some(t) = cur {
                let pgid = *t.pgid.lock().unwrap();
                let n = kernel.tasks.send_signal_group(pgid, sig as i32);
                Ok(n)
            } else {
                Ok(0)
            }
        }
        -1 => {
            let all = kernel.tasks.active_tasks();
            let mut sent = 0;
            for tid in all {
                if tid <= 1 {
                    continue;
                }
                if let Some(t) = kernel.tasks.find(tid) {
                    t.send_sig(sig as i32, -1);
                    sent += 1;
                }
            }
            if sent == 0 {
                Err("esrch")
            } else {
                Ok(sent)
            }
        }
        p if p > 0 => match kernel.tasks.find(p as usize) {
            Some(t) => {
                if t.done() && sig != 0 {
                    return Err("esrch");
                }
                t.send_sig(sig as i32, -1);
                Ok(0)
            }
            None => Err("esrch"),
        },
        p => {
            let pgid = (-p) as Pgid;
            let n = kernel.tasks.send_signal_group(pgid, sig as i32);
            if n == 0 {
                Err("esrch")
            } else {
                Ok(n)
            }
        }
    }
}

pub(super) fn sys_sigaction(
    kernel: &Kernel,
    a0: usize,
    a1: usize,
    a2: usize,
    a3: usize,
    a4: usize,
) -> Result<usize, &'static str> {
    let signo = a0;
    let act_addr = a1;
    let oldact_addr = a2;
    if signo == 0 || signo >= NSIG as usize {
        return Err("einval");
    }
    if signo == SIGKILL as usize || signo == SIGSTOP as usize {
        return Err("einval");
    } // AGENT: fix inverted condition
    if act_addr != 0 && !check_access(act_addr, 32) {
        return Err("efault");
    }
    if oldact_addr != 0 && !check_access(oldact_addr, 32) {
        return Err("efault");
    }
    let _sa_flags = if act_addr != 0 { a3 & 0xFFFF } else { 0 };
    let _sa_mask = if act_addr != 0 { a4 } else { 0 };
    Ok(0)
}

pub(super) fn sys_sigprocmask(
    kernel: &Kernel,
    a0: usize,
    a1: usize,
    a2: usize,
) -> Result<usize, &'static str> {
    let how = a0;
    let set_addr = a1;
    let oldset_addr = a2;
    if set_addr != 0 && !check_access(set_addr, 8) {
        return Err("efault");
    }
    if oldset_addr != 0 && !check_access(oldset_addr, 8) {
        return Err("efault");
    }
    let unmaskable: u64 = (1u64 << SIGKILL) | (1u64 << SIGSTOP);
    let cur = kernel.cur_task(0);
    if let Some(t) = cur {
        let old_mask = *t.sig_mask.lock().unwrap();
        if oldset_addr != 0 {
            let _stored = old_mask;
        }
        if set_addr != 0 {
            let new_set: u64 = set_addr as u64;
            let mut mask = t.sig_mask.lock().unwrap();
            match how {
                0 => {
                    *mask = (*mask | new_set) & !unmaskable;
                }
                1 => {
                    *mask = *mask & !new_set;
                }
                2 => {
                    *mask = new_set & !unmaskable;
                }
                _ => {
                    return Err("einval");
                }
            }
        }
    }
    Ok(0)
}
