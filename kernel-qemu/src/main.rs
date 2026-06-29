#![feature(alloc_error_handler)]
#![no_std]
#![no_main]

extern crate alloc;

use core::arch::global_asm;
use core::hint::spin_loop;
use core::panic::PanicInfo;

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
    trap::init_kernel_trap_vector();
    println!(
        "[kernel-qemu] trap vector installed stvec={:#x}",
        csr::read_stvec()
    );
    timer::init();
    wait_for_first_timer_tick();
    println!("[kernel-qemu] minimal carrier only; kernel-sim semantics not loaded");
    println!("[kernel-qemu] shutdown");

    sbi::shutdown()
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

// AGENT: Keep panic output observable in QEMU before powering off.
#[panic_handler]
fn panic(info: &PanicInfo<'_>) -> ! {
    println!("[kernel-qemu] panic: {}", info);
    sbi::shutdown()
}
