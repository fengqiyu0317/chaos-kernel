use crate::kernel::kernel_core::prelude::*;

// AGENT: single logical 100 Hz clock shared by scheduler and timeout semantics.
pub static CLK: AtomicUsize = AtomicUsize::new(0);

// AGENT: only CPU0 advances global logical time; secondary CPUs must not make
// clock_gettime or timer deadlines run faster on an SMP system.
pub fn dtk(cpu_id: usize) {
    if cpu_id == 0 {
        CLK.fetch_add(1, Ordering::Relaxed);
    }
}
