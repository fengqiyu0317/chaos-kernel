// AGENT
use super::*;

pub mod access;
pub mod address_space;
pub mod alloc;
pub mod bits;
// AGENT: expose the experimental buddy allocator as a focused MM module while
// preserving the existing flat re-exported API.
pub mod buddy;
pub mod direct_map;
pub mod frame;
pub mod heap;
pub mod kernel_stack;
// AGENT: keep resident page-table metadata separate from AddrSpace operations.
pub mod page_table;
pub mod sv39;
pub mod vm_map;

pub use self::access::*;
pub use self::address_space::*;
pub use self::alloc::*;
pub use self::bits::*;
pub use self::buddy::*;
pub use self::direct_map::*;
pub use self::frame::*;
pub use self::heap::*;
pub use self::kernel_stack::*;
pub use self::page_table::*;
pub use self::sv39::*;
pub use self::vm_map::*;
