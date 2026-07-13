// AGENT: keep non-cryptographic state hashing separate from memory-management
// bit and address helpers.

// AGENT: combine one structured field into an accumulated diagnostic hash.
pub fn hash_combine(seed: u64, value: u64) -> u64 {
    seed ^ value
        .wrapping_add(0x9e3779b97f4a7c15)
        .wrapping_add(seed << 6)
        .wrapping_add(seed >> 2)
}

// AGENT: apply the MurmurHash3 final avalanche after all fields are combined.
pub fn murmurhash3_finalize(mut hash: u64) -> u64 {
    hash ^= hash >> 33;
    hash = hash.wrapping_mul(0xff51afd7ed558ccd);
    hash ^= hash >> 33;
    hash = hash.wrapping_mul(0xc4ceb9fe1a85ec53);
    hash ^= hash >> 33;
    hash
}
