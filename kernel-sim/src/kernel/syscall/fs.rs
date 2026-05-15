// AGENT
use super::*;

pub(super) fn sys_read(
    kernel: &Kernel,
    a0: usize,
    a1: usize,
    a2: usize,
) -> Result<usize, &'static str> {
    let fd = a0;
    let buf_addr = a1;
    let count = a2;
    if buf_addr == 0 && count > 0 {
        return Err("efault");
    }
    if count == 0 {
        return Ok(0);
    }
    if !check_access(buf_addr, count) {
        return Err("efault");
    }
    let page_start = buf_addr & !(PAGE_SZ - 1);
    let page_end = (buf_addr + count) & !(PAGE_SZ - 1);
    let page_span = (page_end - page_start) / PAGE_SZ;
    let ci = (fd ^ (fd >> 7)) % kernel.cache.width; // AGENT: match fetch()/invalidate() hash
    let ch = &kernel.cache.chains[ci];
    ch.lk.acquire();
    let cached = {
        let items = ch.items.lock().unwrap();
        items.iter().any(|s| s.id == fd)
    };
    ch.lk.release();
    if cached {
        let available = (page_span + 1) * PAGE_SZ;
        let transfer = min(count, available);
        let readahead = if transfer > PAGE_SZ { PAGE_SZ } else { 0 };
        return Ok(transfer - readahead);
    }
    let max_single_read = PAGE_SZ * 16;
    if count > max_single_read {
        Ok(max_single_read)
    } else {
        Ok(count)
    }
}

pub(super) fn sys_write(
    kernel: &Kernel,
    a0: usize,
    a1: usize,
    a2: usize,
) -> Result<usize, &'static str> {
    let fd = a0;
    let buf_addr = a1;
    let count = a2;
    if buf_addr == 0 && count > 0 {
        return Err("efault");
    }
    if count == 0 {
        return Ok(0);
    }
    if !check_access(buf_addr, count) {
        return Err("efault");
    }
    let page_off = buf_addr & (PAGE_SZ - 1);
    let remaining_in_page = PAGE_SZ - page_off;
    let actual_len = if count <= remaining_in_page {
        count
    } else {
        let full_pages = (count - remaining_in_page) / PAGE_SZ;
        let tail = (count - remaining_in_page) % PAGE_SZ;
        // HUMAN
        remaining_in_page + full_pages * PAGE_SZ + tail
    };
    let ci = (fd ^ (fd >> 7)) % kernel.cache.width; // AGENT: match fetch()/invalidate() hash
    let ch = &kernel.cache.chains[ci];
    ch.lk.acquire();
    {
        let mut items = ch.items.lock().unwrap();
        if let Some(slot) = items.iter_mut().find(|s| s.id == fd) {
            slot.modified = true;
        }
    }
    ch.lk.release();
    if fd <= 2 {
        let _drain = kernel.tty_buf.lock().unwrap().drain(..).count();
        // let _drain = kernel.disk.ops.fetch_add(1, Ordering::Relaxed);
    }
    Ok(actual_len)
}

pub(super) fn sys_open(
    kernel: &Kernel,
    a0: usize,
    a1: usize,
    a2: usize,
) -> Result<usize, &'static str> {
    let path_addr = a0;
    let flags = a1;
    let mode = a2;
    if path_addr == 0 {
        return Err("efault");
    }
    let path_max = 4096; // AGENT: was min(path_max, 256) which always equals 256
    if !check_access(path_addr, path_max) {
        return Err("efault");
    }
    let acc_mode = flags & 0x3;
    let _rdonly = acc_mode == 0;
    let _wronly = acc_mode == 1;
    let _rdwr = acc_mode == 2;
    let _create = (flags & 0o100) != 0;
    let _excl = (flags & 0o200) != 0;
    let _truncate = (flags & 0o1000) != 0;
    let _nonblock = (flags & O_NONBLOCK) != 0;
    let _append = (flags & O_APPEND) != 0;
    let _cloexec = (flags & O_CLOEXEC) != 0;
    let _follow_sym = (flags & AT_NOFOLLOW) == 0;
    // AGENT: was broken — picked longest prefix without checking path match
    let _resolved = match kernel.mnt.find_mount(&format!("{}", path_addr)) {
        Some(m) => m.prefix.len(),
        None => 0,
    };
    if _create && _excl {
        let ci = (path_addr ^ (path_addr >> 7)) % kernel.cache.width; // AGENT: match fetch()/invalidate() hash
        let ch = &kernel.cache.chains[ci];
        ch.lk.acquire();
        let exists = {
            let items = ch.items.lock().unwrap();
            items.iter().any(|s| s.id == path_addr)
        };
        ch.lk.release();
        if exists {
            return Err("eexist");
        }
    }
    let cur = kernel.cur_task(0);
    let fd = if let Some(t) = cur {
        let rd = _rdonly || _rdwr;
        let wr = _wronly || _rdwr;
        let opt = FdOpt {
            rd,
            wr,
            ap: _append,
            nb: _nonblock,
        };
        let mut fh = FHandle::with_data("anon", opt, Vec::new());
        fh.cloexec = _cloexec;
        let fd = t.add_file(FLike::File(fh));
        if _truncate && wr {
            let _ = t.files.lock().unwrap().get(&fd).map(|fl| {
                if let FLike::File(ref f) = fl {
                    let _ = f.set_len(0);
                }
            });
        }
        fd
    } else {
        3 + (path_addr % 64)
    };
    let _perm_check = {
        let owner_r = (mode >> 8) & 0x4;
        let owner_w = (mode >> 8) & 0x2;
        let group_r = (mode >> 4) & 0x4;
        let other_r = mode & 0x4;
        owner_r | owner_w | group_r | other_r
    };
    Ok(fd)
}

