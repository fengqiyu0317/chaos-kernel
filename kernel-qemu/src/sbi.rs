use core::arch::asm;

const SBI_LEGACY_CONSOLE_PUTCHAR: usize = 1;
const SBI_LEGACY_SHUTDOWN: usize = 8;
const SBI_LEGACY_SET_TIMER: usize = 0;

// AGENT: Emit one byte through the legacy SBI console used by OpenSBI/QEMU.
pub fn console_putchar(byte: u8) {
    unsafe {
        legacy_call(SBI_LEGACY_CONSOLE_PUTCHAR, byte as usize);
    }
}

// AGENT: Program the next timer interrupt through the legacy SBI timer extension.
pub fn set_timer(stime_value: u64) {
    unsafe {
        legacy_call(SBI_LEGACY_SET_TIMER, stime_value as usize);
    }
}

// AGENT: sleep on the boot/idle stack until an enabled interrupt arrives;
// callers own the interrupt-enable ordering around this instruction.
pub fn wait_for_interrupt() {
    unsafe {
        asm!("wfi", options(nomem, nostack));
    }
}

// AGENT: Ask OpenSBI to terminate the QEMU machine, then idle if it returns.
pub fn shutdown() -> ! {
    unsafe {
        legacy_call(SBI_LEGACY_SHUTDOWN, 0);
    }

    loop {
        unsafe {
            asm!("wfi", options(nomem, nostack));
        }
    }
}

// AGENT: Minimal legacy SBI ecall wrapper; later milestones should move to SBI v0.2 where needed.
unsafe fn legacy_call(which: usize, arg0: usize) -> usize {
    let ret: usize;
    asm!(
        "ecall",
        inlateout("a0") arg0 => ret,
        in("a7") which,
        options(nostack)
    );
    ret
}
