// AGENT: isolate capability-set validation and inheritance from process
// identity, lifecycle, and initial image construction.
use super::*;

// AGENT: model owned, effective, and ambient capabilities as one value object.
pub struct CapSet {
    pub bits: u64,
    pub effective: u64,
    pub ambient: u64,
}

// AGENT: centralize capability-set mutation and inheritance invariants.
impl CapSet {
    // AGENT: keep capability-index validation in one place so callers do not
    // repeat manual shift bounds checks.
    fn cap_bit(cap: u32) -> Option<u64> {
        if cap < 64 {
            Some(1u64 << cap)
        } else {
            None
        }
    }

    pub fn new() -> Self {
        Self {
            bits: 0,
            effective: 0,
            ambient: 0,
        }
    }

    pub fn full() -> Self {
        Self {
            bits: !0u64,
            effective: !0u64,
            ambient: 0,
        }
    }

    pub fn check(&self, cap: u32) -> bool {
        if let Some(bit) = Self::cap_bit(cap) {
            (self.effective & bit) != 0
        } else {
            false
        }
    }

    pub fn grant(&mut self, cap: u32) {
        if let Some(bit) = Self::cap_bit(cap) {
            self.bits |= bit;
            self.effective |= bit;
        }
    }

    // AGENT: dropping a capability must also remove it from ambient so a later
    // inheritance path cannot keep a capability the process no longer owns.
    pub fn drop_cap(&mut self, cap: u32) {
        if let Some(bit) = Self::cap_bit(cap) {
            self.bits &= !bit;
            self.effective &= !bit;
            self.ambient &= !bit;
        }
    }

    // AGENT: keep inherited capabilities easy to reason about: the mask lists
    // what may cross the boundary, and effective/ambient cannot outgrow it.
    pub fn inherit(parent: &CapSet) -> CapSet {
        let inherited_bits = parent.bits & INHERITABLE_MASK;
        let inherited_effective = parent.effective & inherited_bits;
        let inherited_ambient = parent.ambient & inherited_bits;
        CapSet {
            bits: inherited_bits,
            effective: inherited_effective,
            ambient: inherited_ambient,
        }
    }

    pub fn has_any(&self, mask: u64) -> bool {
        (self.effective & mask) != 0
    }

    pub fn clear_ambient(&mut self) {
        self.ambient = 0;
    }

    // AGENT: only owned capabilities that are allowed to cross an inheritance
    // boundary may be raised into the ambient set.
    pub fn raise_ambient(&mut self, cap: u32) -> bool {
        let Some(bit) = Self::cap_bit(cap) else {
            return false;
        };
        let owns_capability = (self.bits & bit) != 0;
        let may_inherit = (INHERITABLE_MASK & bit) != 0;
        if owns_capability && may_inherit {
            self.ambient |= bit;
            true
        } else {
            false
        }
    }
}
