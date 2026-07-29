// AGENT: keep filesystem-owned attributes independent from the RISC-V userspace
// stat byte layout so VFS and fd objects share one metadata source.

pub const S_IFMT: u32 = 0o170000;
pub const S_IFREG: u32 = 0o100000;
pub const S_IFDIR: u32 = 0o040000;

// AGENT: represent one stat timestamp even while the first ChaosFs metadata
// format has no persisted clock fields and therefore reports the zero value.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct FileTime {
    pub sec: i64,
    pub nsec: u64,
}

// AGENT: carry a complete first-stage stat snapshot without coupling live inode
// ownership to ABI padding, field offsets, or userspace pointer access.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct FileAttr {
    pub dev: u64,
    pub ino: u64,
    pub mode: u32,
    pub nlink: u32,
    pub uid: u32,
    pub gid: u32,
    pub rdev: u64,
    pub size: u64,
    pub block_size: u32,
    pub blocks: u64,
    pub atime: FileTime,
    pub mtime: FileTime,
    pub ctime: FileTime,
}
