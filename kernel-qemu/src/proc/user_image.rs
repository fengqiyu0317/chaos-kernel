// AGENT
use super::*;

// AGENT: carry a fully prepared user address space and initial thread context
// across the transactional task-creation or exec commit boundary.
pub(crate) struct PreparedUserImage {
    pub addr_space: AddrSpace,
    pub thd_ctx: ThdCtx,
}

// AGENT: parse an ELF and construct one complete user image so initial task
// creation and exec share mapping, segment-copy, bss, stack, and brk semantics.
pub(crate) fn prepare_user_image(
    elf_data: &[u8],
    args: Vec<String>,
    envs: Vec<String>,
    pool: &FramePool,
) -> Result<PreparedUserImage, &'static str> {
    let elf = parse_elf(elf_data)?;
    let mut addr_space = AddrSpace::new();
    match populate_user_image(&mut addr_space, elf_data, elf, args, envs, pool) {
        Ok(thd_ctx) => Ok(PreparedUserImage {
            addr_space,
            thd_ctx,
        }),
        Err(err) => {
            addr_space.release_all_pages(pool);
            Err(err)
        }
    }
}

// AGENT: translate aggregated ELF permission bits into the VM flags used by
// one page-granular AddrSpace mapping.
fn elf_vm_flags(elf_flags: u32) -> u32 {
    let mut flags = 0;
    if elf_flags & 0x4 != 0 {
        flags |= VM_READ;
    }
    if elf_flags & 0x2 != 0 {
        flags |= VM_WRITE;
    }
    if elf_flags & 0x1 != 0 {
        flags |= VM_EXEC;
    }
    if flags == 0 {
        VM_READ
    } else {
        flags
    }
}

// AGENT: populate a fresh address space from parsed ELF segments, then build
// the initial userspace stack and thread entry context.
fn populate_user_image(
    addr_space: &mut AddrSpace,
    elf_data: &[u8],
    elf: ParsedElf,
    args: Vec<String>,
    envs: Vec<String>,
    pool: &FramePool,
) -> Result<ThdCtx, &'static str> {
    let ParsedElf {
        entry,
        load_segments,
        load_pages,
    } = elf;

    // AGENT: map the union of all PT_LOAD pages exactly once with temporary
    // write permission; final per-page permissions are installed after copying.
    for page in &load_pages {
        addr_space.map_region(VmRegion::new(page.vaddr, PAGE_SZ, VM_READ | VM_WRITE), pool)?;
    }

    let zeroes = vec![0u8; PAGE_SZ];
    // AGENT: after every page exists, populate each segment in program-header
    // order and explicitly clear its BSS range, including shared boundary pages.
    for segment in &load_segments {
        let file_end = segment
            .offset
            .checked_add(segment.file_size)
            .ok_or("ph_overflow")?;
        if file_end > elf_data.len() {
            return Err("ph_overflow");
        }
        addr_space.write_user_bytes(segment.vaddr, &elf_data[segment.offset..file_end], pool)?;

        let bss_start = segment
            .vaddr
            .checked_add(segment.file_size)
            .ok_or("ph_overflow")?;
        let bss_end = segment
            .vaddr
            .checked_add(segment.mem_size)
            .ok_or("ph_overflow")?;
        let mut cursor = bss_start;
        while cursor < bss_end {
            let len = min(PAGE_SZ, bss_end - cursor);
            addr_space.write_user_bytes(cursor, &zeroes[..len], pool)?;
            cursor += len;
        }
    }

    // AGENT: shared pages receive the union of all covering segment flags;
    // protect only after file and BSS writes no longer need temporary access.
    for page in &load_pages {
        addr_space.protect(page.vaddr, PAGE_SZ, elf_vm_flags(page.flags))?;
    }
    let image_end = load_pages
        .last()
        .and_then(|page| page.vaddr.checked_add(PAGE_SZ))
        .ok_or("ph_overflow")?;

    let init = ProcInit {
        args,
        envs,
        auxv: BTreeMap::from([(AT_PAGESZ, PAGE_SZ), (AT_ENTRY, entry)]),
    };
    if init.checked_total_size()? > USR_STK_SZ {
        return Err("e2big");
    }

    let stack = VmRegion::new(USR_STK_OFF, USR_STK_SZ, VM_READ | VM_WRITE | VM_GROWSDOWN);
    addr_space.map_region(stack, pool)?;
    let sp = init.push_at(addr_space, pool, USR_STK_OFF + USR_STK_SZ)?;
    if sp < USR_STK_OFF || sp > USR_STK_OFF + USR_STK_SZ {
        return Err("e2big");
    }

    addr_space.vm_map.brk = (image_end + PAGE_SZ - 1) & !(PAGE_SZ - 1);
    let mut thd_ctx = ThdCtx::default();
    thd_ctx.uctx.set_sp(sp as u64);
    thd_ctx.uctx.set_ip(entry as u64);
    Ok(thd_ctx)
}
