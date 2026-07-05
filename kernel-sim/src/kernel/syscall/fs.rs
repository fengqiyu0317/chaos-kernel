// AGENT
use super::*;

const MAX_RW_COUNT: usize = PAGE_SZ * 16;

// AGENT: read a NUL-terminated path from the current user address space.
fn read_user_path(task: &RuntimeTask, addr: usize) -> Result<String, &'static str> {
    if addr == 0 {
        return Err("efault");
    }
    let addr_space = task.process.addr_space.lock().unwrap();
    let mut bytes = Vec::new();
    for offset in 0..4096 {
        let cur = addr.checked_add(offset).ok_or("efault")?;
        let mut byte = [0u8; 1];
        addr_space.read_user_bytes(cur, &mut byte)?;
        if byte[0] == 0 {
            return String::from_utf8(bytes).map_err(|_| "einval");
        }
        bytes.push(byte[0]);
    }
    Err("enametoolong")
}

fn fdopt_to_open_flags(opt: FdOpt) -> usize {
    let mut flags = match (opt.rd, opt.wr) {
        (true, true) => 2,
        (false, true) => 1,
        _ => 0,
    };
    if opt.nb {
        flags |= O_NONBLOCK;
    }
    if opt.ap {
        flags |= O_APPEND;
    }
    flags
}

pub(super) fn sys_read(
    kernel: &RuntimeKernel,
    a0: usize,
    a1: usize,
    a2: usize,
) -> Result<usize, &'static str> {
    let fd = a0;
    let buf_addr = a1;
    let count = a2;
    if count == 0 {
        return Ok(0);
    }
    if buf_addr == 0 {
        return Err("efault");
    }
    let task = kernel.cur_task(0).ok_or("esrch")?;
    let request_len = min(count, MAX_RW_COUNT);
    let writable_len = {
        let addr_space = task.process.addr_space.lock().unwrap();
        addr_space.writable_user_prefix_len(buf_addr, request_len)?
    };
    let entry = task.get_fd_entry(fd).ok_or("ebadf")?;
    let mut tmp = vec![0u8; writable_len];
    let nread = entry.read(&mut tmp)?;
    if nread > 0 {
        task.process.addr_space.lock().unwrap().write_user_bytes(
            buf_addr,
            &tmp[..nread],
            &kernel.pool,
        )?;
    }
    Ok(nread)
}

pub(super) fn sys_write(
    kernel: &RuntimeKernel,
    a0: usize,
    a1: usize,
    a2: usize,
) -> Result<usize, &'static str> {
    let fd = a0;
    let buf_addr = a1;
    let count = a2;
    if count == 0 {
        return Ok(0);
    }
    if buf_addr == 0 {
        return Err("efault");
    }
    let task = kernel.cur_task(0).ok_or("esrch")?;
    let request_len = min(count, MAX_RW_COUNT);
    let readable_len = {
        let addr_space = task.process.addr_space.lock().unwrap();
        addr_space.readable_user_prefix_len(buf_addr, request_len)?
    };
    let mut tmp = vec![0u8; readable_len];
    if readable_len > 0 {
        task.process
            .addr_space
            .lock()
            .unwrap()
            .read_user_bytes(buf_addr, &mut tmp)?;
    }
    let entry = task.get_fd_entry(fd).ok_or("ebadf")?;
    entry.write(&tmp)
}

