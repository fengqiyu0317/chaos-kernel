// AGENT
// First-pass physical split of the standalone kernel simulation.
#![allow(unused, dead_code, non_upper_case_globals, non_camel_case_types, unused_assignments, unused_mut)]

include!("core/prelude.rs");
include!("core/sync.rs");
include!("mm/memory.rs");
include!("core/net.rs");
include!("mm/alloc.rs");
include!("fs/fs_misc.rs");
include!("fs/fd.rs");
include!("fs/pipe.rs");
include!("fs/epoll_tty.rs");
include!("fs/channel.rs");
include!("fs/page_cache.rs");
include!("fs/kobj.rs");
include!("fs/block_cache.rs");
include!("fs/mount_io_disk.rs");
include!("proc/ipc.rs");
include!("proc/process.rs");
include!("proc/signal.rs");
include!("core/time.rs");
include!("core/arch.rs");
include!("proc/sched.rs");
include!("proc/task.rs");
include!("core/kernel_base.rs");
include!("syscall/dispatch.rs");
include!("syscall/fs.rs");
include!("syscall/mm.rs");
include!("syscall/proc.rs");
include!("syscall/signal.rs");
include!("syscall/epoll.rs");
include!("syscall/time.rs");
include!("syscall/sync.rs");
include!("core/kernel_ops.rs");
include!("util/misc.rs");
include!("mm/address_space.rs");
include!("proc/wait.rs");
include!("proc/resource.rs");
include!("mm/bits.rs");
