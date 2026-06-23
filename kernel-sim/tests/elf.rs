// AGENT
use kernel_sim::{parse_elf_load_segments, PAGE_SZ, VM_EXEC, VM_READ};

const PH_OFF: usize = 64;
const PH_SIZE: usize = 56;

fn elf_with_load_segment(
    offset: usize,
    vaddr: usize,
    file_size: usize,
    mem_size: usize,
    align: usize,
) -> Vec<u8> {
    let mut data = vec![0u8; (PH_OFF + PH_SIZE).max(offset + file_size)];
    data[0] = 0x7f;
    data[1] = b'E';
    data[2] = b'L';
    data[3] = b'F';
    data[4] = 2;
    data[5] = 1;
    data[6] = 1;
    write_u16_le(&mut data, 16, 2);
    write_u16_le(&mut data, 18, 0x3e);
    write_u32_le(&mut data, 20, 1);
    write_u64_le(&mut data, 24, vaddr as u64);
    write_u64_le(&mut data, 32, PH_OFF as u64);
    write_u16_le(&mut data, 52, 64);
    write_u16_le(&mut data, 54, PH_SIZE as u16);
    write_u16_le(&mut data, 56, 1);

    write_u32_le(&mut data, PH_OFF, 1);
    write_u32_le(&mut data, PH_OFF + 4, 0x5);
    write_u64_le(&mut data, PH_OFF + 8, offset as u64);
    write_u64_le(&mut data, PH_OFF + 16, vaddr as u64);
    write_u64_le(&mut data, PH_OFF + 24, vaddr as u64);
    write_u64_le(&mut data, PH_OFF + 32, file_size as u64);
    write_u64_le(&mut data, PH_OFF + 40, mem_size as u64);
    write_u64_le(&mut data, PH_OFF + 48, align as u64);
    data
}

fn write_u16_le(data: &mut [u8], off: usize, value: u16) {
    data[off..off + 2].copy_from_slice(&value.to_le_bytes());
}

fn write_u32_le(data: &mut [u8], off: usize, value: u32) {
    data[off..off + 4].copy_from_slice(&value.to_le_bytes());
}

fn write_u64_le(data: &mut [u8], off: usize, value: u64) {
    data[off..off + 8].copy_from_slice(&value.to_le_bytes());
}

#[test]
fn parse_elf_rejects_non_power_of_two_pt_load_align() {
    let elf = elf_with_load_segment(PAGE_SZ, 0x0040_1000, 1, PAGE_SZ, 3);

    assert_eq!(parse_elf_load_segments(&elf).unwrap_err(), "bad_phdr");
}

#[test]
fn parse_elf_rejects_mismatched_pt_load_file_and_memory_alignment() {
    let elf = elf_with_load_segment(PAGE_SZ, 0x0040_1080, 1, PAGE_SZ, PAGE_SZ);

    assert_eq!(parse_elf_load_segments(&elf).unwrap_err(), "bad_phdr");
}

#[test]
fn parse_elf_preserves_legal_segment_file_page_offset() {
    let elf = elf_with_load_segment(PAGE_SZ + 0x234, 0x0040_1234, 1, PAGE_SZ, PAGE_SZ);
    let (_entry, segments) = parse_elf_load_segments(&elf).expect("valid ELF should parse");

    let region = segments[0].vm_region().expect("valid segment should map");
    assert_eq!(region.base, 0x0040_1000);
    assert_eq!(region.len, PAGE_SZ * 2);
    assert_eq!(region.offset, PAGE_SZ);
    assert_eq!(region.flags & VM_READ, VM_READ);
    assert_eq!(region.flags & VM_EXEC, VM_EXEC);
}