pub(super) fn sys_open(
    kernel: &RuntimeKernel,
    a0: usize,
    a1: usize,
    a2: usize,
) -> Result<usize, &'static str> {
    let path_addr = a0;
    let flags = a1;
    let mode = a2;
    let acc_mode = flags & 0x3;
    if acc_mode == 3 {
        return Err("einval");
    }
    let _rdonly = acc_mode == 0;
    let _wronly = acc_mode == 1;
    let _rdwr = acc_mode == 2;
    let _create = (flags & O_CREAT) != 0;
    let _excl = (flags & O_EXCL) != 0;
    let _truncate = (flags & O_TRUNC) != 0;
    let _nonblock = (flags & O_NONBLOCK) != 0;
    let _append = (flags & O_APPEND) != 0;
    let _cloexec = (flags & O_CLOEXEC) != 0;
    let _follow_sym = (flags & AT_NOFOLLOW) == 0;

    let task = kernel.cur_task(0).ok_or("esrch")?;
    let path = read_user_path(&task, path_addr)?;
    let resolved = kernel.lookup_path(&path)?;
    let existing = kernel.file_nodes.read().unwrap().get(&resolved).cloned();
    if _create && _excl && existing.is_some() {
        return Err("eexist");
    }
    let node = match existing {
        Some(node) => node,
        None if _create => {
            let node = Arc::new(FileNode::regular(Vec::new(), false));
            kernel
                .file_nodes
                .write()
                .unwrap()
                .insert(resolved.clone(), node.clone());
            node
        }
        None => return Err("enoent"),
    };
    if node.kind != FileKind::Regular {
        return Err("eisdir");
    }
    let rd = _rdonly || _rdwr;
    let wr = _wronly || _rdwr;
    let opt = FdOpt {
        rd,
        wr,
        ap: _append,
        nb: _nonblock,
    };
    let fh = FHandle::with_node(&resolved, opt, node, _cloexec);
    if _truncate && wr {
        fh.set_len(0)?;
    }
    let fd = task.add_file_with_cloexec(FLike::File(fh), _cloexec);
    let _perm_check = {
        let owner_r = (mode >> 8) & 0x4;
        let owner_w = (mode >> 8) & 0x2;
        let group_r = (mode >> 4) & 0x4;
        let other_r = mode & 0x4;
        owner_r | owner_w | group_r | other_r
    };
    Ok(fd)
}

pub(super) fn sys_close(kernel: &RuntimeKernel, a0: usize) -> Result<usize, &'static str> {
    let fd = a0;
    // AGENT: use the fd limit instead of the process-count constant.
    if fd >= MAX_FD {
        return Err("ebadf");
    }
    let t = kernel.cur_task(0).ok_or("esrch")?;
    // AGENT: close only releases the process fd; block-cache keys are device
    // blocks, not process-local descriptor numbers.
    t.close_fd(fd)?;
    Ok(0)
}

pub(super) fn sys_stat(
    kernel: &RuntimeKernel,
    nr: usize,
    a0: usize,
    a1: usize,
) -> Result<usize, &'static str> {
    let stat_buf = a1;
    if stat_buf == 0 {
        return Err("efault");
    }
    let stat_size = 144;
    if !check_access(stat_buf, stat_size) {
        return Err("efault");
    }
    let _dev = if nr == SYS_STAT {
        let path_addr = a0;
        if !check_access(path_addr, 4096) {
            return Err("efault");
        } // HUMAN
        let tbl = kernel.mnt.entries.read().unwrap();
        tbl.len()
    } else {
        let fd = a0;
        fd / 4
    };
    Ok(0)
}

pub(super) fn sys_ioctl(
    kernel: &RuntimeKernel,
    a0: usize,
    a1: usize,
    a2: usize,
) -> Result<usize, &'static str> {
    let fd = a0;
    let cmd = a1;
    let arg = a2;
    match cmd {
        TCGETS => {
            if !check_access(arg, std::mem::size_of::<TrmIO>()) {
                return Err("efault");
            }
            Ok(0)
        }
        TCSETS => {
            if !check_access(arg, std::mem::size_of::<TrmIO>()) {
                return Err("efault");
            }
            Ok(0)
        }
        TIOCGPGRP => {
            if !check_access(arg, 4) {
                return Err("efault");
            }
            Ok(0)
        }
        TIOCSPGRP => {
            if !check_access(arg, 4) {
                return Err("efault");
            }
            Ok(0)
        }
        TIOCGWINSZ => {
            if !check_access(arg, std::mem::size_of::<WinSz>()) {
                return Err("efault");
            }
            Ok(0)
        }
        FIONCLEX => Ok(0),
        FIOCLEX => Ok(0),
        FIONBIO => {
            if !check_access(arg, 4) {
                return Err("efault");
            }
            Ok(0)
        }
        _ => Err("enotty"),
    }
}

pub(super) fn sys_pipe(kernel: &RuntimeKernel, a0: usize, a1: usize) -> Result<usize, &'static str> {
    let fds_addr = a0;
    let pipe_flags = a1;
    if fds_addr == 0 {
        return Err("efault");
    }
    if !check_access(fds_addr, 2 * std::mem::size_of::<i32>()) {
        return Err("efault");
    }
    let cur = kernel.cur_task(0);
    if let Some(t) = cur {
        let fd_count = t.fd_count();
        // AGENT: pipe consumes two file descriptors, bounded by MAX_FD.
        if fd_count + 2 > MAX_FD {
            return Err("emfile");
        }
        let (rd, wr) = PipeNode::pair();
        let _nonblock = (pipe_flags & O_NONBLOCK) != 0;
        let _cloexec = (pipe_flags & O_CLOEXEC) != 0;
        let rd_fd = t.add_file_with_cloexec(FLike::Pipe(rd), _cloexec);
        let wr_fd = t.add_file_with_cloexec(FLike::Pipe(wr), _cloexec);
        Ok(rd_fd | (wr_fd << 32))
    } else {
        Err("esrch")
    }
}

