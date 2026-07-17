// AGENT
use super::*;

// AGENT: matches the userspace litc sigaction layout used by kernel-sim tests.
#[repr(C)]
#[derive(Clone, Copy)]
struct UserSigAction {
    sa_handler: usize,
    sa_sigaction: usize,
    sa_mask: u64,
    sa_flags: i32,
}

// AGENT: accept Linux signal numbers through 64 while retaining signal zero as
// kill's existence/permission probe rather than enqueueing it for delivery.
pub(super) fn sys_kill(kernel: &Kernel, a0: usize, a1: usize) -> Result<usize, &'static str> {
    let pid = a0 as isize;
    let sig = a1;
    if sig > NSIG as usize {
        return Err("einval");
    }

    let protected =
        |tid: usize| (sig == SIGKILL as usize || sig == SIGSTOP as usize) && tid <= INIT_PID;
    let send_one = |t: &Arc<Task>| -> bool {
        if protected(t.id()) {
            return false;
        }
        if !t.done() && sig != 0 {
            kernel.send_signal_to_task(t, sig as i32, -1);
        }
        true
    };
    let finish_many = |targets: Vec<Arc<Task>>| -> Result<usize, &'static str> {
        if targets.is_empty() {
            return Err("esrch");
        }
        let sent = targets.iter().filter(|t| send_one(t)).count();
        if sent == 0 {
            if targets.iter().any(|t| protected(t.id())) {
                Err("eperm")
            } else {
                Err("esrch")
            }
        } else {
            Ok(0)
        }
    };

    match pid {
        0 => {
            let cur = kernel.cur_task(0);
            if let Some(t) = cur {
                let pgid = *t.process.pgid.lock().unwrap();
                finish_many(kernel.tasks.pgid_group(pgid))
            } else {
                Err("esrch")
            }
        }
        -1 => {
            let cur_id = kernel.cur_task(0).map(|t| t.id());
            let targets = kernel
                .tasks
                .active_tasks()
                .into_iter()
                .filter(|tid| Some(*tid) != cur_id)
                .filter_map(|tid| kernel.tasks.find(tid))
                .collect();
            finish_many(targets)
        }
        p if p > 0 => match kernel.tasks.find(p as usize) {
            Some(t) => {
                if send_one(&t) {
                    Ok(0)
                } else {
                    Err("eperm")
                }
            }
            None => Err("esrch"),
        },
        p => {
            let pgid = (-p) as Pgid;
            finish_many(kernel.tasks.pgid_group(pgid))
        }
    }
}

// AGENT: validate the inclusive Linux/RISC-V signal-number range before
// reading or installing one process-wide disposition.
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
    let act_size = mem::size_of::<UserSigAction>();
    if signo == 0 || signo > NSIG as usize {
        return Err("einval");
    }
    if signo == SIGKILL as usize || signo == SIGSTOP as usize {
        return Err("einval");
    } // AGENT: fix inverted condition
    if act_addr != 0 && !check_access(act_addr, act_size) {
        return Err("efault");
    }
    if oldact_addr != 0 && !check_access(oldact_addr, act_size) {
        return Err("efault");
    }
    let cur = kernel.cur_task(0).ok_or("esrch")?;
    let signo = signo as u32;

    if oldact_addr != 0 {
        let action = {
            let sig_state = cur.process.sig_state.lock().unwrap();
            sig_state.get_action(signo).ok_or("einval")?.clone()
        };
        let old = UserSigAction {
            sa_handler: action.handler,
            sa_sigaction: action.handler,
            sa_mask: action.mask,
            sa_flags: 0,
        };
        unsafe {
            ptr::write_unaligned(oldact_addr as *mut UserSigAction, old);
        }
    }

    if act_addr != 0 {
        let act = unsafe { ptr::read_unaligned(act_addr as *const UserSigAction) };
        let requested_flags = if a3 != 0 { a3 } else { act.sa_flags as usize };
        // TODO(AGENT): implement sigaction flags as one coherent delivery
        // feature, starting with real SA_SIGINFO frames; until then reject
        // nonzero flags instead of storing or partially honoring them.
        if requested_flags != 0 {
            return Err("enotsup");
        }
        let sa_mask = if a4 != 0 { a4 as u64 } else { act.sa_mask };
        if !cur.process.set_signal_action(
            signo,
            SigAction {
                handler: act.sa_handler,
                mask: sa_mask,
            },
        ) {
            return Err("einval");
        }
    }
    Ok(0)
}

// AGENT: preserve userspace's signo-minus-one sigset_t representation while
// enforcing the kernel invariant that SIGKILL and SIGSTOP remain unblocked.
pub(super) fn sys_sigprocmask(
    kernel: &Kernel,
    a0: usize,
    a1: usize,
    a2: usize,
) -> Result<usize, &'static str> {
    let how = a0;
    let set_addr = a1;
    let oldset_addr = a2;
    const SIG_BLOCK_HOW: usize = 1;
    const SIG_SETMASK_HOW: usize = 2;
    const SIG_UNBLOCK_HOW: usize = 3;
    if set_addr != 0 && !check_access(set_addr, 8) {
        return Err("efault");
    }
    if oldset_addr != 0 && !check_access(oldset_addr, 8) {
        return Err("efault");
    }
    // AGENT: userspace sigset_t maps signal N to bit N-1, including the
    // unmaskable SIGKILL and SIGSTOP bits removed at this syscall boundary.
    let unmaskable = UNMASKABLE_SIGNAL_MASK;
    let t = kernel.cur_task(0).ok_or("esrch")?;
    let old_mask = *t.sig_mask.lock().unwrap();
    if oldset_addr != 0 {
        // AGENT: expose the previous blocked-set value back to userspace.
        unsafe {
            ptr::write_unaligned(oldset_addr as *mut u64, old_mask);
        }
    }
    if set_addr != 0 {
        // AGENT: userspace passes a pointer to sigset_t, not the mask value itself.
        let new_set = unsafe { ptr::read_unaligned(set_addr as *const u64) };
        let mut mask = t.sig_mask.lock().unwrap();
        match how {
            SIG_BLOCK_HOW => {
                *mask = (*mask | new_set) & !unmaskable;
            }
            SIG_SETMASK_HOW => {
                *mask = new_set & !unmaskable;
            }
            SIG_UNBLOCK_HOW => {
                *mask &= !new_set;
            }
            _ => {
                return Err("einval");
            }
        }
    }
    Ok(0)
}

// AGENT: restore the last complete user signal frame through the syscall outcome
// so the trap owner can replace its live frame without creating a second alias.
pub(super) fn sys_sigreturn(kernel: &Kernel) -> Result<SyscallOutcome, &'static str> {
    let t = kernel.cur_task(0).ok_or("esrch")?;
    Ok(SyscallOutcome::RestoreUserContext(
        t.restore_signal_frame()?,
    ))
}
