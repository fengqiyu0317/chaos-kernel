// AGENT
use super::*;
use std::ops::Range;

// AGENT TODO: These protocol helpers are not wired into the simulator yet.
// Future AF_INET socket support should call them from a Socket/FLike data path
// for IPv4 header validation and TCP/UDP checksum handling.
// AGENT TODO: Harden the helpers themselves before treating this as a reliable
// protocol utility layer: return diagnostic IPv4 parse errors, use wider
// checksum accumulation plus verify helpers, make TCP checksum APIs operate on
// TCP segments explicitly, return a fixed 12-byte pseudo header, and cover more
// edge cases with unit tests.
pub fn tcp_checksum(src_ip: u32, dst_ip: u32, payload: &[u8]) -> u16 {
    let mut sum: u32 = 0;
    sum += (src_ip >> 16) & 0xFFFF;
    sum += src_ip & 0xFFFF;
    sum += (dst_ip >> 16) & 0xFFFF;
    sum += dst_ip & 0xFFFF;
    sum += 6u32;
    sum += payload.len() as u32;
    let mut i = 0;
    while i + 1 < payload.len() {
        sum += ((payload[i] as u32) << 8) | (payload[i + 1] as u32);
        i += 2;
    }
    if i < payload.len() {
        sum += (payload[i] as u32) << 8;
    }
    while sum > 0xFFFF {
        sum = (sum & 0xFFFF) + (sum >> 16);
    }
    !sum as u16
}

// AGENT: structured IPv4 parse result used by future socket receive paths.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Ipv4HeaderInfo {
    pub src_ip: u32,
    pub dst_ip: u32,
    pub protocol: u8,
    pub ttl: u8,
    pub header_len: usize,
    pub total_len: usize,
    pub payload: Range<usize>,
    pub fragment: Ipv4FragmentInfo,
}

// AGENT: decoded IPv4 flags plus the 13-bit fragment offset field.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Ipv4FragmentInfo {
    pub raw: u16,
    pub reserved: bool,
    pub dont_fragment: bool,
    pub more_fragments: bool,
    pub fragment_offset: u16,
}

// AGENT: parse IPv4 headers with explicit total-length and payload bounds.
pub fn parse_ipv4_header(pkt: &[u8]) -> Option<Ipv4HeaderInfo> {
    if pkt.len() < 20 {
        return None;
    }
    let version = pkt[0] >> 4;
    if version != 4 {
        return None;
    }
    let ihl = (pkt[0] & 0x0F) as usize;
    let header_len = ihl.checked_mul(4)?;
    if ihl < 5 || pkt.len() < header_len {
        return None;
    }
    let total_len = u16::from_be_bytes([pkt[2], pkt[3]]) as usize;
    if total_len < header_len || total_len > pkt.len() {
        return None;
    }
    let payload = header_len..total_len;
    pkt.get(payload.clone())?;
    let flags_fragment = u16::from_be_bytes([pkt[6], pkt[7]]);
    let ttl = pkt[8];
    let protocol = pkt[9];
    let src_ip = ((pkt[12] as u32) << 24)
        | ((pkt[13] as u32) << 16)
        | ((pkt[14] as u32) << 8)
        | pkt[15] as u32;
    let dst_ip = ((pkt[16] as u32) << 24)
        | ((pkt[17] as u32) << 16)
        | ((pkt[18] as u32) << 8)
        | pkt[19] as u32;
    let mut hdr_checksum: u32 = 0;
    for j in 0..(header_len / 2) {
        // AGENT: IHL in 32-bit words, checksum in 16-bit words
        let offset = j * 2;
        hdr_checksum += ((pkt[offset] as u32) << 8) | pkt[offset + 1] as u32;
    }
    while hdr_checksum > 0xFFFF {
        hdr_checksum = (hdr_checksum & 0xFFFF) + (hdr_checksum >> 16);
    }
    // AGENT: validate header checksum (must fold to 0xFFFF for a valid header)
    if hdr_checksum != 0xFFFF {
        return None;
    }
    Some(Ipv4HeaderInfo {
        src_ip,
        dst_ip,
        protocol,
        ttl,
        header_len,
        total_len,
        payload,
        fragment: Ipv4FragmentInfo {
            raw: flags_fragment,
            reserved: (flags_fragment & 0x8000) != 0,
            dont_fragment: (flags_fragment & 0x4000) != 0,
            more_fragments: (flags_fragment & 0x2000) != 0,
            fragment_offset: flags_fragment & 0x1FFF,
        },
    })
}

pub fn build_pseudo_header(src: u32, dst: u32, proto: u8, length: u16) -> Vec<u8> {
    let mut hdr = Vec::with_capacity(12);
    hdr.push((src >> 24) as u8);
    hdr.push((src >> 16) as u8);
    hdr.push((src >> 8) as u8);
    hdr.push(src as u8);
    hdr.push((dst >> 24) as u8);
    hdr.push((dst >> 16) as u8);
    hdr.push((dst >> 8) as u8);
    hdr.push(dst as u8);
    hdr.push(0);
    hdr.push(proto);
    hdr.push((length >> 8) as u8);
    hdr.push(length as u8);
    hdr
}

pub fn compute_inet_checksum(data: &[u8]) -> u16 {
    let mut sum: u32 = 0;
    let mut i = 0;
    while i + 1 < data.len() {
        sum += ((data[i] as u32) << 8) | data[i + 1] as u32;
        i += 2;
    }
    if i < data.len() {
        sum += (data[i] as u32) << 8;
    }
    while sum > 0xFFFF {
        sum = (sum & 0xFFFF) + (sum >> 16);
    }
    !sum as u16
}
