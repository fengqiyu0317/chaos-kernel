#![allow(dead_code)]

use core::arch::global_asm;

use crate::{csr, println, sbi, timer};

global_asm!(include_str!("trap.S"));

unsafe extern "C" {
    fn __kernel_trap_entry();
    fn __user_trap_entry();
    fn __user_trap_return(frame: *const TrapFrame) -> !;
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

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum TrapOrigin {
    Kernel,
    User,
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

// AGENT: Install the direct-mode user trap vector that switches through sscratch.
pub fn init_user_trap_vector() {
    unsafe {
        csr::write_stvec(
            __user_trap_entry as *const () as usize,
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

// AGENT: Rust entry for traps taken while the kernel is already on a kernel stack.
#[no_mangle]
pub extern "C" fn rust_kernel_trap(frame: &mut TrapFrame) {
    handle_trap(frame, TrapOrigin::Kernel);
}

// AGENT: Rust entry for user traps after trap.S has switched from user sp via sscratch.
#[no_mangle]
pub extern "C" fn rust_user_trap(frame: &mut TrapFrame) {
    init_kernel_trap_vector();
    handle_trap(frame, TrapOrigin::User);
    init_user_trap_vector();
}

fn handle_trap(frame: &mut TrapFrame, origin: TrapOrigin) {
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
                "[kernel-qemu] unhandled {:?} trap cause={:?} sepc={:#x} stval={:#x}",
                origin, cause, frame.sepc, stval
            );
            sbi::shutdown();
        }
    }
}

// AGENT: Return to a prepared user trap frame; the frame must sit at kernel_stack_top - sizeof(TrapFrame).
pub unsafe fn enter_user_mode(frame: &TrapFrame) -> ! {
    init_user_trap_vector();
    unsafe { __user_trap_return(frame as *const TrapFrame) }
}

fn user_sstatus() -> usize {
    let mut value = csr::read_sstatus();
    value &= !csr::SSTATUS_SPP;
    value &= !csr::SSTATUS_SIE;
    value |= csr::SSTATUS_SPIE;
    value
}

// AGENT: TrapFrame helpers expose RISC-V syscall ABI slots.
impl TrapFrame {
    // AGENT: Build an empty trap frame for first entry into a user task.
    pub const fn new() -> Self {
        Self {
            regs: [0; 32],
            sstatus: 0,
            sepc: 0,
        }
    }

    // AGENT: Configure the frame so __user_trap_return sret enters U-mode at entry with user_sp.
    pub fn prepare_user_entry(&mut self, entry: usize, user_sp: usize) {
        self.regs = [0; 32];
        self.regs[2] = user_sp;
        self.sstatus = user_sstatus();
        self.sepc = entry;
    }

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
