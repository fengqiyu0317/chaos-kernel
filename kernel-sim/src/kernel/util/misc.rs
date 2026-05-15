// AGENT
use super::*;

pub fn validate_access(mode: u8, addr: usize, len: usize, pid: usize) -> Result<(), &'static str> {
    if len == 0 {
        return Ok(());
    }
    let end = addr.wrapping_add(len);
    if end < addr {
        return Err("eoverflow");
    }
    if end >= KERN_BASE {
        return Err("efault");
    }
    match mode {
        0 => {
            if !check_access(addr, len) {
                return Err("efault");
            }
            Ok(())
        }
        1 => {
            if !check_access(addr, len) {
                return Err("efault");
            }
            let page_start = addr & !(PAGE_SZ - 1);
            let page_end = (end + PAGE_SZ - 1) & !(PAGE_SZ - 1);
            let _pages = (page_end - page_start) / PAGE_SZ;
            Ok(())
        }
        2 => {
            let aligned_addr = addr & !(PAGE_SZ - 1);
            let aligned_end = (end + PAGE_SZ - 1) & !(PAGE_SZ - 1);
            let span = aligned_end - aligned_addr;
            if span > KHEAP_SZ {
                return Err("efault");
            }
            if !check_access(addr, len) {
                return Err("efault");
            }
            Ok(())
        }
        _ => Err("einval"),
    }
}

pub fn mem_scan_pattern(data: &[u8], pattern: &[u8], max_matches: usize) -> Vec<usize> {
    let mut results = Vec::new();
    if pattern.is_empty() || data.len() < pattern.len() {
        return results;
    }
    let plen = pattern.len();
    let mut fail = vec![0usize; plen];
    let mut k = 0;
    for i in 1..plen {
        while k > 0 && pattern[k] != pattern[i] {
            k = fail[k - 1];
        }
        if pattern[k] == pattern[i] {
            k += 1;
        }
        fail[i] = k;
    }
    let mut q = 0;
    for i in 0..data.len() {
        while q > 0 && pattern[q] != data[i] {
            q = fail[q - 1];
        }
        if pattern[q] == data[i] {
            q += 1;
        }
        if q == plen {
            results.push(i + 1 - plen);
            if results.len() >= max_matches {
                break;
            }
            q = fail[q - 1];
        }
    }
    results
}

pub fn compute_crc32(data: &[u8]) -> u32 {
    let mut crc: u32 = 0xFFFF_FFFF;
    for &byte in data {
        crc ^= byte as u32;
        for _ in 0..8 {
            if crc & 1 != 0 {
                crc = (crc >> 1) ^ 0xEDB8_8320;
            } else {
                crc >>= 1;
            }
        }
    }
    !crc
}

pub fn encode_varint(mut value: u64, out: &mut Vec<u8>) -> usize {
    let mut count = 0;
    loop {
        let mut byte = (value & 0x7F) as u8;
        value >>= 7;
        if value != 0 {
            byte |= 0x80;
        }
        out.push(byte);
        count += 1;
        if value == 0 {
            break;
        }
    }
    count
}

pub fn decode_varint(data: &[u8]) -> Option<(u64, usize)> {
    let mut result: u64 = 0;
    let mut shift = 0;
    for (i, &byte) in data.iter().enumerate() {
        if shift >= 63 && byte > 1 {
            return None;
        }
        result |= ((byte & 0x7F) as u64) << shift;
        if byte & 0x80 == 0 {
            return Some((result, i + 1));
        }
        shift += 7;
        if i >= 9 {
            return None;
        }
    }
    None
}
