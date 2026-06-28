#![allow(dead_code)]

use core::arch::global_asm;

use crate::{csr, println, sbi, timer};

global_asm!(include_str!("trap.S"));

unsafe extern "C" {
    fn __kernel_trap_entry();
}

// AGENT: RISC-V S-mode trap frame saved by trap.S before entering Rust.
#[repr(C)]
pub struct TrapFrame {
    pub regs: [usize; 32],
    pub sstatus: usize,
    pub sepc: usize,
}

// AGENT: Small decoded view of scause used by the early trap dispatcher.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TrapCause {
    Interrupt(usize),
    Exception(usize),
}

// AGENT: Install the early direct-mode S-mode trap vector.
pub fn init_kernel_trap_vector() {
    unsafe {
        csr::write_stvec(
            __kernel_trap_entry as *const () as usize,
            csr::STVEC_MODE_DIRECT,
        );
    }
}

// AGENT: Decode scause into interrupt/exception form without assigning semantics yet.
pub fn decode_scause(scause: usize) -> TrapCause {
    let code = scause & csr::SCAUSE_CODE_MASK;
    if scause & csr::SCAUSE_INTERRUPT_BIT != 0 {
        TrapCause::Interrupt(code)
    } else {
        TrapCause::Exception(code)
    }
}

// AGENT: Rust entry called from trap.S after all general registers are saved.
#[no_mangle]
pub extern "C" fn rust_trap(frame: &mut TrapFrame) {
    let scause = csr::read_scause();
    let stval = csr::read_stval();
    match decode_scause(scause) {
        TrapCause::Interrupt(5) => {
            timer::on_timer_interrupt();
        }
        TrapCause::Exception(8) => {
            frame.sepc = frame.sepc.wrapping_add(4);
            let request = crate::syscall::decode_from_trap_frame(frame);
            let _ = request;
            crate::syscall::write_return(frame, crate::syscall::ENOSYS_RET);
        }
        cause => {
            println!(
                "[kernel-qemu] unhandled trap cause={:?} sepc={:#x} stval={:#x}",
                cause, frame.sepc, stval
            );
            sbi::shutdown();
        }
    }
}

// AGENT: TrapFrame helpers expose RISC-V syscall ABI slots.
impl TrapFrame {
    // AGENT: Read syscall number from the RISC-V a7 slot.
    pub fn syscall_nr(&self) -> usize {
        self.regs[17]
    }

    // AGENT: Return the six RISC-V syscall arguments from a0 through a5.
    pub fn syscall_args(&self) -> [usize; 6] {
        [
            self.regs[10],
            self.regs[11],
            self.regs[12],
            self.regs[13],
            self.regs[14],
            self.regs[15],
        ]
    }

    // AGENT: Write the syscall return value into a0.
    pub fn set_return_value(&mut self, value: usize) {
        self.regs[10] = value;
    }
}
