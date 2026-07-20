// AGENT
use super::*;

// AGENT: carry a fully prepared user address space and initial architecture entry
// across the transactional task-creation or exec commit boundary.
pub(crate) struct PreparedUserImage {
    pub addr_space: AddrSpace,
    pub user_entry: UserEntry,
}

// AGENT: keep the pure exec result independent of the live TrapFrame reference
// held by the architecture syscall adapter.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct UserEntry {
    pub entry: usize,
    pub stack_pointer: usize,
}

// AGENT: represent one maximal page-aligned ELF mapping run after combining
// the permissions of every PT_LOAD segment that covers the same pages.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct ElfLoadRegion {
    pub base: usize,
    pub len: usize,
    pub flags: u32,
}

// AGENT: keep one start/end event per PT_LOAD instead of materializing one
// parser-owned record for every covered virtual page.
#[derive(Clone, Copy)]
struct ElfLoadBoundary {
    addr: usize,
    flags: u32,
    entering: bool,
}

// AGENT: parse an ELF, normalize format failures to ENOEXEC, and construct one
// complete user image shared by initial task creation and exec.
pub(crate) fn prepare_user_image(
    elf_data: &[u8],
    args: Vec<String>,
    envs: Vec<String>,
    pool: &FramePool,
) -> Result<PreparedUserImage, &'static str> {
    let elf = parse_elf(elf_data).map_err(normalize_user_image_error)?;
    let mut addr_space = AddrSpace::new();
    match populate_user_image(&mut addr_space, elf_data, elf, args, envs, pool) {
        Ok(user_entry) => Ok(PreparedUserImage {
            addr_space,
            user_entry,
        }),
        Err(err) => {
            addr_space.release_all_pages();
            Err(normalize_user_image_error(err))
        }
    }
}

// AGENT: keep parser diagnostics private while presenting Linux exec semantics;
// resource, permission, size, and explicitly unsupported errors remain intact.
fn normalize_user_image_error(err: &'static str) -> &'static str {
    match err {
        "too_short" | "bad_magic" | "not_64bit" | "not_le" | "bad_version" | "not_exec"
        | "bad_machine" | "no_phdrs" | "bad_phent" | "ph_overflow" | "bad_phdr" | "no_load"
        | "bad_entry" => "enoexec",
        _ => err,
    }
}

// AGENT: translate only the ELF PF_R/PF_W/PF_X bits into VM permissions so a
// segment with no access bits remains inaccessible after loader writes finish.
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
    flags
}

// AGENT: keep page-aligned ELF mapping policy beside user-image construction
// while reusing the parser's checked raw PT_LOAD memory-range validation.
fn load_segment_page_range(vaddr: usize, mem_size: usize) -> Result<(usize, usize), &'static str> {
    let mem_end = validate_load_segment_memory_range(vaddr, mem_size)?;
    let page_start = align_down(vaddr, PAGE_SZ);
    let page_end = checked_align_up(mem_end, PAGE_SZ).ok_or("bad_phdr")?;
    if page_start >= page_end {
        return Err("bad_phdr");
    }
    Ok((page_start, page_end))
}

// AGENT: sweep PT_LOAD page boundaries into sorted, disjoint mapping runs;
// adjacent runs with the same permission union are coalesced before AddrSpace
// mapping and protection work begins.
pub(crate) fn normalize_elf_load_regions(
    segments: &[ElfLoadSegment],
) -> Result<Vec<ElfLoadRegion>, &'static str> {
    let event_capacity = segments.len().checked_mul(2).ok_or("ph_overflow")?;
    let mut events = Vec::with_capacity(event_capacity);
    for segment in segments {
        let (page_start, page_end) = load_segment_page_range(segment.vaddr, segment.mem_size)?;
        events.push(ElfLoadBoundary {
            addr: page_start,
            flags: segment.flags,
            entering: true,
        });
        events.push(ElfLoadBoundary {
            addr: page_end,
            flags: segment.flags,
            entering: false,
        });
    }
    if events.is_empty() {
        return Err("no_load");
    }
    events.sort_unstable_by_key(|event| event.addr);

    const PERMISSION_BITS: [u32; 3] = [0x1, 0x2, 0x4];
    let mut active_segments = 0usize;
    let mut active_permissions = [0usize; PERMISSION_BITS.len()];
    let mut regions = Vec::<ElfLoadRegion>::new();
    let mut cursor = events[0].addr;
    let mut index = 0usize;

    while index < events.len() {
        let boundary = events[index].addr;
        if cursor < boundary && active_segments != 0 {
            let mut flags = 0u32;
            for (bit, count) in PERMISSION_BITS.iter().zip(active_permissions.iter()) {
                if *count != 0 {
                    flags |= *bit;
                }
            }
            let len = boundary.checked_sub(cursor).ok_or("bad_phdr")?;
            if let Some(previous) = regions.last_mut() {
                let previous_end = previous
                    .base
                    .checked_add(previous.len)
                    .ok_or("ph_overflow")?;
                if previous_end == cursor && previous.flags == flags {
                    previous.len = previous.len.checked_add(len).ok_or("ph_overflow")?;
                } else {
                    regions.push(ElfLoadRegion {
                        base: cursor,
                        len,
                        flags,
                    });
                }
            } else {
                regions.push(ElfLoadRegion {
                    base: cursor,
                    len,
                    flags,
                });
            }
        }

        while index < events.len() && events[index].addr == boundary {
            let event = events[index];
            if event.entering {
                active_segments = active_segments.checked_add(1).ok_or("ph_overflow")?;
            } else {
                active_segments = active_segments.checked_sub(1).ok_or("bad_phdr")?;
            }
            for (permission_index, bit) in PERMISSION_BITS.iter().enumerate() {
                if event.flags & *bit == 0 {
                    continue;
                }
                active_permissions[permission_index] = if event.entering {
                    active_permissions[permission_index]
                        .checked_add(1)
                        .ok_or("ph_overflow")?
                } else {
                    active_permissions[permission_index]
                        .checked_sub(1)
                        .ok_or("bad_phdr")?
                };
            }
            index += 1;
        }
        cursor = boundary;
    }

    if active_segments != 0 || active_permissions.iter().any(|count| *count != 0) {
        return Err("bad_phdr");
    }
    Ok(regions)
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
) -> Result<UserEntry, &'static str> {
    let ParsedElf {
        entry,
        load_segments,
    } = elf;
    let load_regions = normalize_elf_load_regions(&load_segments)?;

    // AGENT: map each maximal permission run once with temporary write access;
    // shared boundary pages retain the union of all covering segment flags.
    for region in &load_regions {
        let temporary_flags = elf_vm_flags(region.flags) | VM_WRITE;
        addr_space.map_region(
            VmRegion::new(region.base, region.len, temporary_flags),
            pool,
        )?;
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

    // AGENT: remove temporary write access once per normalized run instead of
    // splitting and protecting every page independently.
    for region in &load_regions {
        let final_flags = elf_vm_flags(region.flags);
        if final_flags & VM_WRITE == 0 {
            addr_space.protect(region.base, region.len, final_flags)?;
        }
    }
    let image_end = load_regions
        .last()
        .and_then(|region| region.base.checked_add(region.len))
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

    addr_space.set_brk_metadata(checked_align_up(image_end, PAGE_SZ).ok_or("ph_overflow")?)?;
    Ok(UserEntry {
        entry,
        stack_pointer: sp,
    })
}