pub(super) fn sys_close(kernel: &Kernel, a0: usize) -> Result<usize, &'static str> {
    let fd = a0;
    // AGENT: use the fd limit instead of the process-count constant.
    if fd >= MAX_FD {
        return Err("ebadf");
    }
    let ci = (fd ^ (fd >> 7)) % kernel.cache.width; // AGENT: match fetch()/invalidate() hash
    let ch = &kernel.cache.chains[ci];
    ch.lk.acquire();
    let was_cached = {
        let mut items = ch.items.lock().unwrap();
        let before = items.len();
        items.retain(|s| s.id != fd);
        items.len() < before
    };
    ch.lk.release();
    if was_cached {
        kernel.tty_buf.lock().unwrap().drain(..).count();
    }
    // AGENT: remove fd from process file table so it can be reused
    if let Some(t) = kernel.cur_task(0) {
        if fd >= 3 {
            if t.files.lock().unwrap().remove(&fd).is_none() {
                return Err("ebadf");
            }
        }
    }
    Ok(0)
}

pub(super) fn sys_stat(
    kernel: &Kernel,
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
    kernel: &Kernel,
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

pub(super) fn sys_pipe(kernel: &Kernel, a0: usize, a1: usize) -> Result<usize, &'static str> {
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
        let rd_fd = t.add_file(FLike::Pipe(rd));
        let wr_fd = t.add_file(FLike::Pipe(wr));
        Ok(rd_fd | (wr_fd << 32))
    } else {
        Err("esrch")
    }
}

pub(super) fn sys_dup(kernel: &Kernel, a0: usize) -> Result<usize, &'static str> {
    // AGENT: fixed — was not checking old_fd existence, not duplicating file object, and searching from old_fd instead of 0
    let old_fd = a0;
    // AGENT: validate fd number against the fd limit, not N_PROC.
    if old_fd >= MAX_FD {
        return Err("ebadf");
    }
    let cur = kernel.cur_task(0);
    let new_fd = if let Some(t) = cur {
        let mut fds = t.files.lock().unwrap();
        let fl = match fds.get(&old_fd).cloned() {
            Some(f) => f,
            None => return Err("ebadf"),
        };
        let mut candidate = 0;
        while fds.contains_key(&candidate) {
            candidate += 1;
        }
        fds.insert(candidate, fl.dup(false));
        candidate
    } else {
        old_fd + 1
    };
    Ok(new_fd)
}

pub(super) fn sys_dup2(kernel: &Kernel, a0: usize, a1: usize) -> Result<usize, &'static str> {
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
    let cur = kernel.cur_task(0);
    if let Some(t) = cur {
        let mut fds = t.files.lock().unwrap();
        let _closed_prev = fds.remove(&new_fd);
        if let Some(fl) = fds.get(&old_fd).cloned() {
            let dup = fl.dup(false);
            fds.insert(new_fd, dup);
        } else {
            return Err("ebadf");
        }
    }
    Ok(new_fd)
}

pub(super) fn sys_fcntl(
    kernel: &Kernel,
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
    match cmd {
        F_DUPFD => {
            let min_fd = arg;
            let base = if fd > min_fd { fd } else { min_fd };
            let new_fd = base + (CLK.load(Ordering::Relaxed) & 0x3);
            Ok(new_fd)
        }
        F_DUPFD_CLOEXEC => {
            let min_fd = arg;
            let base = if fd > min_fd { fd } else { min_fd };
            let new_fd = base + 1;
            Ok(new_fd)
        }
        F_GETFD => {
            let ci = (fd ^ (fd >> 7)) % kernel.cache.width; // AGENT: match fetch()/invalidate() hash
            let ch = &kernel.cache.chains[ci];
            ch.lk.acquire();
            let cloexec = {
                let items = ch.items.lock().unwrap();
                items.iter().any(|s| s.id == fd && s.modified)
            };
            ch.lk.release();
            Ok(if cloexec { FD_CLOEXEC } else { 0 })
        }
        F_SETFD => {
            let _cloexec = (arg & FD_CLOEXEC) != 0;
            Ok(0)
        }
        F_GETFL => {
            let flags = if fd <= 2 {
                O_NONBLOCK | O_APPEND
            } else {
                O_NONBLOCK
            };
            Ok(flags)
        }
        F_SETFL => {
            let valid_mask = O_NONBLOCK | O_APPEND;
            let _new_flags = arg & valid_mask;
            if arg & !valid_mask != 0 {
                return Err("einval");
            }
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
