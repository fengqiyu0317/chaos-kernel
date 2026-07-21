#![allow(dead_code)]

use core::arch::global_asm;

use crate::kernel::{SavedTrapFrame, Task, PAGE_SZ, TRAMPOLINE, TRAP_CONTEXT};
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
    fn __user_trap_return(frame: *const TrapFrame, user_satp: usize) -> !;
    static strampoline: u8;
    static etrampoline: u8;
}

// AGENT: keep user register state followed by the runtime-only satp/frame
// handoff metadata that trap.S needs but checkpoint and signal ABIs must ignore.
#[derive(Clone, Debug, Eq, PartialEq)]
#[repr(C, align(16))]
pub struct TrapFrame {
    pub regs: [usize; 32],
    pub sstatus: usize,
    pub sepc: usize,
    kernel_satp: usize,
    user_satp: usize,
    kernel_frame: usize,
    user_frame: usize,
    trap_handler: usize,
}

// AGENT: pin every Rust field to the offsets consumed by trap.S and keep the
// expanded frame 16-byte aligned when carved from a task or kernel stack.
const _: () = {
    assert!(core::mem::offset_of!(TrapFrame, sstatus) == 256);
    assert!(core::mem::offset_of!(TrapFrame, sepc) == 264);
    assert!(core::mem::offset_of!(TrapFrame, kernel_satp) == 272);
    assert!(core::mem::offset_of!(TrapFrame, user_satp) == 280);
    assert!(core::mem::offset_of!(TrapFrame, kernel_frame) == 288);
    assert!(core::mem::offset_of!(TrapFrame, user_frame) == 296);
    assert!(core::mem::offset_of!(TrapFrame, trap_handler) == 304);
    assert!(core::mem::align_of::<TrapFrame>() == 16);
    assert!(core::mem::size_of::<TrapFrame>() == 320);
};

// AGENT: expose the page-aligned low-linked physical identity used to install
// the trampoline into the kernel root and each process root.
pub fn trampoline_paddr() -> usize {
    let start = core::ptr::addr_of!(strampoline) as usize;
    let end = core::ptr::addr_of!(etrampoline) as usize;
    assert_eq!(start % PAGE_SZ, 0, "trampoline must be page aligned");
    assert!(end >= start && end - start <= PAGE_SZ);
    start
}

// AGENT: translate one low-linked symbol inside the trampoline page to the
// fixed virtual alias shared by the kernel and user page-table roots.
fn trampoline_alias(symbol: usize) -> usize {
    let start = trampoline_paddr();
    let end = core::ptr::addr_of!(etrampoline) as usize;
    assert!(symbol >= start && symbol < end);
    TRAMPOLINE + (symbol - start)
}

// AGENT: publish the exact supervisor trap-vector address rather than the
// low-linked symbol that disappears after the user satp becomes active.
fn user_trap_entry_va() -> usize {
    trampoline_alias(__user_trap_entry as *const () as usize)
}

