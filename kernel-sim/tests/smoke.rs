// AGENT
use kernel_sim::{Kernel, N_FRAMES, SYS_GETPID};

#[test]
fn boot_kernel_in_standalone_runtime() {
    let kernel = Kernel::new(N_FRAMES);
    kernel.proc_init();

    let pid = kernel
        .dispatch_syscall(SYS_GETPID, 0, 0, 0, 0, 0, 0)
        .expect("getpid should succeed in standalone runtime");

    assert_eq!(pid, 1);
}
