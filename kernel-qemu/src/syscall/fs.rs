// AGENT
use super::*;

const MAX_RW_COUNT: usize = PAGE_SZ * 16;
const O_ACCMODE: usize = 0x3;
const SUPPORTED_OPEN_FLAGS: usize =
    O_ACCMODE | O_CREAT | O_EXCL | O_TRUNC | O_NONBLOCK | O_APPEND | O_CLOEXEC;

// AGENT: keep the first-stage supported open flags explicit instead of
// accepting unimplemented path, durability, or directory semantics silently.
#[derive(Clone, Copy)]
struct OpenOptions {
    status: FdOpt,
    creation: CreateDisposition,
    truncate: bool,
    cloexec: bool,
}

// AGENT: decode immutable access mode separately from mutable OFD status flags.
impl OpenOptions {
    // AGENT: reject unsupported flag bits at the syscall semantic boundary.
    fn parse(flags: usize) -> Result<Self, &'static str> {
        let (rd, wr) = match flags & O_ACCMODE {
            0 => (true, false),
            1 => (false, true),
            2 => (true, true),
            _ => return Err("einval"),
        };
        if flags & !SUPPORTED_OPEN_FLAGS != 0 {
            return Err("enotsup");
        }
        let creation = match (flags & O_CREAT != 0, flags & O_EXCL != 0) {
            (false, false) => CreateDisposition::OpenExisting,
            (true, false) => CreateDisposition::CreateIfMissing,
            (true, true) => CreateDisposition::CreateNew,
            (false, true) => return Err("einval"),
        };
        Ok(Self {
            status: FdOpt {
                rd,
                wr,
                ap: flags & O_APPEND != 0,
                nb: flags & O_NONBLOCK != 0,
            },
            creation,
            truncate: flags & O_TRUNC != 0,
            cloexec: flags & O_CLOEXEC != 0,
        })
    }
}

// AGENT: read a NUL-terminated path from the current user address space.
fn read_user_path(task: &Task, addr: usize) -> Result<String, &'static str> {
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

// AGENT: resolve source and filesystem type to one registered live FsInstance,
// then attach that shared instance without implying any on-disk recovery.
pub(super) fn sys_mount(
    kernel: &Kernel,
    source_addr: usize,
    target_addr: usize,
    filesystem_type_addr: usize,
    mount_flags: usize,
    data_addr: usize,
) -> Result<usize, &'static str> {
    if mount_flags != 0 || data_addr != 0 {
        return Err("enotsup");
    }
    if filesystem_type_addr == 0 {
        return Err("einval");
    }

    let task = kernel.cur_task(0).ok_or("esrch")?;
    let source = read_user_path(&task, source_addr)?;
    let target = read_user_path(&task, target_addr)?;
    let filesystem_type = read_user_path(&task, filesystem_type_addr)?;
    if source.is_empty() || filesystem_type.is_empty() {
        return Err("einval");
    }

    let kind = FsKind::from_name(&filesystem_type)?;
    kernel
        .vfs
        .mount_source(&source, &target, kind, MountFlags::empty())?;
    Ok(0)
}

// AGENT: map Linux's zero and MNT_DETACH flag sets onto explicit mount lifecycle
// modes while continuing to reject force, expire, no-follow, and unknown bits.
pub(super) fn sys_umount2(
    kernel: &Kernel,
    target_addr: usize,
    flags: usize,
) -> Result<usize, &'static str> {
    let mode = match flags {
        0 => UnmountMode::Normal,
        MNT_DETACH => UnmountMode::Lazy,
        _ => return Err("enotsup"),
    };
    let task = kernel.cur_task(0).ok_or("esrch")?;
    let target = read_user_path(&task, target_addr)?;
    kernel.vfs.unmount(&target, mode)?;
    Ok(0)
}

// AGENT: ioctl integer arguments live in user memory; copy them through the
// active address space instead of trusting the raw pointer.
fn read_user_i32(task: &Task, addr: usize) -> Result<i32, &'static str> {
    if addr == 0 {
        return Err("efault");
    }
    let mut bytes = [0u8; mem::size_of::<i32>()];
    task.process
        .addr_space
        .lock()
        .unwrap()
        .read_user_bytes(addr, &mut bytes)?;
    Ok(i32::from_ne_bytes(bytes))
}

