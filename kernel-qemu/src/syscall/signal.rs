// AGENT
use super::*;

const KERNEL_SIGSET_SIZE: usize = mem::size_of::<u64>();
const _: () = assert!(mem::size_of::<usize>() == KERNEL_SIGSET_SIZE);

// AGENT: match the RV64 asm-generic kernel rt_sigaction ABI: RISC-V has no
// SA_RESTORER field, and the extensible signal mask is the final 8-byte field.
#[derive(Clone, Copy)]
struct UserRtSigAction {
    handler: usize,
    flags: usize,
    mask: u64,
}

// AGENT: keep fixed ABI encoding separate from Rust layout and route every
// userspace access through the live AddrSpace translation helpers.
impl UserRtSigAction {
    const SIZE: usize = mem::size_of::<usize>() * 2 + KERNEL_SIGSET_SIZE;

    // AGENT: copy in one RV64 kernel sigaction without dereferencing its user VA.
    fn read_from(task: &Task, addr: usize) -> Result<Self, &'static str> {
        let mut bytes = [0u8; Self::SIZE];
        task.process
            .addr_space
            .lock()
            .unwrap()
            .read_user_bytes(addr, &mut bytes)?;

        let mut handler = [0u8; mem::size_of::<usize>()];
        handler.copy_from_slice(&bytes[..8]);
        let mut flags = [0u8; mem::size_of::<usize>()];
        flags.copy_from_slice(&bytes[8..16]);
        let mut mask = [0u8; KERNEL_SIGSET_SIZE];
        mask.copy_from_slice(&bytes[16..24]);
        Ok(Self {
            handler: usize::from_ne_bytes(handler),
            flags: usize::from_ne_bytes(flags),
            mask: u64::from_ne_bytes(mask),
        })
    }

    // AGENT: copy out one RV64 kernel sigaction through the writable user VMA.
    fn write_to(&self, kernel: &Kernel, task: &Task, addr: usize) -> Result<(), &'static str> {
        let mut bytes = [0u8; Self::SIZE];
        bytes[..8].copy_from_slice(&self.handler.to_ne_bytes());
        bytes[8..16].copy_from_slice(&self.flags.to_ne_bytes());
        bytes[16..24].copy_from_slice(&self.mask.to_ne_bytes());
        task.process
            .addr_space
            .lock()
            .unwrap()
            .write_user_bytes(addr, &bytes, &kernel.pool)
    }
}

// AGENT: copy one RV64 kernel sigset_t from the current process page table.
fn read_user_sigset(task: &Task, addr: usize) -> Result<u64, &'static str> {
    let mut bytes = [0u8; KERNEL_SIGSET_SIZE];
    task.process
        .addr_space
        .lock()
        .unwrap()
        .read_user_bytes(addr, &mut bytes)?;
    Ok(u64::from_ne_bytes(bytes))
}

// AGENT: copy one RV64 kernel sigset_t to the current process page table.
fn write_user_sigset(
    kernel: &Kernel,
    task: &Task,
    addr: usize,
    set: u64,
) -> Result<(), &'static str> {
    task.process
        .addr_space
        .lock()
        .unwrap()
        .write_user_bytes(addr, &set.to_ne_bytes(), &kernel.pool)
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
        |pid: usize| (sig == SIGKILL as usize || sig == SIGSTOP as usize) && pid <= INIT_PID;
    let send_one = |process: &Arc<Process>| -> bool {
        if protected(process.pid()) {
            return false;
        }
        if !process.is_terminating() && !process.is_zombie() && sig != 0 {
            kernel.send_signal_to_process(process, sig as i32, -1);
        }
        true
    };
    let finish_many = |targets: Vec<Arc<Process>>| -> Result<usize, &'static str> {
        if targets.is_empty() {
            return Err("esrch");
        }
        let sent = targets.iter().filter(|process| send_one(process)).count();
        if sent == 0 {
            if targets.iter().any(|process| protected(process.pid())) {
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
                let pgid = kernel.tasks.process_pgid(t.process.pid()).ok_or("esrch")?;
                finish_many(kernel.tasks.pgid_group(pgid))
            } else {
                Err("esrch")
            }
        }
        -1 => {
            let cur_pid = kernel.cur_task(0).map(|task| task.process.pid());
            let targets = kernel
                .tasks
                .active_processes()
                .into_iter()
                .filter(|process| Some(process.pid()) != cur_pid)
                .collect();
            finish_many(targets)
        }
        p if p > 0 => match kernel.tasks.find_process(p as usize) {
            Some(process) => {
                if send_one(&process) {
                    Ok(0)
                } else {
                    Err("eperm")
                }
            }
            None => Err("esrch"),
        },
        p => {
            let pgid = (-p) as i32;
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
) -> Result<usize, &'static str> {
    let signo = a0;
    let act_addr = a1;
    let oldact_addr = a2;
    let sigsetsize = a3;
    if sigsetsize != KERNEL_SIGSET_SIZE {
        return Err("einval");
    }
    if signo == 0 || signo > NSIG as usize {
        return Err("einval");
    }
    if signo == SIGKILL as usize || signo == SIGSTOP as usize {
        return Err("einval");
    }
    let cur = kernel.cur_task(0).ok_or("esrch")?;
    let signo = signo as u32;

    let requested = if act_addr == 0 {
        None
    } else {
        let action = UserRtSigAction::read_from(&cur, act_addr)?;
        // TODO(AGENT): implement sigaction flags as one coherent delivery
        // feature, starting with real SA_SIGINFO frames; until then reject
        // nonzero flags instead of storing or partially honoring them.
        if action.flags != 0 {
            return Err("enotsup");
        }
        Some(action)
    };

    let old_action = {
        let sig_state = cur.process.sig_state.lock().unwrap();
        sig_state.get_action(signo).ok_or("einval")?.clone()
    };
    if oldact_addr != 0 {
        UserRtSigAction {
            handler: old_action.handler,
            flags: 0,
            mask: old_action.mask,
        }
        .write_to(kernel, &cur, oldact_addr)?;
    }

    if let Some(action) = requested {
        if !cur.process.set_signal_action(
            signo,
            SigAction {
                handler: action.handler,
                mask: action.mask,
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
    a3: usize,
) -> Result<usize, &'static str> {
    let how = a0;
    let set_addr = a1;
    let oldset_addr = a2;
    let sigsetsize = a3;
    const SIG_BLOCK_HOW: usize = 0;
    const SIG_UNBLOCK_HOW: usize = 1;
    const SIG_SETMASK_HOW: usize = 2;
    if sigsetsize != KERNEL_SIGSET_SIZE {
        return Err("einval");
    }
    // AGENT: userspace sigset_t maps signal N to bit N-1, including the
    // unmaskable SIGKILL and SIGSTOP bits removed at this syscall boundary.
    let unmaskable = UNMASKABLE_SIGNAL_MASK;
    let t = kernel.cur_task(0).ok_or("esrch")?;
    // AGENT: capture the requested set before copy-out so aliased set/oldset
    // pointers retain Linux's input value rather than the overwritten old mask.
    let requested_set = if set_addr == 0 {
        None
    } else {
        Some(read_user_sigset(&t, set_addr)?)
    };
    let old_mask = *t.sig_mask.lock().unwrap();
    if oldset_addr != 0 {
        write_user_sigset(kernel, &t, oldset_addr, old_mask)?;
    }
    if let Some(new_set) = requested_set {
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
