// AGENT
use super::*;

// AGENT: keep executable-format parsing with process image construction rather
// than treating ELF metadata as filesystem state.
pub mod elf;
pub mod ipc;
pub mod process;
// AGENT: expose process-stack regressions to optional QEMU boot selftests.
#[cfg(any(test, feature = "qemu-proc-selftest"))]
pub mod process_tests;
pub mod sched;
pub mod signal;
pub mod task;
// AGENT: share one transactional ELF/address-space builder between initial
// task creation and exec.
pub mod user_image;
pub mod wait;

pub use self::elf::*;
pub use self::ipc::*;
pub use self::process::*;
pub use self::sched::*;
pub use self::signal::*;
pub use self::task::*;
pub use self::user_image::*;
pub use self::wait::*;
