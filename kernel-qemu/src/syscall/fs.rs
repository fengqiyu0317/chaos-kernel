// AGENT
use super::*;

const MAX_RW_COUNT: usize = PAGE_SZ * 16;
const O_ACCMODE: usize = 0x3;
const SUPPORTED_OPEN_FLAGS: usize =
    O_ACCMODE | O_CREAT | O_EXCL | O_TRUNC | O_NONBLOCK | O_APPEND | O_CLOEXEC;
// AGENT: support the two ordinary Linux pipe2 creation flags while rejecting
// packet-mode and notification-pipe behavior that this pipe layer lacks.
const SUPPORTED_PIPE_FLAGS: usize = O_NONBLOCK | O_CLOEXEC;

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
pub(super) fn read_user_path(task: &Task, addr: usize) -> Result<String, &'static str> {
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

// AGENT: copy one native RV64 off_t through the live address space without
// directly dereferencing a userspace pointer.
fn read_user_i64(task: &Task, addr: usize) -> Result<i64, &'static str> {
    let mut bytes = [0u8; mem::size_of::<i64>()];
    task.process
        .addr_space
        .lock()
        .unwrap()
        .read_user_bytes(addr, &mut bytes)?;
    Ok(i64::from_ne_bytes(bytes))
}

// AGENT: write one updated native RV64 off_t through AddrSpace so COW and page
// permissions follow the same usercopy path as other filesystem syscalls.
fn write_user_i64(
    kernel: &Kernel,
    task: &Task,
    addr: usize,
    value: i64,
) -> Result<(), &'static str> {
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
    let nread = entry.read(task.id(), &mut tmp)?;
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
    match entry.write(task.id(), &tmp)? {
        FdWriteOutcome::Written(n) => Ok(n),
        FdWriteOutcome::BrokenPipe { written } => {
            kernel.send_signal_to_task(&task, SIGPIPE as i32, 0);
            if written == 0 {
                Err("epipe")
            } else {
                Ok(written)
            }
        }
    }
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

// AGENT: keep descriptor/OFD ioctls at this boundary, use authoritative
// usercopy for integer arguments, and reject unmigrated device requests.
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
            entry.set_nonblocking(nonblock);
            Ok(0)
        }
        // TIOCINQ is the same numeric request as FIONREAD.
        FIONREAD => {
            let readable = entry.io_ctl(FIONREAD)?;
            let readable = i32::try_from(readable).map_err(|_| "eoverflow")?;
            write_user_i32(kernel, &task, arg, readable)?;
            Ok(0)
        }
        _ => Err("enotty"),
    }
}

// AGENT: implement Linux RV64 pipe2 by copying int[2] to the active address
// space before atomically publishing two fully initialized fd entries.
pub(super) fn sys_pipe(kernel: &Kernel, a0: usize, a1: usize) -> Result<usize, &'static str> {
    let fds_addr = a0;
    let pipe_flags = a1;
    if pipe_flags & !SUPPORTED_PIPE_FLAGS != 0 {
        return Err("einval");
    }
    if fds_addr == 0 {
        return Err("efault");
    }
    let task = kernel.cur_task(0).ok_or("esrch")?;
    let output_len = 2 * mem::size_of::<i32>();
    let writable_len = task
        .process
        .addr_space
        .lock()
        .unwrap()
        .writable_user_prefix_len(fds_addr, output_len)?;
    if writable_len != output_len {
        return Err("efault");
    }

    let nonblock = pipe_flags & O_NONBLOCK != 0;
    let cloexec = pipe_flags & O_CLOEXEC != 0;
    let (read_end, write_end) = PipeNode::pair();
    let read_entry = FdEntry::with_status(
        FLike::Pipe(read_end),
        FdOpt {
            rd: true,
            wr: false,
            ap: false,
            nb: nonblock,
        },
        cloexec,
    );
    let write_entry = FdEntry::with_status(
        FLike::Pipe(write_end),
        FdOpt {
            rd: false,
            wr: true,
            ap: false,
            nb: nonblock,
        },
        cloexec,
    );

    task.add_file_pair_transaction(read_entry, write_entry, |read_fd, write_fd| {
        let read_fd = i32::try_from(read_fd).map_err(|_| "eoverflow")?;
        let write_fd = i32::try_from(write_fd).map_err(|_| "eoverflow")?;
        let fd_size = mem::size_of::<i32>();
        let mut output = [0u8; 2 * mem::size_of::<i32>()];
        output[..fd_size].copy_from_slice(&read_fd.to_ne_bytes());
        output[fd_size..].copy_from_slice(&write_fd.to_ne_bytes());
        task.process
            .addr_space
            .lock()
            .unwrap()
            .write_user_bytes(fds_addr, &output, &kernel.pool)
    })?;
    Ok(0)
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

// AGENT: implement Linux dup3 flags and same-fd rules before delegating exact
// target replacement to the authoritative per-process fd table.
pub(super) fn sys_dup3(
    kernel: &Kernel,
    a0: usize,
    a1: usize,
    a2: usize,
) -> Result<usize, &'static str> {
    let old_fd = a0;
    let new_fd = a1;
    let flags = a2;
    if flags & !O_CLOEXEC != 0 || old_fd == new_fd {
        return Err("einval");
    }
    if old_fd >= MAX_FD || new_fd >= MAX_FD {
        return Err("ebadf");
    }
    let task = kernel.cur_task(0).ok_or("esrch")?;
    task.dup3_fd(old_fd, new_fd, flags & O_CLOEXEC != 0)
}

// AGENT: adapt Linux splice(2) arguments into OFD/pipe semantics while keeping
// user off_t copy-in/copy-out and SIGPIPE generation at the syscall boundary.
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
    let off_in_addr = a1;
    let fd_out = a2;
    let off_out_addr = a3;
    let size = a4;
    let flags = a5;

    // AGENT: Linux returns success for a zero-length splice before inspecting
    // flags, descriptors, or user offset pointers.
    if size == 0 {
        return Ok(0);
    }
    if flags & !SPLICE_KNOWN_FLAGS != 0 {
        return Err("einval");
    }

    let task = kernel.cur_task(0).ok_or("esrch")?;
    let in_entry = task.get_fd_entry(fd_in).ok_or("ebadf")?;
    let out_entry = task.get_fd_entry(fd_out).ok_or("ebadf")?;
    in_entry.validate_splice_offset_args(&out_entry, off_in_addr != 0, off_out_addr != 0)?;

    let mut offsets = SpliceOffsets {
        input: if off_in_addr == 0 {
            None
        } else {
            Some(read_user_i64(&task, off_in_addr)?)
        },
        output: if off_out_addr == 0 {
            None
        } else {
            Some(read_user_i64(&task, off_out_addr)?)
        },
    };

    let outcome = in_entry.splice_to(
        &out_entry,
        task.id(),
        &mut offsets,
        min(size, MAX_RW_COUNT),
        flags,
    )?;
    let moved = match outcome {
        SpliceOutcome::Moved(moved) => moved,
        SpliceOutcome::BrokenPipe { moved } => {
            kernel.send_signal_to_task(&task, SIGPIPE as i32, 0);
            if moved == 0 {
                return Err("epipe");
            }
            moved
        }
    };

    // AGENT: copy output then input positions after a successful operation,
    // matching Linux; a late EFAULT does not roll back already-moved data.
    if let Some(offset) = offsets.output {
        write_user_i64(kernel, &task, off_out_addr, offset)?;
    }
    if let Some(offset) = offsets.input {
        write_user_i64(kernel, &task, off_in_addr, offset)?;
    }

    Ok(moved)
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
