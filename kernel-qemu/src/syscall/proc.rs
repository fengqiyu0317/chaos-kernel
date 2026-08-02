// AGENT
use super::*;
use crate::trap::TrapFrame;

const WAIT4_WNOHANG: usize = 1;
const CLONE_EXIT_SIGNAL_MASK: usize = 0xff;
// AGENT: bound exec pathname and combined argv/envp copy-in before allocating
// a replacement image; the full stack footprint remains capped by USR_STK_SZ.
const EXEC_PATH_MAX: usize = 4096;
const EXEC_ARG_MAX: usize = USR_STK_SZ;
const EXEC_MAX_POINTERS: usize = 128;

// AGENT: make argv and envp share both byte and pointer budgets instead of
// allowing each userspace array to consume an independent maximum.
#[derive(Default)]
struct ExecCopyBudget {
    string_bytes: usize,
    pointers: usize,
}

// AGENT: centralize checked accounting for every copied exec argument.
impl ExecCopyBudget {
    fn remaining_bytes(&self) -> usize {
        EXEC_ARG_MAX.saturating_sub(self.string_bytes)
    }

    fn charge_pointer(&mut self) -> Result<(), &'static str> {
        self.pointers = self.pointers.checked_add(1).ok_or("e2big")?;
        if self.pointers > EXEC_MAX_POINTERS {
            return Err("e2big");
        }
        Ok(())
    }

    fn charge_string(&mut self, len: usize) -> Result<(), &'static str> {
        let len_with_nul = len.checked_add(1).ok_or("e2big")?;
        self.string_bytes = self.string_bytes.checked_add(len_with_nul).ok_or("e2big")?;
        if self.string_bytes > EXEC_ARG_MAX {
            return Err("e2big");
        }
        Ok(())
    }
}

