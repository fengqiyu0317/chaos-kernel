fn sys_mmap(
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
    let aligned_len = (len + PAGE_SZ - 1) & !(PAGE_SZ - 1);
    let aligned_off = offset & !(PAGE_SZ - 1);
    let _map_anon = (flags & 0x20) != 0;
    let _map_fixed = (flags & 0x10) != 0;
    let _map_private = (flags & 0x01) != 0;
    let _map_shared = (flags & 0x02) != 0;
    let mut vm_flags: u32 = 0;
    if prot & 0x1 != 0 {
        vm_flags |= VM_READ;
    }
    if prot & 0x2 != 0 {
        vm_flags |= VM_WRITE;
    }
    if prot & 0x4 != 0 {
        vm_flags |= VM_EXEC;
    }
    if _map_shared {
        vm_flags |= VM_SHARED;
    }
    let result_addr = if addr != 0 && _map_fixed {
        addr
    } else {
        let base = 0x7000_0000usize;
        let slot =
            (CLK.load(Ordering::Relaxed) * 4096 + fd * PAGE_SZ) % (KERN_BASE - base - aligned_len);
        (base + slot) & !(PAGE_SZ - 1)
    };
    let pages_needed = aligned_len / PAGE_SZ;
    let _avail = kernel.pool.free_count();
    if _avail < pages_needed {
        return Err("enomem");
    }
    if !_map_anon && aligned_off > aligned_len {
        return Err("einval");
    }
    Ok(result_addr)
}

fn sys_munmap(kernel: &Kernel, a0: usize, a1: usize) -> Result<usize, &'static str> {
    let addr = a0;
    let len = a1;
    if addr % PAGE_SZ != 0 {
        return Err("einval");
    }
    let aligned_len = (len + PAGE_SZ - 1) & !(PAGE_SZ - 1);
    let pages = aligned_len / PAGE_SZ;
    for i in 0..pages {
        let _va = addr + i * PAGE_SZ;
    }
    Ok(0)
}

fn sys_brk(kernel: &Kernel, a0: usize) -> Result<usize, &'static str> {
    let new_brk = a0;
    if new_brk == 0 {
        return Ok(0x0040_0000);
    }
    if new_brk >= KERN_BASE {
        return Err("enomem");
    }
    let aligned = (new_brk + PAGE_SZ - 1) & !(PAGE_SZ - 1);
    let cur = kernel.cur_task(0);
    if let Some(t) = cur {
        let old_brk = t.vm_token.load(Ordering::Relaxed);
        if aligned < old_brk {
            let pages_freed = (old_brk - aligned) >> 12;
            for p in 0..pages_freed {
                let va = aligned + p * PAGE_SZ;
                let _pa = v2p(va);
            }
        } else if aligned > old_brk {
            let pages_needed = (aligned - old_brk) / PAGE_SZ;
            let free = kernel.pool.free_count();
            if free < pages_needed {
                return Err("enomem");
            }
            for p in 0..pages_needed {
                let va = old_brk + p * PAGE_SZ;
                let _frame = frame_alloc(&kernel.pool);
            }
        }
        t.vm_token.store(aligned, Ordering::Release);
    }
    Ok(aligned)
}
