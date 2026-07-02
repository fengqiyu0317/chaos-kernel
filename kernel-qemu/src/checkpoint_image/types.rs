use alloc::vec::Vec;
use core::convert::TryFrom;

// AGENT: typed errors keep checkpoint validation separate from string errno
// mapping; syscall integration can translate these later.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CheckpointError {
    BadMagic,
    UnsupportedVersion,
    UnsupportedArch,
    BadPageSize,
    Truncated,
    LengthOverflow,
    InvalidEnum,
    BadSection,
    DuplicateSection,
    MissingProcess,
    MissingTrapFrame,
    BadAlignment,
    BadPageLength,
    UnsupportedFd,
    UnsupportedState,
    InconsistentOpenDescription,
}

// AGENT: section tags make the image extensible without changing the fixed
// header when later milestones add namespaces, credentials, or devices.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u16)]
pub enum SectionTag {
    Process = 1,
    TrapFrame = 2,
    Vmas = 3,
    Pages = 4,
    Fds = 5,
    Timers = 6,
}

// AGENT: restore only to a new pid in the first version; original pid reuse is
// a later namespace/process-table feature.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u16)]
pub enum RestorePolicy {
    NewPid = 1,
}

// AGENT: store only safe states that do not require serializing a kernel stack
// or a wait-queue position.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u16)]
pub enum SavedRunState {
    SyscallSafePoint = 1,
    ExplicitQuiescentPoint = 2,
    BlockedWait = 3,
}

// AGENT: classify mappings before the restore path recreates VMAs and pages.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u16)]
pub enum MappingKind {
    Anonymous = 1,
    Heap = 2,
    Stack = 3,
    FilePrivate = 4,
    FileShared = 5,
}

// AGENT: file descriptor kinds are deliberately narrow for the first restore
// implementation; unsupported state should be rejected before image emission.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u16)]
pub enum SavedFdKind {
    Stdin = 1,
    Stdout = 2,
    Stderr = 3,
    RegularMemoryFile = 4,
    Pipe = 5,
    Epoll = 6,
    Socket = 7,
    Tty = 8,
}

// AGENT: fixed image header shared by all M10 sections.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CheckpointHeader {
    pub magic: [u8; 8],
    pub version: u16,
    pub arch: u16,
    pub page_size: u32,
    pub flags: u64,
    pub section_count: u32,
}

// AGENT: process-level metadata that is not owned by a single VMA or trap
// frame, including the first-version one-thread restriction.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SavedProcess {
    pub original_pid: u64,
    pub brk: u64,
    pub stack_base: u64,
    pub stack_len: u64,
    pub thread_count: u32,
    pub run_state: SavedRunState,
    pub restore_policy: RestorePolicy,
}

// AGENT: architecture return frame captured at a syscall safe point after the
// ecall PC has advanced to the user continuation address.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SavedTrapFrame {
    pub regs: [u64; 32],
    pub sstatus: u64,
    pub sepc: u64,
}

// AGENT: VMA metadata is restored before resident page contents are replayed.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SavedVma {
    pub start: u64,
    pub len: u64,
    pub flags: u32,
    pub file_offset: u64,
    pub kind: MappingKind,
    pub object_id: u64,
}

// AGENT: resident anonymous page payload copied out of the user address space.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SavedPage {
    pub vaddr: u64,
    pub bytes: Vec<u8>,
}

// AGENT: fd entries preserve descriptor flags separately from shared
// open-file-description state.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SavedFdEntry {
    pub fd: u32,
    pub description_id: u32,
    pub cloexec: bool,
    pub status_flags: u32,
    pub kind: SavedFdKind,
    pub object_id: u64,
    pub offset: u64,
}

// AGENT: minimal timer state for deadlines that can be restarted after restore.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SavedTimer {
    pub timer_id: u64,
    pub clock_id: u32,
    pub deadline_ticks: u64,
    pub interval_ticks: u64,
}

// AGENT: complete first-version checkpoint image assembled from independently
// parseable sections.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CheckpointImage {
    pub header: CheckpointHeader,
    pub process: Option<SavedProcess>,
    pub trap_frame: Option<SavedTrapFrame>,
    pub vmas: Vec<SavedVma>,
    pub pages: Vec<SavedPage>,
    pub fds: Vec<SavedFdEntry>,
    pub timers: Vec<SavedTimer>,
}

// AGENT: convert serialized numeric tags into checked section identifiers.
impl TryFrom<u16> for SectionTag {
    type Error = CheckpointError;

    // AGENT: map known serialized section tag numbers and reject unknown tags.
    fn try_from(value: u16) -> Result<Self, Self::Error> {
        match value {
            1 => Ok(Self::Process),
            2 => Ok(Self::TrapFrame),
            3 => Ok(Self::Vmas),
            4 => Ok(Self::Pages),
            5 => Ok(Self::Fds),
            6 => Ok(Self::Timers),
            _ => Err(CheckpointError::BadSection),
        }
    }
}

// AGENT: convert serialized restore policy fields into checked enums.
impl TryFrom<u16> for RestorePolicy {
    type Error = CheckpointError;

    // AGENT: accept only the first-version new-pid restore policy.
    fn try_from(value: u16) -> Result<Self, Self::Error> {
        match value {
            1 => Ok(Self::NewPid),
            _ => Err(CheckpointError::InvalidEnum),
        }
    }
}

// AGENT: convert serialized process run state fields into checked enums.
impl TryFrom<u16> for SavedRunState {
    type Error = CheckpointError;

    // AGENT: map serialized run-state values into the checkpoint enum.
    fn try_from(value: u16) -> Result<Self, Self::Error> {
        match value {
            1 => Ok(Self::SyscallSafePoint),
            2 => Ok(Self::ExplicitQuiescentPoint),
            3 => Ok(Self::BlockedWait),
            _ => Err(CheckpointError::InvalidEnum),
        }
    }
}

// AGENT: convert serialized mapping kind fields into checked enums.
impl TryFrom<u16> for MappingKind {
    type Error = CheckpointError;

    // AGENT: map serialized VMA kind values into the checkpoint enum.
    fn try_from(value: u16) -> Result<Self, Self::Error> {
        match value {
            1 => Ok(Self::Anonymous),
            2 => Ok(Self::Heap),
            3 => Ok(Self::Stack),
            4 => Ok(Self::FilePrivate),
            5 => Ok(Self::FileShared),
            _ => Err(CheckpointError::InvalidEnum),
        }
    }
}

// AGENT: convert serialized fd kind fields into checked enums.
impl TryFrom<u16> for SavedFdKind {
    type Error = CheckpointError;

    // AGENT: map serialized fd kind values into the checkpoint enum.
    fn try_from(value: u16) -> Result<Self, Self::Error> {
        match value {
            1 => Ok(Self::Stdin),
            2 => Ok(Self::Stdout),
            3 => Ok(Self::Stderr),
            4 => Ok(Self::RegularMemoryFile),
            5 => Ok(Self::Pipe),
            6 => Ok(Self::Epoll),
            7 => Ok(Self::Socket),
            8 => Ok(Self::Tty),
            _ => Err(CheckpointError::InvalidEnum),
        }
    }
}
