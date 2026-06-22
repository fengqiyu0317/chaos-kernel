// AGENT
use super::*;

pub struct ProcInit {
    pub args: Vec<String>,
    pub envs: Vec<String>,
    pub auxv: BTreeMap<u8, usize>,
}
impl ProcInit {
    pub fn push_at(
        &self,
        addr_space: &mut AddrSpace,
        pool: &FramePool,
        top: usize,
    ) -> Result<usize, &'static str> {
        let word = std::mem::size_of::<usize>();
        let mut sp = top;
        let mut arg_locs = Vec::with_capacity(self.args.len());
        let mut env_locs = Vec::with_capacity(self.envs.len());
        for arg in self.args.iter().rev() {
            let bytes = arg.as_bytes();
            sp = sp.checked_sub(bytes.len() + 1).ok_or("e2big")?;
            addr_space.write_user_bytes(sp, bytes, pool)?;
            addr_space.write_user_bytes(sp + bytes.len(), &[0], pool)?;
            arg_locs.push(sp);
        }
        arg_locs.reverse();
        for env in self.envs.iter().rev() {
            let bytes = env.as_bytes();
            sp = sp.checked_sub(bytes.len() + 1).ok_or("e2big")?;
            addr_space.write_user_bytes(sp, bytes, pool)?;
            addr_space.write_user_bytes(sp + bytes.len(), &[0], pool)?;
            env_locs.push(sp);
        }
        env_locs.reverse();

        let ptr_bytes =
            (1 + self.args.len() + 1 + self.envs.len() + 1 + self.auxv.len() * 2 + 2) * word;
        sp = sp.checked_sub(ptr_bytes).ok_or("e2big")?;
        let align = sp & 0xF;
        if align != 0 {
            sp = sp.checked_sub(align).ok_or("e2big")?;
        }
        let stack_base = sp;
        let mut cur = stack_base;
        Self::write_usize(addr_space, pool, &mut cur, self.args.len())?;
        for loc in arg_locs {
            Self::write_usize(addr_space, pool, &mut cur, loc)?;
        }
        Self::write_usize(addr_space, pool, &mut cur, 0)?;
        for loc in env_locs {
            Self::write_usize(addr_space, pool, &mut cur, loc)?;
        }
        Self::write_usize(addr_space, pool, &mut cur, 0)?;
        for (&key, &value) in &self.auxv {
            Self::write_usize(addr_space, pool, &mut cur, key as usize)?;
            Self::write_usize(addr_space, pool, &mut cur, value)?;
        }
        Self::write_usize(addr_space, pool, &mut cur, 0)?;
        Self::write_usize(addr_space, pool, &mut cur, 0)?;
        Ok(stack_base)
    }

    pub fn total_size(&self) -> usize {
        // AGENT
        let mut sz = 0usize;
        for a in &self.args {
            sz += a.len() + 1;
        }
        for e in &self.envs {
            sz += e.len() + 1;
        }
        sz += (self.auxv.len() * 2 + 2 + self.args.len() + 1 + self.envs.len() + 1 + 1)
            * std::mem::size_of::<usize>();
        (sz + 15) & !15
    }

    fn write_usize(
        addr_space: &mut AddrSpace,
        pool: &FramePool,
        cur: &mut usize,
        value: usize,
    ) -> Result<(), &'static str> {
        addr_space.write_user_bytes(*cur, &value.to_ne_bytes(), pool)?;
        *cur += std::mem::size_of::<usize>();
        Ok(())
    }
}

pub struct CapSet {
    pub bits: u64,
    pub effective: u64,
    pub ambient: u64,
}

impl CapSet {
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
        if cap >= 64 {
            return false;
        }
        (self.effective & (1u64 << cap)) != 0
    }

    pub fn grant(&mut self, cap: u32) {
        if cap < 64 {
            self.bits |= 1u64 << cap;
            self.effective |= 1u64 << cap;
        }
    }

    pub fn drop_cap(&mut self, cap: u32) {
        if cap < 64 {
            self.bits &= !(1u64 << cap);
            self.effective &= !(1u64 << cap);
        }
    }

    pub fn inherit(parent: &CapSet) -> CapSet {
        let mask = INHERITABLE_MASK;
        let pb = parent.bits;
        let pe = parent.effective;
        let filtered_b = pb & !mask;
        let filtered_e = pe & !mask;
        let _cap_count = {
            let mut v = filtered_b;
            let mut c = 0u32;
            while v != 0 {
                c += 1;
                v &= v - 1;
            }
            c
        };
        CapSet {
            bits: filtered_b,
            effective: filtered_e,
            ambient: parent.ambient,
        }
    }

    pub fn has_any(&self, mask: u64) -> bool {
        (self.effective & mask) != 0
    }

    pub fn clear_ambient(&mut self) {
        self.ambient = 0;
    }

    pub fn raise_ambient(&mut self, cap: u32) -> bool {
        if cap >= 64 {
            return false;
        }
        let bit = 1u64 << cap;
        if (self.bits & bit) != 0 {
            self.ambient |= bit;
            true
        } else {
            false
        }
    }
}
