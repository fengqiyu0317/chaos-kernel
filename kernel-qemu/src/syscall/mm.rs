// AGENT
use super::*;

// AGENT: validate anonymous or regular-file mmap arguments, retain positioned
// file backing independently from fd lifetime, and route fixed mappings through
// the matching transactional eager replacement path.
pub(super) fn sys_mmap(
    kernel: &Kernel,
    a0: usize,
    a1: usize,
    a2: usize,
    a3: usize,
    a4: usize,
    a5: usize,
) -> Result<usize, &'static str> {
    let addr = a0;
    let len = a1;
    let prot = a2;
    let flags = a3;
    let fd = a4;
    let offset = a5;
    if len == 0 {
        return Err("einval");
    }
    let aligned_len = checked_align_up(len, PAGE_SZ).ok_or("enomem")?;
    let known_prot = PROT_READ | PROT_WRITE | PROT_EXEC;
    if prot & !known_prot != 0 {
        return Err("einval");
    }
    let known_flags = MAP_SHARED | MAP_PRIVATE | MAP_FIXED | MAP_ANONYMOUS;
    if flags & !known_flags != 0 {
        return Err("einval");
    }
    let map_anon = (flags & MAP_ANONYMOUS) != 0;
    let map_fixed = (flags & MAP_FIXED) != 0;
    let map_shared = (flags & MAP_SHARED) != 0;
    let map_private = (flags & MAP_PRIVATE) != 0;
    if map_shared == map_private {
        return Err("einval");
    }
    if map_anon && offset != 0 {
        return Err("einval");
    }
    if !map_anon && (offset > i64::MAX as usize || offset % PAGE_SZ != 0) {
        return Err("einval");
    }
    let mut vm_flags: u32 = 0;
    if prot & PROT_READ != 0 {
        vm_flags |= VM_READ;
    }
    if prot & PROT_WRITE != 0 {
        vm_flags |= VM_WRITE;
    }
    if prot & PROT_EXEC != 0 {
        vm_flags |= VM_EXEC;
    }
    if map_shared {
        vm_flags |= VM_SHARED;
    }
    let task = kernel.cur_task(0).ok_or("esrch")?;
    let file_source = if map_anon {
        None
    } else {
        let file_end = offset.checked_add(aligned_len).ok_or("eoverflow")?;
        if file_end > i64::MAX as usize {
            return Err("eoverflow");
        }
        let entry = task.get_fd_entry(fd).ok_or("ebadf")?;
        let (source, status) = entry.mmap_source()?;
        if !status.rd || (map_shared && prot & PROT_WRITE != 0 && !status.wr) {
            return Err("eacces");
        }
        Some(source)
    };
    let mut addr_space = task.process.addr_space.lock().unwrap();
    let result_addr = if map_fixed {
        if addr == 0 || addr % PAGE_SZ != 0 {
            return Err("einval");
        }
        addr.checked_add(aligned_len).ok_or("enomem")?;
        addr
    } else if addr == 0 {
        addr_space
            .find_free_region(aligned_len, PAGE_SZ)
            .ok_or("enomem")?
    } else {
        addr_space
            .find_free_region_from(addr, aligned_len, PAGE_SZ)
            .or_else(|| addr_space.find_free_region(aligned_len, PAGE_SZ))
            .ok_or("enomem")?
    };
    let result_end = result_addr.checked_add(aligned_len).ok_or("enomem")?;
    if result_end > USER_SIGTRAMP {
        return Err("enomem");
    }
    if let Some(source) = file_source {
        let region = VmRegion::new_file(result_addr, aligned_len, vm_flags, source, offset);
        if map_fixed {
            addr_space.replace_file_region(region, &kernel.pool)?;
        } else {
            addr_space.map_file_region(region, &kernel.pool)?;
        }
    } else {
        let region = VmRegion::new(result_addr, aligned_len, vm_flags);
        if map_fixed {
            addr_space.replace_region(region, &kernel.pool)?;
        } else {
            addr_space.map_region(region, &kernel.pool)?;
        }
    }
    Ok(result_addr)
}

// AGENT: reject invalid munmap parameters before mutating address-space state,
// then propagate unmap/writeback failures from the address-space layer.
pub(super) fn sys_munmap(kernel: &Kernel, a0: usize, a1: usize) -> Result<usize, &'static str> {
    let addr = a0;
    let len = a1;
    if len == 0 || addr % PAGE_SZ != 0 {
        return Err("einval");
    }
    let aligned_len = checked_align_up(len, PAGE_SZ).ok_or("enomem")?;
    let end = addr.checked_add(aligned_len).ok_or("enomem")?;
    if end > USER_SIGTRAMP {
        return Err("enomem");
    }
    let task = kernel.cur_task(0).ok_or("esrch")?;
    task.process
        .addr_space
        .lock()
        .unwrap()
        .unmap_range(addr, aligned_len, &kernel.pool)?;
    Ok(0)
}

// AGENT: implement the raw Linux brk ABI over byte-granular address-space
// metadata: success returns the request and normal rejection returns old brk.
pub(super) fn sys_brk(kernel: &Kernel, new_brk: usize) -> Result<usize, &'static str> {
    let task = kernel.cur_task(0).ok_or("esrch")?;
    let mut addr_space = task.process.addr_space.lock().unwrap();
    let old_brk = addr_space.brk();
    if new_brk == 0 {
        return Ok(old_brk);
    }
    match addr_space.resize_brk(new_brk, &kernel.pool) {
        Ok(()) => Ok(new_brk),
        Err(BrkResizeError::Rejected) => Ok(old_brk),
        Err(BrkResizeError::Internal(err)) => Err(err),
    }
}
