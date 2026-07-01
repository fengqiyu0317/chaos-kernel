// AGENT
use super::*;

pub struct ProcInit {
    pub args: Vec<String>,
    pub envs: Vec<String>,
    pub auxv: BTreeMap<u8, usize>,
}
impl ProcInit {
    // AGENT: keep the stack-construction helper available to release builds
    // that call it from migrated exec/task code across codegen units.
    #[inline]
    pub fn push_at(
        &self,
        addr_space: &mut AddrSpace,
        pool: &FramePool,
        top: usize,
    ) -> Result<usize, &'static str> {
        let word = mem::size_of::<usize>();
        if top & 0xF != 0 {
            return Err("einval");
        }
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

        let ptr_bytes = self.checked_ptr_bytes(word)?;
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

    // AGENT: expose a checked size calculation so exec rejects impossible
    // argument layouts before mapping and writing the user stack.
    pub fn checked_total_size(&self) -> Result<usize, &'static str> {
        let word = mem::size_of::<usize>();
        let mut sz = 0usize;
        for a in &self.args {
            sz = sz
                .checked_add(a.len().checked_add(1).ok_or("e2big")?)
                .ok_or("e2big")?;
        }
        for e in &self.envs {
            sz = sz
                .checked_add(e.len().checked_add(1).ok_or("e2big")?)
                .ok_or("e2big")?;
        }
        sz = sz
            .checked_add(self.checked_ptr_bytes(word)?)
            .ok_or("e2big")?;
        sz.checked_add(15).map(|size| size & !15).ok_or("e2big")
    }

    // AGENT: keep the old infallible helper as a saturating compatibility view;
    // new exec paths should use checked_total_size() for error reporting.
    pub fn total_size(&self) -> usize {
        self.checked_total_size().unwrap_or(usize::MAX)
    }

    // AGENT: account for argc, argv/envp null sentinels, auxv key/value pairs,
    // and the final AT_NULL pair without relying on unchecked usize arithmetic.
    fn checked_ptr_bytes(&self, word: usize) -> Result<usize, &'static str> {
        let aux_words = self.auxv.len().checked_mul(2).ok_or("e2big")?;
        let ptr_words = 1usize
            .checked_add(self.args.len())
            .ok_or("e2big")?
            .checked_add(1)
            .ok_or("e2big")?
            .checked_add(self.envs.len())
            .ok_or("e2big")?
            .checked_add(1)
            .ok_or("e2big")?
            .checked_add(aux_words)
            .ok_or("e2big")?
            .checked_add(2)
            .ok_or("e2big")?;
        ptr_words.checked_mul(word).ok_or("e2big")
    }

    // AGENT: write one native-width stack slot through the unified user-copy path.
    fn write_usize(
        addr_space: &mut AddrSpace,
        pool: &FramePool,
        cur: &mut usize,
        value: usize,
    ) -> Result<(), &'static str> {
        addr_space.write_user_bytes(*cur, &value.to_ne_bytes(), pool)?;
        *cur += mem::size_of::<usize>();
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
