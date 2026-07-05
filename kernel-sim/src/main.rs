// AGENT: use RuntimeKernel for the full simulator; root Kernel is the
// chaos-tests-compatible facade.
use kernel_sim::{RuntimeKernel, N_FRAMES, SYS_GETPID};

fn main() {
    let kernel = RuntimeKernel::new(N_FRAMES);
    kernel.proc_init();
    let pid = kernel
        .dispatch_syscall(SYS_GETPID, 0, 0, 0, 0, 0, 0)
        .expect("kernel-sim getpid syscall failed");
    println!("kernel-sim booted, root pid={pid}");
}