// AGENT: compute the never-returning trampoline alias used when Rust initiates
// the first user return; trampoline-internal returns use a page-local jump.
fn user_trap_return_va() -> usize {
    trampoline_alias(__user_trap_return as *const () as usize)
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

    // AGENT: convert the architecture-local fault class into the Kernel
    // boundary type before invoking migrated memory semantics.
    fn into_kernel_access(self) -> crate::kernel::KernelPageFaultAccess {
        match self {
            Self::Instruction => crate::kernel::KernelPageFaultAccess::Instruction,
            Self::Load => crate::kernel::KernelPageFaultAccess::Load,
            Self::Store => crate::kernel::KernelPageFaultAccess::Store,
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
        csr::write_stvec(user_trap_entry_va(), csr::STVEC_MODE_DIRECT);
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
    let Some(kernel) = crate::kernel::qemu_wait_kernel() else {
        println!("[kernel-qemu] user trap return has no installed kernel");
        sbi::shutdown();
    };
    let Some(task) = kernel.cur_task(0) else {
        println!("[kernel-qemu] user trap return has no CPU0 task");
        sbi::shutdown();
    };
    if let Err(err) = prepare_user_return(&task, frame) {
        println!(
            "[kernel-qemu] cannot prepare task {} user return: {}",
            task.id(),
            err
        );
        sbi::shutdown();
    }
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

// AGENT: route recoverable user page faults through Kernel memory semantics,
// while keeping unrecoverable early faults on the structured fatal path.
fn handle_page_fault(frame: &TrapFrame, origin: TrapOrigin, scause: usize, stval: usize) {
    let cause = decode_scause(scause);
    let access = match cause {
        TrapCause::Exception(code) => PageFaultAccess::from_exception_code(code),
        TrapCause::Interrupt(_) => None,
    };
    let Some(access) = access else {
        fail_trap(frame, origin, FatalTrap::Unhandled { cause }, stval);
    };
    if origin == TrapOrigin::User && recover_user_page_fault(stval, access).is_ok() {
        return;
    }
    fail_trap(frame, origin, FatalTrap::PageFault { access, cause }, stval);
}

// AGENT: keep trap recovery as an architecture dispatch step; migrated memory
// semantics stay behind Kernel::handle_pgfault.
fn recover_user_page_fault(addr: usize, access: PageFaultAccess) -> Result<(), &'static str> {
    let kernel = crate::kernel::qemu_wait_kernel().ok_or("esrch")?;
    kernel.handle_pgfault(addr, access.into_kernel_access())
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

// AGENT: bind the selected CPU0 task's stack page into its current process root
// and refresh every runtime-only address after fork, exec, or a prior task ran.
fn prepare_user_return(task: &Task, frame: &mut TrapFrame) -> Result<(), &'static str> {
    let kernel = crate::kernel::qemu_wait_kernel().ok_or("esrch")?;
    let kernel_frame = frame as *mut TrapFrame as usize;
    let frame_offset = kernel_frame & (PAGE_SZ - 1);
    if frame_offset + core::mem::size_of::<TrapFrame>() > PAGE_SZ {
        return Err("ekstk");
    }
    let trap_context_paddr = task.user_trap_frame_page_paddr()?;
    let user_satp = task
        .process
        .addr_space
        .lock()
        .unwrap()
        .bind_cpu0_user_trap(trampoline_paddr(), trap_context_paddr, &kernel.pool)?;
    let kernel_satp = csr::read_satp();
    if kernel_satp == 0 || kernel_satp == user_satp {
        return Err("esatp");
    }

    frame.kernel_satp = kernel_satp;
    frame.user_satp = user_satp;
    frame.kernel_frame = kernel_frame;
    frame.user_frame = TRAP_CONTEXT + frame_offset;
    frame.trap_handler = rust_user_trap as *const () as usize;
    init_user_trap_vector();
    Ok(())
}

// AGENT: enter the fixed trampoline alias with interrupts disabled; only that
// dual-mapped page may install user_satp before restoring registers and sret.
pub unsafe fn enter_user_mode(frame: &mut TrapFrame) -> ! {
    type UserTrapReturn = unsafe extern "C" fn(*const TrapFrame, usize) -> !;
    let return_entry: UserTrapReturn = unsafe { core::mem::transmute(user_trap_return_va()) };
    let user_frame = frame.user_frame as *const TrapFrame;
    let user_satp = frame.user_satp;
    csr::disable_interrupts();
    unsafe { return_entry(user_frame, user_satp) }
}

// AGENT: return the complete user frame already installed in the fixed top slot
// of this task's owned kernel stack.
pub unsafe fn prepare_task_user_trap_frame(task: &Task) -> Result<*mut TrapFrame, &'static str> {
    let frame = task.user_trap_frame_ptr()?;
    if unsafe { (*frame).sepc == 0 || (*frame).regs[2] == 0 } {
        return Err("enoexec");
    }
    Ok(frame)
}

// AGENT: enter a task's first user frame through the same trap-return path used
// after later user traps.
pub unsafe fn enter_task_user_mode(task: &Task) -> ! {
    match unsafe { prepare_task_user_trap_frame(task) } {
        Ok(frame) => {
            let frame = unsafe { &mut *frame };
            if let Err(err) = prepare_user_return(task, frame) {
                println!(
                    "[kernel-qemu] cannot prepare task {} first user entry: {}",
                    task.id(),
                    err
                );
                sbi::shutdown();
            }
            unsafe { enter_user_mode(frame) }
        }
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
            kernel_satp: 0,
            user_satp: 0,
            kernel_frame: 0,
            user_frame: 0,
            trap_handler: 0,
        }
    }

    // AGENT: Configure the frame so __user_trap_return sret enters U-mode at entry with user_sp.
    pub fn prepare_user_entry(&mut self, entry: usize, user_sp: usize) {
        self.regs = [0; 32];
        self.regs[2] = user_sp;
        self.sstatus = user_sstatus();
        self.sepc = entry;
        self.clear_runtime_handoff();
    }

    // AGENT: discard process- and stack-specific pointers whenever a restored
    // or exec-replaced user image installs new architectural register state.
    fn clear_runtime_handoff(&mut self) {
        self.kernel_satp = 0;
        self.user_satp = 0;
        self.kernel_frame = 0;
        self.user_frame = 0;
        self.trap_handler = 0;
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

    // AGENT: construct one complete first-entry frame so task creation and exec
    // install the same architecture state that trap.S later saves in place.
    pub fn for_user_entry(entry: usize, user_sp: usize) -> Self {
        let mut frame = Self::new();
        frame.prepare_user_entry(entry, user_sp);
        frame
    }

    // AGENT: capture the complete RISC-V user trap frame for checkpoint images.
    pub fn to_saved_checkpoint_frame(&self) -> SavedTrapFrame {
        let mut regs = [0u64; 32];
        for (dst, src) in regs.iter_mut().zip(self.regs.iter()) {
            *dst = *src as u64;
        }
        SavedTrapFrame {
            regs,
            sstatus: self.sstatus as u64,
            sepc: self.sepc as u64,
        }
    }

    // AGENT: materialize a restored checkpoint frame for the normal trap-return path.
    pub fn from_saved_checkpoint_frame(saved: &SavedTrapFrame) -> Self {
        let mut regs = [0usize; 32];
        for (dst, src) in regs.iter_mut().zip(saved.regs.iter()) {
            *dst = *src as usize;
        }
        Self {
            regs,
            sstatus: saved.sstatus as usize,
            sepc: saved.sepc as usize,
            kernel_satp: 0,
            user_satp: 0,
            kernel_frame: 0,
            user_frame: 0,
            trap_handler: 0,
        }
    }
}

// AGENT: keep the real user-satp transition regression behind the scheduler
// QEMU feature because it consumes a task through idle -> U-mode -> idle.
#[cfg(any(test, feature = "qemu-sched-selftest"))]
pub mod tests {
    use super::*;
    use crate::kernel::{
        install_qemu_wait_kernel, FramePool, Kernel, VmRegion, VM_EXEC, VM_READ, VM_WRITE,
    };
    use alloc::boxed::Box;

    const USER_CODE: usize = 0x0001_0000;
    const USER_STACK: usize = 0x0002_0000;

    // AGENT: execute real RISC-V user instructions under a process satp, return
    // to U-mode after getpid, then prove SYS_EXIT reaches idle on kernel satp
    // before the idle side releases the task's kernel stack.
    pub fn user_satp_exit_round_trip(pool: &FramePool) {
        let kernel_satp = csr::read_satp();
        let kernel = Box::leak(Box::new(Kernel::new(pool.clone())));
        kernel.proc_init();
        let task = kernel.cur_task(0).expect("init task should be current");

        let instructions = [
            0x0ac0_0893u32, // addi a7, zero, 172 (getpid)
            0x0000_0073u32, // ecall and return to the following user PC
            0x0000_0513u32, // addi a0, zero, 0
            0x05d0_0893u32, // addi a7, zero, 93 (exit)
            0x0000_0073u32, // ecall
            0x0000_006fu32, // jal zero, 0 (must never be reached)
        ];
        let mut code = [0u8; 24];
        for (chunk, instruction) in code.chunks_exact_mut(4).zip(instructions) {
            chunk.copy_from_slice(&instruction.to_le_bytes());
        }

        {
            let mut addr_space = task.process.addr_space.lock().unwrap();
            addr_space
                .map_region(
                    VmRegion::new(USER_CODE, PAGE_SZ, VM_READ | VM_WRITE | VM_EXEC),
                    pool,
                )
                .expect("user exit code page should map");
            addr_space
                .write_user_bytes(USER_CODE, &code, pool)
                .expect("user exit code should copy");
            addr_space
                .protect(USER_CODE, PAGE_SZ, VM_READ | VM_EXEC)
                .expect("user exit code should become read-execute");
            addr_space
                .map_region(VmRegion::new(USER_STACK, PAGE_SZ, VM_READ | VM_WRITE), pool)
                .expect("user stack page should map");
        }
        task.install_user_trap_frame(TrapFrame::for_user_entry(USER_CODE, USER_STACK + PAGE_SZ))
            .expect("user exit frame should install");
        install_qemu_wait_kernel(kernel);

        assert!(kernel.run_one_cpu0_task_for_test());
        assert_eq!(csr::read_satp(), kernel_satp);
        assert!(kernel.cur_task(0).is_none());
        assert!(task.done());
        assert!(task.kernel_stack_top().is_none());
    }
}
