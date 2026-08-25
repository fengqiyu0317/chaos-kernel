// AGENT: translate ABI-neutral filesystem attributes into the Linux
// asm-generic RV64 stat layout and copy them through the live user address space.
use super::*;

pub(crate) const RISCV64_STAT_SIZE: usize = 128;

// AGENT: write one little-endian u32 at a fixed, compile-time-audited ABI offset.
fn put_u32(out: &mut [u8; RISCV64_STAT_SIZE], offset: usize, value: u32) {
    out[offset..offset + mem::size_of::<u32>()].copy_from_slice(&value.to_le_bytes());
}

// AGENT: write one little-endian u64 at a fixed, compile-time-audited ABI offset.
fn put_u64(out: &mut [u8; RISCV64_STAT_SIZE], offset: usize, value: u64) {
    out[offset..offset + mem::size_of::<u64>()].copy_from_slice(&value.to_le_bytes());
}

// AGENT: preserve signed Linux stat fields without relying on a Rust repr(C)
// cast that could expose implicit padding bytes to userspace.
fn put_i64(out: &mut [u8; RISCV64_STAT_SIZE], offset: usize, value: i64) {
    out[offset..offset + mem::size_of::<i64>()].copy_from_slice(&value.to_le_bytes());
}

// AGENT: encode exactly the 128-byte Linux asm-generic stat structure used by
// RV64, leaving every explicit padding and unused field deterministically zero.
fn encode_riscv64_stat(attr: FileAttr) -> Result<[u8; RISCV64_STAT_SIZE], &'static str> {
    let mut out = [0u8; RISCV64_STAT_SIZE];
    put_u64(&mut out, 0, attr.dev);
    put_u64(&mut out, 8, attr.ino);
    put_u32(&mut out, 16, attr.mode);
    put_u32(&mut out, 20, attr.nlink);
    put_u32(&mut out, 24, attr.uid);
    put_u32(&mut out, 28, attr.gid);
    put_u64(&mut out, 32, attr.rdev);
    put_i64(
        &mut out,
        48,
        i64::try_from(attr.size).map_err(|_| "eoverflow")?,
    );
    put_u32(&mut out, 56, attr.block_size);
    put_i64(
        &mut out,
        64,
        i64::try_from(attr.blocks).map_err(|_| "eoverflow")?,
    );
    put_i64(&mut out, 72, attr.atime.sec);
    put_u64(&mut out, 80, attr.atime.nsec);
    put_i64(&mut out, 88, attr.mtime.sec);
    put_u64(&mut out, 96, attr.mtime.nsec);
    put_i64(&mut out, 104, attr.ctime.sec);
    put_u64(&mut out, 112, attr.ctime.nsec);
    Ok(out)
}

// AGENT: preflight the complete output range before resolving COW and copying
// bytes so partial mappings fail without exposing a truncated stat structure.
fn copy_stat_to_user(
    kernel: &Kernel,
    task: &Task,
    stat_addr: usize,
    attr: FileAttr,
) -> Result<(), &'static str> {
    if stat_addr == 0 {
        return Err("efault");
    }
    let bytes = encode_riscv64_stat(attr)?;
    let mut addr_space = task.process.addr_space.lock().unwrap();
    if addr_space.writable_user_prefix_len(stat_addr, bytes.len(), &kernel.pool)? != bytes.len() {
        return Err("efault");
    }
    addr_space.write_user_bytes(stat_addr, &bytes, &kernel.pool)
}

// AGENT: resolve one process descriptor through its shared open-file
// description, snapshot object metadata, and copy the RV64 result to userspace.
pub(super) fn sys_fstat(
    kernel: &Kernel,
    fd: usize,
    stat_addr: usize,
) -> Result<usize, &'static str> {
    let task = kernel.cur_task(0).ok_or("esrch")?;
    let entry = task.get_fd_entry(fd).ok_or("ebadf")?;
    let attr = entry.file_attr()?;
    copy_stat_to_user(kernel, &task, stat_addr, attr)?;
    Ok(0)
}

// AGENT: implement the honest absolute-path subset of Linux RV64 newfstatat;
// cwd/dirfd traversal and AT_* flag semantics remain rejected until migrated.
pub(super) fn sys_newfstatat(
    kernel: &Kernel,
    _dirfd: usize,
    path_addr: usize,
    stat_addr: usize,
    flags: usize,
) -> Result<usize, &'static str> {
    if flags != 0 {
        return Err("enotsup");
    }
    let task = kernel.cur_task(0).ok_or("esrch")?;
    let path = super::fs::read_user_path(kernel, &task, path_addr)?;
    if path.is_empty() {
        return Err("enoent");
    }
    if !path.starts_with('/') {
        return Err("enotsup");
    }
    let attr = kernel.vfs.resolve(&path)?.path_ref.file_attr()?;
    copy_stat_to_user(kernel, &task, stat_addr, attr)?;
    Ok(0)
}