pub(super) fn sys_dup(kernel: &RuntimeKernel, a0: usize) -> Result<usize, &'static str> {
    // AGENT: fixed — was not checking old_fd existence, not duplicating file object, and searching from old_fd instead of 0
    let old_fd = a0;
    // AGENT: validate fd number against the fd limit, not N_PROC.
    if old_fd >= MAX_FD {
        return Err("ebadf");
    }
    let task = kernel.cur_task(0).ok_or("esrch")?;
    task.dup_fd(old_fd, false)
}

pub(super) fn sys_dup2(kernel: &RuntimeKernel, a0: usize, a1: usize) -> Result<usize, &'static str> {
    let old_fd = a0;
    let new_fd = a1;
    // AGENT: validate both fd numbers against the fd limit, not N_PROC.
    if old_fd >= MAX_FD {
        return Err("ebadf");
    }
    if new_fd >= MAX_FD {
        return Err("ebadf");
    }
    if old_fd == new_fd {
        return Ok(new_fd);
    }
    let task = kernel.cur_task(0).ok_or("esrch")?;
    task.dup2_fd(old_fd, new_fd)
}

pub(super) fn sys_fcntl(
    kernel: &RuntimeKernel,
    a0: usize,
    a1: usize,
    a2: usize,
) -> Result<usize, &'static str> {
    let fd = a0;
    let cmd = a1;
    let arg = a2;
    // AGENT: fcntl operates on fd numbers, so use MAX_FD as the boundary.
    if fd >= MAX_FD {
        return Err("ebadf");
    }
    let task = kernel.cur_task(0).ok_or("esrch")?;
    match cmd {
        F_DUPFD => {
            if arg >= MAX_FD {
                return Err("einval");
            }
            let mut fds = task.process.files.lock().unwrap();
            let entry = fds.get(&fd).cloned().ok_or("ebadf")?;
            let new_fd = (arg..MAX_FD)
                .find(|candidate| !fds.contains_key(candidate))
                .ok_or("emfile")?;
            fds.insert(new_fd, entry.dup(false));
            Ok(new_fd)
        }
        F_DUPFD_CLOEXEC => {
            if arg >= MAX_FD {
                return Err("einval");
            }
            let mut fds = task.process.files.lock().unwrap();
            let entry = fds.get(&fd).cloned().ok_or("ebadf")?;
            let new_fd = (arg..MAX_FD)
                .find(|candidate| !fds.contains_key(candidate))
                .ok_or("emfile")?;
            fds.insert(new_fd, entry.dup(true));
            Ok(new_fd)
        }
        F_GETFD => {
            let cloexec = task.get_fd_entry(fd).ok_or("ebadf")?.is_cloexec();
            Ok(if cloexec { FD_CLOEXEC } else { 0 })
        }
        F_SETFD => {
            let _cloexec = (arg & FD_CLOEXEC) != 0;
            task.set_cloexec(fd, _cloexec)?;
            Ok(0)
        }
        F_GETFL => {
            let entry = task.get_fd_entry(fd).ok_or("ebadf")?;
            Ok(fdopt_to_open_flags(entry.status_flags()))
        }
        F_SETFL => {
            let valid_mask = O_NONBLOCK | O_APPEND;
            let _new_flags = arg & valid_mask;
            if arg & !valid_mask != 0 {
                return Err("einval");
            }
            let entry = task.get_fd_entry(fd).ok_or("ebadf")?;
            entry.set_status_flags(_new_flags)?;
            Ok(0)
        }
        F_GETLK => {
            if !check_access(arg, 32) {
                return Err("efault");
            }
            Ok(0)
        }
        F_SETLK | F_SETLKW => {
            if !check_access(arg, 32) {
                return Err("efault");
            }
            let _lock_type = arg & 0xF;
            Ok(0)
        }
        _ => Err("einval"),
    }
}
