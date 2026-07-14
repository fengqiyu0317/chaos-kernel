// AGENT: keep migrated stateless bit and alignment helpers together.
use core::mem::size_of;

pub fn bitwise_merge(a: u64, b: u64, mask: u64) -> u64 {
    (a & !mask) | (b & mask)
}

// AGENT: keep zero-distance rotations masked to the requested bit width.
pub fn rotate_bits(value: u64, amount: u32, width: u32) -> u64 {
    if width == 0 || width > 64 {
        return value;
    }
    let mask = if width == 64 {
        !0u64
    } else {
        (1u64 << width) - 1
    };
    let v = value & mask;
    let actual = amount % width;
    if actual == 0 {
        return v;
    }
    ((v << actual) | (v >> (width - actual))) & mask
}

// AGENT: report invalid alignments and arithmetic overflow to callers instead
// of returning the unaligned input as if alignment had succeeded.
pub fn checked_align_up(addr: usize, align: usize) -> Option<usize> {
    if !align.is_power_of_two() {
        return None;
    }
    addr.checked_add(align - 1)
        .map(|value| value & !(align - 1))
}

// AGENT: centralize power-of-two address flooring while treating a bad
// alignment as an internal caller error rather than silently accepting it.
pub fn align_down(addr: usize, align: usize) -> usize {
    assert!(align.is_power_of_two(), "alignment must be a power of two");
    addr & !(align - 1)
}

pub fn log2_floor(v: usize) -> usize {
    if v == 0 {
        return 0;
    }
    (size_of::<usize>() * 8) - 1 - (v.leading_zeros() as usize)
}

// AGENT: keep the MM selftest module in src/mm/tests.rs even when bits.rs is
// compiled through the standalone mm_bits path in kernel-qemu/src/main.rs.
#[cfg(any(test, feature = "qemu-mm-selftest"))]
#[path = "tests.rs"]
pub mod tests;
