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

// AGENT: translate ELF permission bits into the VM flags used by AddrSpace.
fn segment_vm_flags(segment: &ElfLoadSegment) -> u32 {
    let mut flags = 0;
    if segment.flags & 0x4 != 0 {
        flags |= VM_READ;
    }
    if segment.flags & 0x2 != 0 {
        flags |= VM_WRITE;
    }
    if segment.flags & 0x1 != 0 {
        flags |= VM_EXEC;
    }
    if flags == 0 {
        VM_READ
    } else {
        flags
    }
}

// AGENT: derive the page-aligned virtual mapping covered by one PT_LOAD
// segment while keeping address-space policy out of the pure ELF parser.
fn segment_vm_region(segment: &ElfLoadSegment) -> Result<VmRegion, &'static str> {
    let page_base = segment.vaddr & !(PAGE_SZ - 1);
    let page_off = segment.vaddr - page_base;
    let file_page_offset = segment.offset.checked_sub(page_off).ok_or("bad_phdr")?;
    if file_page_offset % PAGE_SZ != 0 {
        return Err("bad_phdr");
    }
    let mapped_len = page_off
        .checked_add(segment.mem_size)
        .and_then(|len| len.checked_add(PAGE_SZ - 1))
        .map(|len| len & !(PAGE_SZ - 1))
        .ok_or("ph_overflow")?;
    if mapped_len == 0 || page_base.checked_add(mapped_len).is_none() {
        return Err("ph_overflow");
    }
    Ok(VmRegion::new(
        page_base,
        mapped_len,
        segment_vm_flags(segment),
    ))
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
    let mut image_end = 0usize;
    for segment in elf.load_segments {
        let region = segment_vm_region(&segment)?;
        let region_base = region.base;
        let region_len = region.len;
        let region_flags = region.flags;
        let region_end = region.end();
        addr_space.map_region(
            VmRegion {
                flags: region_flags | VM_WRITE,
                ..region
            },
            pool,
        )?;

        let file_end = segment
            .offset
            .checked_add(segment.file_size)
            .ok_or("ph_overflow")?;
        if file_end > elf_data.len() {
            return Err("ph_overflow");
        }
        addr_space.write_user_bytes(segment.vaddr, &elf_data[segment.offset..file_end], pool)?;
        addr_space.protect(region_base, region_len, region_flags)?;
        image_end = max(image_end, region_end);
    }

    let init = ProcInit {
        args,
        envs,
        auxv: BTreeMap::from([(AT_PAGESZ, PAGE_SZ), (AT_ENTRY, elf.entry)]),
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
    thd_ctx.uctx.set_ip(elf.entry as u64);
    Ok(thd_ctx)
}
