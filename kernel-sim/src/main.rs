// AGENT
use kernel_sim::{Kernel, N_FRAMES, SYS_GETPID};

fn main() {
    let kernel = Kernel::new(N_FRAMES);
    kernel.proc_init();
    let pid = kernel
        .dispatch_syscall(SYS_GETPID, 0, 0, 0, 0, 0, 0)
        .expect("kernel-sim getpid syscall failed");
    println!("kernel-sim booted, root pid={pid}");
}