impl Kernel {
    // AGENT: keep the thread-local exit helper beside sys_exit; only the shared
    // process teardown primitives remain in kernel_ops/process.rs.
    pub(crate) fn exit_current_thread(
        &self,
        cpu: usize,
        task: &Arc<Task>,
        reason: ExitReason,
    ) -> Result<(), &'static str> {
        let process = task.process.clone();
        self.prepare_current_thread_retirement(cpu, task)?;
        let decision = process.complete_thread_exit(task.id(), Some(reason))?;
        self.publish_current_thread_retirement(task, &process);
        if decision == ThreadExitDecision::Last {
            self.commit_process_exit(&process);
        }
        Ok(())
    }

    // AGENT: fork from the complete frame captured by the active syscall trap
    // so the child resumes after ecall with the caller's full register state.
    pub(crate) fn do_fork_from_frame(
        &self,
        parent: &Arc<Task>,
        caller_frame: &TrapFrame,
    ) -> Result<usize, &'static str> {
        if parent.done() {
            return Err("esrch");
        }

        let child = self.tasks.fork_process_from_frame(parent, caller_frame)?;
        let child_id = child.id();
        child.set_sched_state(TaskRunState::Runnable);
        child.reset_slice();
        self.run_queue.enqueue(&child);
        Ok(child_id)
    }

    // AGENT: wait for a matching child to become reapable, but leave the final
    // zombie deletion to sys_wait4 after any userspace status copyout succeeds.
    pub(crate) fn do_wait(
        &self,
        parent_id: usize,
        target_pid: isize,
        options: usize,
    ) -> Result<(usize, usize), &'static str> {
        let parent = self.tasks.process_of_tid(parent_id).ok_or("esrch")?;
        let wnohang = (options & WAIT4_WNOHANG) != 0;

        loop {
            if let Some(child) = self.find_waitable_child(&parent, target_pid)? {
                return Ok(child);
            }

            if wnohang {
                return Ok((0, 0));
            }

            let wait = Self::prepare_child_wait(&parent, parent_id);
            if let Some(child) = self.find_waitable_child(&parent, target_pid)? {
                Self::cancel_child_wait(&parent, wait);
                return Ok(child);
            }

            let outcome = wait.0.wait_interruptible(None);
            Self::cancel_child_wait(&parent, wait);
            match outcome {
                WaitOutcome::Signal => return Err("eintr"),
                WaitOutcome::GroupExit => return Err("group_exit"),
                WaitOutcome::Event | WaitOutcome::Timeout => {}
            }
        }
    }

    // AGENT: scan only the parent's current child list; blocking and reaping
    // stay separate so sys_wait4 can perform fallible status copyout first.
    fn find_waitable_child(
        &self,
        parent: &Arc<Process>,
        target_pid: isize,
    ) -> Result<Option<(usize, usize)>, &'static str> {
        let children = parent.children_snapshot();
        if children.is_empty() {
            return Err("echild");
        }

        let mut matched = false;
        for child in &children {
            if !self.child_matches_wait_target(parent, child, target_pid) {
                continue;
            }

            matched = true;
            if let Some(status) = child.zombie_wait_status() {
                return Ok(Some((child.pid(), status)));
            }
        }

        if matched {
            Ok(None)
        } else {
            Err("echild")
        }
    }

    // AGENT: keep wait4 pid and process-group selection in one predicate.
    fn child_matches_wait_target(
        &self,
        parent: &Process,
        child: &Process,
        target_pid: isize,
    ) -> bool {
        match target_pid {
            -1 => true,
            0 => match (
                self.tasks.process_pgid(child.pid()),
                self.tasks.process_pgid(parent.pid()),
            ) {
                (Some(child_pgid), Some(parent_pgid)) => child_pgid == parent_pgid,
                _ => false,
            },
            pid if pid > 0 => child.pid() == pid as usize,
            pgid => self.tasks.process_pgid(child.pid()) == Some((-pgid) as i32),
        }
    }

    // AGENT: clear stale child-exit readiness before subscribing so a later
    // child exit changes the event bits and wakes this one-shot waiter.
    fn prepare_child_wait(parent: &Arc<Process>, parent_task_id: usize) -> (WaitToken, usize) {
        let token = WaitToken::for_task(parent_task_id);
        let wake_token = token.clone();
        let sub_id = {
            let mut ev = parent.ev.lock().unwrap();
            ev.clear(EvFlag::CHILD_QUIT);
            ev.sub(
                EvFlag::CHILD_QUIT,
                Box::new(move |_| {
                    wake_token.wake();
                    true
                }),
            )
        };
        (token, sub_id)
    }

    // AGENT: remove the one-shot subscription when wait4 returns or is
    // interrupted before the child-exit event fires.
    fn cancel_child_wait(parent: &Arc<Process>, (_token, sub_id): (WaitToken, usize)) {
        parent.ev.lock().unwrap().unsub(sub_id);
    }
}

// AGENT: accept only the RV64 clone form that is semantically equivalent to
// fork, then enter the shared TrapFrame/COW creation path directly; later clone
// milestones must explicitly add stack, TLS, and TID effects.
pub(super) fn sys_clone(
    kernel: &Kernel,
    flags: usize,
    child_stack: usize,
    parent_tid: usize,
    tls: usize,
    child_tid: usize,
    caller_frame: Option<&TrapFrame>,
) -> Result<usize, &'static str> {
    let exit_signal = flags & CLONE_EXIT_SIGNAL_MASK;
    let feature_flags = flags & !CLONE_EXIT_SIGNAL_MASK;
    if exit_signal != SIGCHLD as usize {
        return Err("einval");
    }
    if feature_flags != 0 || child_stack != 0 || parent_tid != 0 || tls != 0 || child_tid != 0 {
        return Err("enotsup");
    }
    let parent = kernel.cur_task(0).ok_or("esrch")?;
    let stored_frame;
    let caller_frame = match caller_frame {
        Some(frame) => frame,
        None => {
            stored_frame = parent.snapshot_user_trap_frame()?;
            &stored_frame
        }
    };
    kernel.do_fork_from_frame(&parent, caller_frame)
}

