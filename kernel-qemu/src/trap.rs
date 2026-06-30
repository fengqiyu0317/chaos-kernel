#![allow(dead_code)]

use core::arch::global_asm;
use core::mem;

use crate::kernel::Task;
use crate::{csr, println, sbi, timer};

global_asm!(include_str!("trap.S"));

// AGENT: Architectural scause codes used by the early Rust trap dispatcher.
const INTERRUPT_SUPERVISOR_TIMER: usize = 5;
const EXCEPTION_ILLEGAL_INSTRUCTION: usize = 2;
const EXCEPTION_USER_ECALL: usize = 8;
const EXCEPTION_INSTRUCTION_PAGE_FAULT: usize = 12;
const EXCEPTION_LOAD_PAGE_FAULT: usize = 13;
const EXCEPTION_STORE_PAGE_FAULT: usize = 15;

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

// AGENT: Page fault access class derived from RISC-V synchronous exception codes.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum PageFaultAccess {
    Instruction,
    Load,
    Store,
}

impl PageFaultAccess {
    // AGENT: Keep page-fault cause decoding separate from the generic trap dispatcher.
    fn from_exception_code(code: usize) -> Option<Self> {
        match code {
            EXCEPTION_INSTRUCTION_PAGE_FAULT => Some(Self::Instruction),
            EXCEPTION_LOAD_PAGE_FAULT => Some(Self::Load),
            EXCEPTION_STORE_PAGE_FAULT => Some(Self::Store),
            _ => None,
        }
    }
}

// AGENT: Structured fatal trap categories for the early no-task-exit QEMU path.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum FatalTrap {
    PageFault {
        access: PageFaultAccess,
        cause: TrapCause,
    },
    IllegalInstruction,
    Unhandled {
        cause: TrapCause,
    },
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

// AGENT: Dispatch raw RISC-V trap causes to narrow handlers without defining syscall semantics.
fn handle_trap(frame: &mut TrapFrame, origin: TrapOrigin) {
    let scause = csr::read_scause();
    let stval = csr::read_stval();
    match decode_scause(scause) {
        TrapCause::Interrupt(INTERRUPT_SUPERVISOR_TIMER) => handle_timer_interrupt(),
        TrapCause::Exception(EXCEPTION_USER_ECALL) => handle_user_ecall(frame),
        TrapCause::Exception(EXCEPTION_ILLEGAL_INSTRUCTION) => {
            handle_illegal_instruction(frame, origin, stval)
        }
        TrapCause::Exception(
            EXCEPTION_INSTRUCTION_PAGE_FAULT
            | EXCEPTION_LOAD_PAGE_FAULT
            | EXCEPTION_STORE_PAGE_FAULT,
        ) => handle_page_fault(frame, origin, scause, stval),
        cause => handle_unhandled_trap(frame, origin, cause, stval),
    }
}

// AGENT: Timer interrupts advance only the QEMU-side tick source for now.
fn handle_timer_interrupt() {
    timer::on_timer_interrupt();
}

// AGENT: User ecall follows the RISC-V ABI boundary and leaves syscall semantics out of trap.rs.
fn handle_user_ecall(frame: &mut TrapFrame) {
    frame.sepc = frame.sepc.wrapping_add(4);
    crate::syscall_abi::dispatch_from_trap_frame(frame);
}

// AGENT: Early page faults fail with architectural context until Sv39/AddrSpace handling lands.
fn handle_page_fault(frame: &TrapFrame, origin: TrapOrigin, scause: usize, stval: usize) -> ! {
    let cause = decode_scause(scause);
    let access = match cause {
        TrapCause::Exception(code) => PageFaultAccess::from_exception_code(code),
        TrapCause::Interrupt(_) => None,
    };
    match access {
        Some(access) => fail_trap(frame, origin, FatalTrap::PageFault { access, cause }, stval),
        None => fail_trap(frame, origin, FatalTrap::Unhandled { cause }, stval),
    }
}