// AGENT: write ioctl integer results through AddrSpace so bad pointers report
// efault rather than corrupting kernel memory.
fn write_user_i32(
    kernel: &Kernel,
    task: &Task,
    addr: usize,
    value: i32,
) -> Result<(), &'static str> {
    if addr == 0 {
        return Err("efault");
    }
    task.process.addr_space.lock().unwrap().write_user_bytes(
        addr,
        &value.to_ne_bytes(),
        &kernel.pool,
    )
}

// AGENT: preflight a bounded writable userspace prefix, read through the current
// task's shared open-file description, then copy the returned bytes to userspace.
pub(super) fn sys_read(
    kernel: &Kernel,
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
    if nread > tmp.len() {
        return Err("eio");
    }
    if nread != 0 {
        task.process.addr_space.lock().unwrap().write_user_bytes(
            buf_addr,
            &tmp[..nread],
            &kernel.pool,
        )?;
    }
    Ok(nread)
}

// AGENT: copy a bounded readable userspace prefix into kernel memory before
// dispatching through the current task's shared open-file description.
pub(super) fn sys_write(
    kernel: &Kernel,
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
    if readable_len != 0 {
        task.process
            .addr_space
            .lock()
            .unwrap()
            .read_user_bytes(buf_addr, &mut tmp)?;
    }
    let entry = task.get_fd_entry(fd).ok_or("ebadf")?;
    entry.write(&tmp)
}

// AGENT: install one absolute path through a descriptor reservation so EMFILE
// is reported before path creation or truncation can change global state.
fn do_open(
    kernel: &Kernel,
    task: &Task,
    path: &str,
    flags: usize,
    _mode: usize,
) -> Result<usize, &'static str> {
    if path.is_empty() {
        return Err("enoent");
    }
    if !path.starts_with('/') {
        return Err("enotsup");
    }
    let options = OpenOptions::parse(flags)?;
    task.add_file_with_status_from(options.status, options.cloexec, || {
        let instance = kernel.open_regular_node(path, options.creation)?.path_ref;
        if options.truncate && options.status.wr {
            instance.set_len(0)?;
        }
        Ok(FLike::File(FHandle::new(instance)))
    })
}

// AGENT: expose the real RISC-V openat ABI while supporting only absolute paths
// until per-process cwd and directory-fd traversal migrate as one coherent layer.
pub(super) fn sys_openat(
    kernel: &Kernel,
    _dirfd: usize,
    path_addr: usize,
    flags: usize,
    mode: usize,
) -> Result<usize, &'static str> {
    let task = kernel.cur_task(0).ok_or("esrch")?;
    let path = read_user_path(&task, path_addr)?;
    do_open(kernel, &task, &path, flags, mode)
}

// AGENT: expose Linux RISC-V mkdirat while the first pathname stage supports
// absolute paths only; mode is accepted but permission metadata is not modeled.
pub(super) fn sys_mkdirat(
    kernel: &Kernel,
    _dirfd: usize,
    path_addr: usize,
    _mode: usize,
) -> Result<usize, &'static str> {
    let task = kernel.cur_task(0).ok_or("esrch")?;
    let path = read_user_path(&task, path_addr)?;
    if path.is_empty() {
        return Err("enoent");
    }
    if !path.starts_with('/') {
        return Err("enotsup");
    }
    kernel.create_directory(&path)?;
    Ok(0)
}

pub(super) fn sys_close(kernel: &Kernel, a0: usize) -> Result<usize, &'static str> {
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
        kernel.vfs.mounts.mount_count()
    } else {
        let fd = a0;
        fd / 4
    };
    Ok(0)
}

// AGENT: validate the fd once, keep descriptor-wide ioctls here, and delegate
// object-specific queries such as pipe FIONREAD through FdEntry::io_ctl.
pub(super) fn sys_ioctl(
    kernel: &Kernel,
    a0: usize,
    a1: usize,
    a2: usize,
) -> Result<usize, &'static str> {
    let fd = a0;
    let cmd = a1;
    let arg = a2;
    if fd >= MAX_FD {
        return Err("ebadf");
    }
    let task = kernel.cur_task(0).ok_or("esrch")?;
    let entry = task.get_fd_entry(fd).ok_or("ebadf")?;
    match cmd {
        TCGETS => {
            if !check_access(arg, mem::size_of::<TrmIO>()) {
                return Err("efault");
            }
            Ok(0)
        }
        TCSETS => {
            if !check_access(arg, mem::size_of::<TrmIO>()) {
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
            if !check_access(arg, mem::size_of::<WinSz>()) {
                return Err("efault");
            }
            Ok(0)
        }
        FIONCLEX => {
            task.set_cloexec(fd, false)?;
            Ok(0)
        }
        FIOCLEX => {
            task.set_cloexec(fd, true)?;
            Ok(0)
        }
        FIONBIO => {
            let nonblock = read_user_i32(&task, arg)? != 0;
            let mut flags = entry.status_flags_bits();
            if nonblock {
                flags |= O_NONBLOCK;
            } else {
                flags &= !O_NONBLOCK;
            }
            entry.set_status_flags(flags)?;
            Ok(0)
        }
        FIONREAD | TIOCINQ => {
            let readable = entry.io_ctl(cmd)?;
            let readable = i32::try_from(readable).map_err(|_| "eoverflow")?;
            write_user_i32(kernel, &task, arg, readable)?;
            Ok(0)
        }
        _ => Err("enotty"),
    }
}

