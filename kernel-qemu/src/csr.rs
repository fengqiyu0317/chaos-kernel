#![allow(dead_code)]

use core::arch::asm;

pub const STVEC_MODE_DIRECT: usize = 0;
pub const SCAUSE_INTERRUPT_BIT: usize = 1usize << (usize::BITS as usize - 1);
pub const SCAUSE_CODE_MASK: usize = !SCAUSE_INTERRUPT_BIT;
pub const SIE_STIE: usize = 1 << 5;
pub const SSTATUS_SIE: usize = 1 << 1;
pub const SSTATUS_SPIE: usize = 1 << 5;
pub const SSTATUS_SPP: usize = 1 << 8;
pub const SATP_MODE_SV39: usize = 8usize << 60;

// AGENT: Read the current trap vector base and mode from stvec.
#[inline]
pub fn read_stvec() -> usize {
    let value: usize;
    unsafe {
        asm!("csrr {}, stvec", out(reg) value, options(nomem, nostack));
    }
    value
}

// AGENT: Install an S-mode trap vector with an explicit stvec mode.
#[inline]
pub unsafe fn write_stvec(base: usize, mode: usize) {
    let value = (base & !0b11) | (mode & 0b11);
    unsafe {
        asm!("csrw stvec, {}", in(reg) value, options(nomem, nostack));
    }
}

// AGENT: Read the saved exception PC for the active trap.
#[inline]
pub fn read_sepc() -> usize {
    let value: usize;
    unsafe {
        asm!("csrr {}, sepc", out(reg) value, options(nomem, nostack));
    }
    value
}

// AGENT: Write the saved exception PC used by the next sret.
#[inline]
pub unsafe fn write_sepc(value: usize) {
    unsafe {
        asm!("csrw sepc, {}", in(reg) value, options(nomem, nostack));
    }
}

// AGENT: Read supervisor status for trap save/restore code.
#[inline]
pub fn read_sstatus() -> usize {
    let value: usize;
    unsafe {
        asm!("csrr {}, sstatus", out(reg) value, options(nomem, nostack));
    }
    value
}

// AGENT: Restore supervisor status before returning from a trap.
#[inline]
pub unsafe fn write_sstatus(value: usize) {
    unsafe {
        asm!("csrw sstatus, {}", in(reg) value, options(nomem, nostack));
    }
}

// AGENT: Set selected supervisor status bits such as SIE.
#[inline]
pub unsafe fn set_sstatus_bits(bits: usize) {
    unsafe {
        asm!("csrs sstatus, {}", in(reg) bits, options(nomem, nostack));
    }
}

// AGENT: Clear selected supervisor status bits such as SIE.
#[inline]
pub unsafe fn clear_sstatus_bits(bits: usize) {
    unsafe {
        asm!("csrc sstatus, {}", in(reg) bits, options(nomem, nostack));
    }
}

// AGENT: mask local S-mode interrupts around CPU0 scheduler selection and
// context publication.
#[inline]
pub fn disable_interrupts() {
    unsafe {
        clear_sstatus_bits(SSTATUS_SIE);
    }
}

// AGENT: let the idle CPU receive timer wakeups only after it has published
// current=None and left all scheduler locks.
#[inline]
pub fn enable_interrupts() {
    unsafe {
        set_sstatus_bits(SSTATUS_SIE);
    }
}

// AGENT: Read the raw trap cause register for Rust-side trap dispatch.
#[inline]
pub fn read_scause() -> usize {
    let value: usize;
    unsafe {
        asm!("csrr {}, scause", out(reg) value, options(nomem, nostack));
    }
    value
}

// AGENT: Read the trap value register, usually faulting virtual address or instruction bits.
#[inline]
pub fn read_stval() -> usize {
    let value: usize;
    unsafe {
        asm!("csrr {}, stval", out(reg) value, options(nomem, nostack));
    }
    value
}

// AGENT: Read supervisor interrupt-enable bits.
#[inline]
pub fn read_sie() -> usize {
    let value: usize;
    unsafe {
        asm!("csrr {}, sie", out(reg) value, options(nomem, nostack));
    }
    value
}

// AGENT: Set selected supervisor interrupt-enable bits such as STIE.
#[inline]
pub unsafe fn set_sie_bits(bits: usize) {
    unsafe {
        asm!("csrs sie, {}", in(reg) bits, options(nomem, nostack));
    }
}

// AGENT: Clear selected supervisor interrupt-enable bits such as STIE.
#[inline]
pub unsafe fn clear_sie_bits(bits: usize) {
    unsafe {
        asm!("csrc sie, {}", in(reg) bits, options(nomem, nostack));
    }
}

// AGENT: Read sscratch for later user/kernel stack handoff checks.
#[inline]
pub fn read_sscratch() -> usize {
    let value: usize;
    unsafe {
        asm!("csrr {}, sscratch", out(reg) value, options(nomem, nostack));
    }
    value
}

// AGENT: Write sscratch, which later user traps will use to find kernel trap state.
#[inline]
pub unsafe fn write_sscratch(value: usize) {
    unsafe {
        asm!("csrw sscratch, {}", in(reg) value, options(nomem, nostack));
    }
}

// AGENT: Read the active address-translation mode and root PPN.
#[inline]
pub fn read_satp() -> usize {
    let value: usize;
    unsafe {
        asm!("csrr {}, satp", out(reg) value, options(nomem, nostack));
    }
    value
}

// AGENT: Install a prepared satp value; callers must issue sfence.vma after
// changing page-table roots or permissions.
#[inline]
pub unsafe fn write_satp(value: usize) {
    unsafe {
        asm!("csrw satp, {}", in(reg) value, options(nomem, nostack));
    }
}

// AGENT: Flush all local address translations after modifying Sv39 tables.
#[inline]
pub fn sfence_vma() {
    unsafe {
        asm!("sfence.vma", options(nomem, nostack));
    }
}

// AGENT: Build an Sv39 satp value from a page-table root physical address.
#[inline]
pub fn make_satp_sv39(root_paddr: usize) -> usize {
    SATP_MODE_SV39 | (root_paddr >> 12)
}

// AGENT: Read the platform time CSR used to schedule the next SBI timer event.
#[inline]
pub fn read_time() -> usize {
    let value: usize;
    unsafe {
        asm!("rdtime {}", out(reg) value, options(nomem, nostack));
    }
    value
}
