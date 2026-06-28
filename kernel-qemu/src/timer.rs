#![allow(dead_code)]

use core::sync::atomic::{AtomicUsize, Ordering};

use crate::{csr, sbi};

pub const TIMEBASE_HZ: usize = 10_000_000;
pub const TICKS_PER_SEC: usize = 100;
pub const CYCLES_PER_TICK: usize = TIMEBASE_HZ / TICKS_PER_SEC;

static TICKS: AtomicUsize = AtomicUsize::new(0);

// AGENT: Enable S-mode timer interrupts and arm the first QEMU timer event.
pub fn init() {
    unsafe {
        csr::set_sie_bits(csr::SIE_STIE);
        csr::set_sstatus_bits(csr::SSTATUS_SIE);
    }
    schedule_next_tick();
}

// AGENT: Advance the kernel-qemu tick counter after an S-mode timer interrupt.
pub fn on_timer_interrupt() {
    TICKS.fetch_add(1, Ordering::Relaxed);
    schedule_next_tick();
}

// AGENT: Return the current bare-metal tick count for smoke tests and later schedulers.
pub fn ticks() -> usize {
    TICKS.load(Ordering::Relaxed)
}

// AGENT: Program the next timer interrupt through OpenSBI.
pub fn schedule_next_tick() {
    let next = csr::read_time().wrapping_add(CYCLES_PER_TICK);
    sbi::set_timer(next as u64);
}