// AGENT: allocate pipe endpoints through one fd allocator transaction.
pub(super) fn sys_pipe(kernel: &Kernel, a0: usize, a1: usize) -> Result<usize, &'static str> {
    let fds_addr = a0;
    let pipe_flags = a1;
    if fds_addr == 0 {
        return Err("efault");
    }
    if !check_access(fds_addr, 2 * mem::size_of::<i32>()) {
        return Err("efault");
    }
    let cur = kernel.cur_task(0);
    if let Some(t) = cur {
        let (rd, wr) = PipeNode::pair();
        let _nonblock = (pipe_flags & O_NONBLOCK) != 0;
        let _cloexec = (pipe_flags & O_CLOEXEC) != 0;
        let (rd_fd, wr_fd) =
            t.add_file_pair_with_cloexec(FLike::Pipe(rd), FLike::Pipe(wr), _cloexec)?;
        if _nonblock {
            for pipe_fd in [rd_fd, wr_fd] {
                t.get_fd_entry(pipe_fd)
                    .ok_or("ebadf")?
                    .set_status_flags(O_NONBLOCK)?;
            }
        }
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
    let task = kernel.cur_task(0).ok_or("esrch")?;
    task.dup_fd(old_fd, false)
}

pub(super) fn sys_dup2(kernel: &Kernel, a0: usize, a1: usize) -> Result<usize, &'static str> {
    let old_fd = a0;
    let new_fd = a1;
    // AGENT: validate both fd numbers against the fd limit, not N_PROC.
    if old_fd >= MAX_FD || new_fd >= MAX_FD {
        return Err("ebadf");
    }
    let task = kernel.cur_task(0).ok_or("esrch")?;
    task.dup2_fd(old_fd, new_fd)
}

// AGENT: TODO(M9-splice): implement Linux splice(2) through real pipe buffers,
// user off_t pointer copy-in/copy-out, blocking/nonblocking waits, and pipe/file
// direction checks. This stub only reserves the syscall surface and rejects
// unsupported nonzero transfers instead of reusing the old file-to-file helper.
pub(super) fn sys_splice(
    kernel: &Kernel,
    a0: usize,
    a1: usize,
    a2: usize,
    a3: usize,
    a4: usize,
    a5: usize,
) -> Result<usize, &'static str> {
    let fd_in = a0;
    let _off_in_addr = a1;
    let fd_out = a2;
    let _off_out_addr = a3;
    let size = a4;
    let flags = a5;

    if fd_in >= MAX_FD || fd_out >= MAX_FD {
        return Err("ebadf");
    }
    if flags & !SPLICE_KNOWN_FLAGS != 0 {
        return Err("einval");
    }

    let task = kernel.cur_task(0).ok_or("esrch")?;
    let _in_entry = task.get_fd_entry(fd_in).ok_or("ebadf")?;
    let _out_entry = task.get_fd_entry(fd_out).ok_or("ebadf")?;
    if size == 0 {
        return Ok(0);
    }

    Err("enosys")
}

// AGENT: fcntl mutates fd entries while keeping access mode fixed in the
// shared open-file description.
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
    let task = kernel.cur_task(0).ok_or("esrch")?;
    match cmd {
        F_DUPFD => {
            if arg >= MAX_FD {
                return Err("einval");
            }
            task.dup_fd_from(fd, arg, false)
        }
        F_DUPFD_CLOEXEC => {
            if arg >= MAX_FD {
                return Err("einval");
            }
            task.dup_fd_from(fd, arg, true)
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
            let valid_mask = O_NONBLOCK | O_APPEND | 0x3;
            if arg & !valid_mask != 0 {
                return Err("einval");
            }
            let entry = task.get_fd_entry(fd).ok_or("ebadf")?;
            entry.set_status_flags(arg)?;
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
