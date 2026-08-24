// AGENT: M10 process-level checkpoint image format draft. This module is not
// wired into the existing module tree yet; it is intentionally split by
// functional scope so the format can be reviewed before syscall/trap integration.
extern crate alloc;

use alloc::vec::Vec;

mod codec;
mod decode;
mod encode;
// AGENT: expose format/validation regressions to the bootable checkpoint
// selftest because the no_std RISC-V binary cannot use the host test harness.
#[cfg(any(test, feature = "qemu-checkpoint-selftest"))]
pub(crate) mod tests;
mod types;
mod validate;

pub use self::types::{
    CheckpointError, CheckpointHeader, CheckpointImage, SavedFdEntry, SavedFdKind, SavedPage,
    SavedProcess, SavedRunState, SavedTimer, SavedTimerTargetKind, SavedTrapFrame, SavedVma,
    SectionTag,
};

// AGENT: fixed checkpoint image identity for guest-kernel process snapshots.
pub const CHECKPOINT_MAGIC: [u8; 8] = *b"CHKM10\0\0";
// AGENT: version 2 adds the immutable start_brk beside the exact current brk;
// version 1 images are rejected instead of being decoded with a shifted layout.
pub const CHECKPOINT_VERSION: u16 = 2;
// AGENT: this draft targets the current QEMU RISC-V 64-bit path.
pub const CHECKPOINT_ARCH_RISCV64: u16 = 1;
// AGENT: match the project-wide page size used by kernel-sim and kernel-qemu.
pub const CHECKPOINT_PAGE_SIZE: u32 = 4096;

const IMAGE_HEADER_LEN: usize = 32;
const SECTION_HEADER_LEN: usize = 16;

// AGENT: construct and validate current-version process snapshot images.
impl CheckpointImage {
    // AGENT: create an empty current-version RISC-V image before sections are added.
    pub fn new_riscv64() -> Self {
        Self {
            header: CheckpointHeader {
                magic: CHECKPOINT_MAGIC,
                version: CHECKPOINT_VERSION,
                arch: CHECKPOINT_ARCH_RISCV64,
                page_size: CHECKPOINT_PAGE_SIZE,
                section_count: 0,
            },
            process: None,
            trap_frame: None,
            vmas: Vec::new(),
            pages: Vec::new(),
            fds: Vec::new(),
            timers: Vec::new(),
        }
    }

    // AGENT: enforce the current M10 format scope before checkpoint bytes are
    // emitted or accepted by restore.
    pub fn validate_current_version(&self) -> Result<(), CheckpointError> {
        validate::validate_current_version(self)
    }

    // AGENT: serialize the validated current-version image into little-endian
    // sectioned bytes suitable for storage in a guest file or memory buffer.
    pub fn encode_current_version(&self) -> Result<Vec<u8>, CheckpointError> {
        encode::encode_current_version(self)
    }

    // AGENT: decode bytes and immediately apply the current-version validation
    // contract expected by restore.
    pub fn decode_current_version(bytes: &[u8]) -> Result<Self, CheckpointError> {
        decode::decode_current_version(bytes)
    }
}
