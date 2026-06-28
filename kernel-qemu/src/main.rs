#![no_std]
#![no_main]

use core::arch::global_asm;
use core::hint::spin_loop;
use core::panic::PanicInfo;

mod console;
mod csr;
mod sbi;
mod syscall;
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

    println!("[kernel-qemu] boot hart={} dtb={:#x}", hartid, dtb_pa);
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
