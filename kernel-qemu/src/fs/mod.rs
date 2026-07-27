// AGENT
use super::*;

pub mod block_cache;
pub mod block_device;
pub mod channel;
pub mod circ_buf;
// AGENT: Keep the fault-injectable simulated disk separate from block-device
// backends, mount resolution, and request scheduling.
pub mod disk;
pub mod epoll;
pub mod fd;
pub mod fhandle;
pub mod file_node;
// AGENT: identify one mounted filesystem object without pathname or open state.
pub mod finstance;
pub mod flike;
// AGENT: own each filesystem-local node table and storage backend together.
pub mod fs_instance;
// AGENT: Isolate the block-I/O request queue from mount and disk behavior.
pub mod io_queue;
// AGENT: Isolate mount-table path resolution from I/O scheduling and devices.
pub mod mount;
pub mod pipe;
pub mod tty;
// AGENT: connect filesystem instances and mounts to resolved path identity.
pub mod vfs;

pub use self::block_cache::*;
pub use self::block_device::*;
pub use self::channel::*;
pub use self::circ_buf::*;
pub use self::disk::*;
pub use self::epoll::*;
pub use self::fd::*;
pub use self::fhandle::*;
pub use self::file_node::*;
pub use self::finstance::*;
pub use self::flike::*;
pub use self::fs_instance::*;
pub use self::io_queue::*;
pub use self::mount::*;
pub use self::pipe::*;
pub use self::tty::*;
pub use self::vfs::*;
