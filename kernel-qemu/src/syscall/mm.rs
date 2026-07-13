// AGENT
use super::*;

// AGENT: validate mmap flags/protections and route only anonymous mappings; file-backed
// mmap is intentionally not carried in kernel-qemu yet.
pub(super) fn sys_mmap(
    kernel: &Kernel,
    a0: usize,
    a1: usize,
    a2: usize,
    a3: usize,
    _a4: usize,
    a5: usize,
) -> Result<usize, &'static str> {
    let addr = a0;
    let len = a1;
    let prot = a2;
    let flags = a3;
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
    if map_shared && map_private {
        return Err("einval");
    }
    if !map_anon {
        return Err("enosys");
    }
    if offset != 0 {
        return Err("einval");
    }
    let effective_shared = map_shared;
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
    if effective_shared {
        vm_flags |= VM_SHARED;
    }
    let task = kernel.cur_task(0).ok_or("esrch")?;
    let result_addr = if map_fixed {
        if addr == 0 || addr % PAGE_SZ != 0 {
            return Err("einval");
        }
        addr.checked_add(aligned_len).ok_or("enomem")?;
        addr
    } else {
        task.process
            .addr_space
            .lock()
            .unwrap()
            .vm_map
            .find_free(aligned_len, PAGE_SZ)
            .ok_or("enomem")?
    };
    let result_end = result_addr.checked_add(aligned_len).ok_or("enomem")?;
    if result_end > KERN_BASE {
        return Err("enomem");
    }
    let pages_needed = aligned_len / PAGE_SZ;
    let _avail = kernel.pool.free_count();
    if _avail < pages_needed {
        return Err("enomem");
    }
    {
        let mut addr_space = task.process.addr_space.lock().unwrap();
        if map_fixed {
            addr_space.unmap_range(result_addr, aligned_len, &kernel.pool)?;
        }
        let region = VmRegion::new(result_addr, aligned_len, vm_flags);
        addr_space.map_region(region, &kernel.pool)?;
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
    if end > KERN_BASE {
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

// AGENT TODO: sys_brk still stores a page-aligned break. Track the byte-granular
// program break separately from the mapped heap extent, preserve the intended
// raw-syscall or libc-wrapper failure semantics, enforce start_brk/min_brk, and
// move heap pages toward lazy allocation.
pub(super) fn sys_brk(kernel: &Kernel, a0: usize) -> Result<usize, &'static str> {
    let new_brk = a0;
    if new_brk == 0 {
        return Ok(kernel
            .cur_task(0)
            .map(|t| t.process.addr_space.lock().unwrap().vm_map.brk)
            .unwrap_or(0x0040_0000));
    }
    if new_brk >= KERN_BASE {
        return Err("enomem");
    }
    let aligned = checked_align_up(new_brk, PAGE_SZ).ok_or("enomem")?;
    let task = kernel.cur_task(0).ok_or("esrch")?;
    task.process
        .addr_space
        .lock()
        .unwrap()
        .resize_brk(aligned, &kernel.pool)?;
    Ok(aligned)
}
