// AGENT: isolate kernel direct-map address conversion from frame ownership and
// virtual-memory metadata.
use super::{KERN_BASE, PHYS_OFF};

// AGENT: translate a physical address through the current high-half direct map.
pub fn p2v(pa: usize) -> usize {
    PHYS_OFF.checked_add(pa).expect("p2v overflow")
}

// AGENT: reverse p2v() and reject addresses outside the high-half direct map.
pub fn v2p(va: usize) -> usize {
    va.checked_sub(PHYS_OFF).expect("v2p below direct map")
}

// AGENT: compute an offset from the kernel virtual base without wrapping.
pub fn k_off(va: usize) -> usize {
    va.checked_sub(KERN_BASE)
        .expect("kernel address below KERN_BASE")
}