// AGENT: Illegal instructions are reported explicitly before the later per-task kill path exists.
fn handle_illegal_instruction(frame: &TrapFrame, origin: TrapOrigin, stval: usize) -> ! {
    fail_trap(frame, origin, FatalTrap::IllegalInstruction, stval)
}

// AGENT: Keep unexpected trap failures centralized so logs stay comparable across milestones.
fn handle_unhandled_trap(
    frame: &TrapFrame,
    origin: TrapOrigin,
    cause: TrapCause,
    stval: usize,
) -> ! {
    fail_trap(frame, origin, FatalTrap::Unhandled { cause }, stval)
}

// AGENT: Terminate early trap failures with enough context for QEMU smoke and handoff logs.
fn fail_trap(frame: &TrapFrame, origin: TrapOrigin, fatal: FatalTrap, stval: usize) -> ! {
    let sp = frame.regs[2];
    match fatal {
        FatalTrap::PageFault { access, cause } => println!(
            "[kernel-qemu] page fault origin={:?} access={:?} cause={:?} sepc={:#x} stval={:#x} sstatus={:#x} sp={:#x}",
            origin, access, cause, frame.sepc, stval, frame.sstatus, sp
        ),
        FatalTrap::IllegalInstruction => println!(
            "[kernel-qemu] illegal instruction origin={:?} sepc={:#x} stval={:#x} sstatus={:#x} sp={:#x}",
            origin, frame.sepc, stval, frame.sstatus, sp
        ),
        FatalTrap::Unhandled { cause } => println!(
            "[kernel-qemu] unhandled trap origin={:?} cause={:?} sepc={:#x} stval={:#x} sstatus={:#x} sp={:#x}",
            origin, cause, frame.sepc, stval, frame.sstatus, sp
        ),
    }
    println!(
        "[kernel-qemu] trap fallback action={}",
        early_fatal_trap_action(origin)
    );
    sbi::shutdown();
}

// AGENT: Document the current early failure policy until task exit and page fault recovery land.
fn early_fatal_trap_action(origin: TrapOrigin) -> &'static str {
    match origin {
        TrapOrigin::User => "shutdown-until-task-exit-is-migrated",
        TrapOrigin::Kernel => "shutdown-kernel-fault",
    }
}

// AGENT: Return to a prepared user trap frame; the frame must sit at kernel_stack_top - sizeof(TrapFrame).
pub unsafe fn enter_user_mode(frame: &TrapFrame) -> ! {
    init_user_trap_vector();
    unsafe { __user_trap_return(frame as *const TrapFrame) }
}

// AGENT: materialize a task's saved user context as a RISC-V trap frame at the
// top of its owned kernel stack.
pub unsafe fn prepare_task_user_trap_frame(task: &Task) -> Result<*mut TrapFrame, &'static str> {
    let stack_top = task.kernel_stack_top().ok_or("ekstk")?;
    let frame_addr = stack_top
        .checked_sub(mem::size_of::<TrapFrame>())
        .ok_or("ekstk")?;
    if frame_addr % mem::align_of::<TrapFrame>() != 0 {
        return Err("ekstk");
    }

    let (entry, user_sp) = {
        let thd = task.thd_ctx.lock().unwrap();
        let ctx = thd.as_ref().ok_or("enoctx")?;
        (
            ctx.uctx.ip as usize,
            ctx.uctx.r[crate::kernel::N_REGS - 1] as usize,
        )
    };
    if entry == 0 || user_sp == 0 {
        return Err("enoexec");
    }

    let frame = frame_addr as *mut TrapFrame;
    unsafe {
        frame.write(TrapFrame::new());
        (*frame).prepare_user_entry(entry, user_sp);
    }
    Ok(frame)
}

// AGENT: enter a task's first user frame through the same trap-return path used
// after later user traps.
pub unsafe fn enter_task_user_mode(task: &Task) -> ! {
    match unsafe { prepare_task_user_trap_frame(task) } {
        Ok(frame) => unsafe { enter_user_mode(&*frame) },
        Err(err) => {
            println!(
                "[kernel-qemu] cannot enter task {} user mode: {}",
                task.id(),
                err
            );
            sbi::shutdown();
        }
    }
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
