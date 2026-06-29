#![feature(alloc_error_handler)]
#![no_std]
#![no_main]

extern crate alloc;

use alloc::boxed::Box;
use core::arch::global_asm;
use core::hint::spin_loop;
use core::panic::PanicInfo;
use core::sync::atomic::Ordering;

mod console;
mod csr;
mod heap;
mod irq_lock;
// AGENT: Directly connect the migrated kernel-sim module tree under the
// crate::kernel path expected by migrated modules; follow-up QEMU work can
// replace host-only pieces incrementally from compile errors.
#[allow(dead_code)]
#[path = "mod.rs"]
mod kernel;
#[allow(dead_code)]
#[path = "mm/bits.rs"]
mod mm_bits;
mod sbi;
mod semantics;
// AGENT: Keep the QEMU RISC-V syscall ABI adapter separate from the migrated kernel-sim syscall directory.
mod syscall_abi;
mod timer;
mod trap;

global_asm!(include_str!("entry.S"));

unsafe extern "C" {
    static mut sbss: u8;
    static mut ebss: u8;
}

// AGENT: First Rust entry point for the M9 QEMU carrier layer.
#[no_mangle]
pub extern "C" fn rust_main(hartid: usize, dtb_pa: usize) -> ! {
    clear_bss();
    heap::init();
    kernel::kernel_core::init_timer_wheel();

    println!("[kernel-qemu] boot hart={} dtb={:#x}", hartid, dtb_pa);
    heap::smoke_check();
    #[cfg(feature = "qemu-mm-selftest")]
    {
        println!("[kernel-qemu] mm bits selftest start");
        mm_bits::tests::run_all();
        println!("[kernel-qemu] mm bits selftest passed");
    }
    // AGENT: mirror the MM optional boot self-test path for QEMU sync primitives.
    #[cfg(feature = "qemu-sync-selftest")]
    {
        println!("[kernel-qemu] sync selftest start");
        kernel::kernel_core::sync::tests::run_all();
        println!("[kernel-qemu] sync selftest passed");
    }
    let _kernel = init_qemu_kernel_backend();
    let timer_probe = arm_timer_wheel_probe();
    trap::init_kernel_trap_vector();
    println!(
        "[kernel-qemu] trap vector installed stvec={:#x}",
        csr::read_stvec()
    );
    timer::init();
    wait_for_first_timer_tick();
    wait_for_timer_wheel_probe(&timer_probe);
    println!("[kernel-qemu] minimal carrier only; kernel-sim semantics not loaded");
    println!("[kernel-qemu] shutdown");

    sbi::shutdown()
}

// AGENT: Install a leaked Kernel as the QEMU scheduler/timer backend so real
// timer interrupts can drive migrated Kernel::schedule_tick() state.
fn init_qemu_kernel_backend() -> &'static kernel::Kernel {
    let kernel = Box::leak(Box::new(kernel::Kernel::new(kernel::N_FRAMES)));
    kernel.proc_init();
    kernel::install_qemu_wait_kernel(kernel);
    let current = kernel.cur_task(0).map(|task| task.id()).unwrap_or(0);
    println!(
        "[kernel-qemu] kernel timer backend installed current_task={}",
        current
    );
    kernel
}

// AGENT: Arm a one-tick logical timer target before hardware timer interrupts
// are enabled; the real interrupt path must expire it through schedule_tick().
fn arm_timer_wheel_probe() -> kernel::WaitToken {
    let token = kernel::WaitToken::current();
    let deadline = kernel::CLK.load(Ordering::Relaxed).saturating_add(1);
    let timer_id = {
        let mut timers = kernel::global_timer_wheel().lock();
        timers.register_timer(
            deadline,
            0,
            kernel::TimerTarget::WakeToken {
                token: token.clone(),
            },
        )
    };
    println!(
        "[kernel-qemu] timer wheel target armed id={} deadline={}",
        timer_id, deadline
    );
    token
}

// AGENT: Clear .bss before any later Rust state is introduced.
fn clear_bss() {
    unsafe {
        let mut cur = core::ptr::addr_of_mut!(sbss) as usize;
        let end = core::ptr::addr_of_mut!(ebss) as usize;
        while cur < end {
            (cur as *mut u8).write_volatile(0);
            cur += 1;
        }
    }
}

// AGENT: Smoke-check that the early S-mode trap vector receives a real timer interrupt.
fn wait_for_first_timer_tick() {
    let start = csr::read_time();
    let timeout_cycles = timer::CYCLES_PER_TICK * 20;
    while timer::ticks() == 0 && csr::read_time().wrapping_sub(start) < timeout_cycles {
        spin_loop();
    }

    let ticks = timer::ticks();
    if ticks == 0 {
        println!("[kernel-qemu] timer tick not observed");
    } else {
        println!("[kernel-qemu] timer tick observed ticks={}", ticks);
    }
}

// AGENT: Smoke-check that a real QEMU timer interrupt also advances the migrated
// Kernel timer wheel, not just the carrier-layer tick counter.
fn wait_for_timer_wheel_probe(token: &kernel::WaitToken) {
    let start = csr::read_time();
    let timeout_cycles = timer::CYCLES_PER_TICK * 20;
    while !token.is_timeout() && csr::read_time().wrapping_sub(start) < timeout_cycles {
        spin_loop();
    }

    let clk = kernel::CLK.load(Ordering::Relaxed);
    let active = kernel::global_timer_wheel().lock().active_count();
    if token.is_timeout() {
        println!(
            "[kernel-qemu] timer wheel target observed clk={} active={}",
            clk, active
        );
    } else {
        println!(
            "[kernel-qemu] timer wheel target not observed clk={} active={}",
            clk, active
        );
    }
}

// AGENT: Keep panic output observable in QEMU before powering off.
#[panic_handler]
fn panic(info: &PanicInfo<'_>) -> ! {
    println!("[kernel-qemu] panic: {}", info);
    sbi::shutdown()
}