pub(super) fn sys_exec(
    kernel: &Kernel,
    a0: usize,
    a1: usize,
    a2: usize,
) -> Result<SyscallOutcome, &'static str> {
    let path_addr = a0;
    let argv_addr = a1;
    let envp_addr = a2;
    let task = kernel.cur_task(0).ok_or("esrch")?;
    let task_id = task.id();
    // AGENT: expose the temporary pre-thread-exec boundary at the syscall edge
    // before even usercopy can obscure it with an unrelated pointer error.
    if task.process.thread_count() != 1 {
        return Err("enotsup");
    }
    let addr_space = task.process.addr_space.lock().unwrap();
    let path_bytes = read_user_c_bytes(&addr_space, path_addr, EXEC_PATH_MAX, "enametoolong")?;
    // AGENT: keep raw bytes for argv/envp, but retain the current UTF-8 VFS
    // pathname boundary until directory entries gain byte-string names.
    let path = core::str::from_utf8(&path_bytes).map_err(|_| "einval")?;
    let mut budget = ExecCopyBudget::default();
    let args = read_user_byte_array(&addr_space, argv_addr, &mut budget)?;
    let envs = read_user_byte_array(&addr_space, envp_addr, &mut budget)?;
    if ProcInit::checked_total_size_for(&args, &envs, 2)? > EXEC_ARG_MAX {
        return Err("e2big");
    }
    drop(addr_space);

    let user_entry = kernel.do_exec_for_trap(task_id, path, args, envs)?;
    Ok(SyscallOutcome::ReplaceUserContext {
        entry: user_entry.entry,
        stack_pointer: user_entry.stack_pointer,
    })
}

// AGENT: copy one NUL-terminated userspace string without imposing UTF-8 on
// argv/envp; the returned byte vector deliberately excludes the terminator.
fn read_user_c_bytes(
    addr_space: &AddrSpace,
    addr: usize,
    max_len: usize,
    too_long: &'static str,
) -> Result<UserCString, &'static str> {
    if addr == 0 {
        return Err("efault");
    }
    let mut bytes = Vec::new();
    for offset in 0..max_len {
        let cur = addr.checked_add(offset).ok_or("efault")?;
        let mut byte = [0u8; 1];
        addr_space.read_user_bytes(cur, &mut byte)?;
        if byte[0] == 0 {
            return Ok(bytes);
        }
        bytes.push(byte[0]);
    }
    Err(too_long)
}

// AGENT: walk one native-pointer array while charging the caller-provided
// budget shared by argv and envp, including each copied trailing NUL.
fn read_user_byte_array(
    addr_space: &AddrSpace,
    array_addr: usize,
    budget: &mut ExecCopyBudget,
) -> Result<Vec<UserCString>, &'static str> {
    if array_addr == 0 {
        return Ok(Vec::new());
    }
    let mut out = Vec::new();
    let word = mem::size_of::<usize>();
    let mut idx = 0usize;
    loop {
        let ptr_addr = array_addr
            .checked_add(idx.checked_mul(word).ok_or("efault")?)
            .ok_or("efault")?;
        let ptr = addr_space.read_user_usize(ptr_addr)?;
        if ptr == 0 {
            return Ok(out);
        }
        budget.charge_pointer()?;
        let value = read_user_c_bytes(addr_space, ptr, budget.remaining_bytes(), "e2big")?;
        budget.charge_string(value.len())?;
        out.push(value);
        idx = idx.checked_add(1).ok_or("e2big")?;
    }
}

// AGENT: terminate only the calling Task, allowing the final-thread lifecycle
// decision to promote this syscall into process teardown when necessary. Keep
// current-task lookup and syscall exit-code decoding at the syscall boundary.
pub(super) fn sys_exit(kernel: &Kernel, a0: usize) -> Result<SyscallOutcome, &'static str> {
    let task = kernel.cur_task(0).ok_or("esrch")?;
    kernel.exit_current_thread(0, &task, ExitReason::Code((a0 & 0xFF) as u8))?;
    Ok(SyscallOutcome::NoReturn)
}

// AGENT: terminate every Task in the caller's Process while preserving the
// common NoReturn handoff contract with thread-local SYS_EXIT. Current-task
// lookup and syscall exit-code decoding belong to this syscall adapter.
pub(super) fn sys_exit_group(kernel: &Kernel, a0: usize) -> Result<SyscallOutcome, &'static str> {
    let task = kernel.cur_task(0).ok_or("esrch")?;
    kernel.exit_thread_group(0, &task, ExitReason::Code((a0 & 0xFF) as u8))?;
    Ok(SyscallOutcome::NoReturn)
}

// AGENT: register the current thread's clear-on-exit futex word; address-space
// validation is intentionally deferred to exit-time put-user semantics.
pub(super) fn sys_set_tid_address(kernel: &Kernel, tidptr: usize) -> Result<usize, &'static str> {
    let task = kernel.cur_task(0).ok_or("esrch")?;
    Ok(task.set_clear_child_tid(tidptr))
}

// AGENT: register the fixed RV64 robust-list head for exit-time best-effort
// owner-death cleanup; list contents remain userspace-owned until retirement.
pub(super) fn sys_set_robust_list(
    kernel: &Kernel,
    head: usize,
    len: usize,
) -> Result<usize, &'static str> {
    if len != ROBUST_LIST_HEAD_SIZE {
        return Err("einval");
    }
    let task = kernel.cur_task(0).ok_or("esrch")?;
    task.set_robust_list_head(head);
    Ok(0)
}

// AGENT: copy wait status before committing the zombie reap so a failed user
// write does not lose the child's observable exit state.
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
    if pid != 0 {
        kernel.tasks.reap(pid)?;
    }
    Ok(pid)
}

// AGENT: return immutable Process identity rather than a leader-task id.
pub(super) fn sys_getpid(kernel: &Kernel) -> Result<usize, &'static str> {
    let cur = kernel.cur_task(0);
    match cur {
        Some(t) => Ok(t.process.pid()),
        None => Ok(1),
    }
}

// AGENT: follow the weak process-family link without depending on a parent Task.
pub(super) fn sys_getppid(kernel: &Kernel) -> Result<usize, &'static str> {
    let cur = kernel.cur_task(0);
    match cur {
        Some(t) => Ok(t.process.parent().map(|parent| parent.pid()).unwrap_or(0)),
        None => Ok(0),
    }
}

// AGENT: validate POSIX-style process-group changes before asking TaskTable to
// perform the single authoritative membership update.
pub(super) fn sys_setpgid(kernel: &Kernel, a0: usize, a1: usize) -> Result<usize, &'static str> {
    let pid = a0;
    let pgid = a1;
    let cur = kernel.cur_task(0).ok_or("esrch")?;
    let caller_pid = cur.process.pid();
    let target_pid = if pid == 0 { caller_pid } else { pid };
    let new_pgid = if pgid == 0 { target_pid } else { pgid };
    if new_pgid > i32::MAX as usize {
        return Err("einval");
    }
    let target = kernel.tasks.find_process(target_pid).ok_or("esrch")?;
    if target_pid != caller_pid {
        let is_child = target
            .parent()
            .map(|parent| parent.pid() == caller_pid)
            .unwrap_or(false);
        if !is_child {
            return Err("esrch");
        }
        if target.did_exec.load(Ordering::SeqCst) {
            return Err("eacces");
        }
    }
    let caller_sid = kernel.tasks.process_sid(caller_pid).ok_or("esrch")?;
    let target_sid = kernel.tasks.process_sid(target_pid).ok_or("esrch")?;
    if caller_sid != target_sid {
        return Err("eperm");
    }
    if target_sid == target_pid {
        return Err("eperm");
    }
    kernel
        .tasks
        .move_process_to_group(&target, new_pgid as i32)?;
    Ok(0)
}

pub(super) fn sys_getpgid(kernel: &Kernel, a0: usize) -> Result<usize, &'static str> {
    let pid = a0;
    let cur = kernel.cur_task(0);
    let target = if pid == 0 {
        cur.as_ref().map(|t| t.process.pid()).unwrap_or(0)
    } else {
        pid
    };
    if target == 0 {
        return Err("esrch");
    }
    kernel
        .tasks
        .process_pgid(target)
        .map(|pgid| pgid as usize)
        .ok_or("esrch")
}

// AGENT: create a new session through TaskTable so sid, pgid, and group
// membership are updated atomically from the syscall boundary.
pub(super) fn sys_setsid(kernel: &Kernel) -> Result<usize, &'static str> {
    let cur = kernel.cur_task(0);
    if let Some(t) = cur {
        kernel.tasks.start_new_session(&t.process)
    } else {
        Err("esrch")
    }
}
