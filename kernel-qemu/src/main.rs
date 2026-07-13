#![feature(alloc_error_handler)]
#![no_std]
#![no_main]

extern crate alloc;

use alloc::boxed::Box;
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::arch::global_asm;
use core::hint::spin_loop;
use core::panic::PanicInfo;
use core::sync::atomic::Ordering;

use crate::irq_lock::IrqOnceCell;

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
    static ekernel: u8;
}

const QEMU_VIRT_RAM_START: usize = 0x8000_0000;
const QEMU_VIRT_RAM_END: usize = 0x8800_0000;

// AGENT: keep the active kernel Sv39 page table owned by a boot-global cell so
// its PgFrame-backed root and intermediate table pages are never returned while
// satp still points at them.
static KERNEL_PAGE_TABLE: IrqOnceCell<kernel::PageTable> = IrqOnceCell::new();

// AGENT: retain RAII ownership of every firmware and linked-kernel frame for
// the complete boot lifetime so ordinary FramePool allocation cannot reuse it.
static BOOT_RESERVED_FRAMES: IrqOnceCell<Vec<kernel::PgFrame>> = IrqOnceCell::new();

// AGENT: optional first-stage init ELF installed as the normal /bin/init file.
// Keep this empty until a real RISC-V user ELF is produced.
const ROOT_INIT_ELF: &[u8] = &[];

// AGENT: First Rust entry point for the M9 QEMU carrier layer.
#[no_mangle]
pub extern "C" fn rust_main(hartid: usize, dtb_pa: usize) -> ! {
    clear_bss();
    heap::init();
    let frame_pool = Arc::new(init_qemu_frame_pool());
    install_kernel_page_table(frame_pool.as_ref());
    heap::promote(frame_pool.clone());

    println!("[kernel-qemu] boot hart={} dtb={:#x}", hartid, dtb_pa);
    heap::smoke_check();
    kernel::kernel_core::init_timer_wheel();
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
    // AGENT: expose focused RunQueue regressions through the same optional
    // QEMU boot self-test path.
    #[cfg(feature = "qemu-sched-selftest")]
    {
        println!("[kernel-qemu] sched selftest start");
        kernel::proc::sched::tests::run_all();
        println!("[kernel-qemu] sched selftest passed");
    }
    let kernel = init_qemu_kernel_backend(frame_pool.as_ref().clone());
    // AGENT: run ProcInit stack-writing checks only after the real QEMU frame
    // pool and direct map are installed.
    #[cfg(feature = "qemu-proc-selftest")]
    {
        println!("[kernel-qemu] proc selftest start");
        kernel::proc::process_tests::run_all(&kernel.pool);
        println!("[kernel-qemu] proc selftest passed");
    }
    // AGENT: filesystem syscall selftests need the installed Kernel, current
    // task, frame pool, Sv39 mappings, and usercopy path.
    #[cfg(feature = "qemu-fs-selftest")]
    {
        println!("[kernel-qemu] fs syscall selftest start");
        kernel::syscall::tests::run_all(kernel);
        println!("[kernel-qemu] fs syscall selftest passed");
    }
    // AGENT: checkpoint selftests run after Kernel/FramePool setup because the
    // snapshot path copies real resident pages and restores them into a task.
    #[cfg(feature = "qemu-checkpoint-selftest")]
    {
        println!("[kernel-qemu] checkpoint selftest start");
        kernel::kernel_core::checkpoint_tests::run_all(kernel);
        println!("[kernel-qemu] checkpoint selftest passed");
    }
    // AGENT: keep the ordinary boot path warning-free while proc selftests are
    // feature-gated out.
    #[cfg(not(feature = "qemu-proc-selftest"))]
    let _ = kernel;
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
fn init_qemu_kernel_backend(frame_pool: kernel::FramePool) -> &'static kernel::Kernel {
    let root_block = Arc::new(kernel::RamBlockDevice::empty());
    let kernel = Box::leak(Box::new(kernel::Kernel::new_with_block_device(
        frame_pool, root_block,
    )));
    match install_embedded_root_init(kernel, ROOT_INIT_ELF) {
        Ok(true) => println!("[kernel-qemu] installed embedded /bin/init"),
        Ok(false) => println!("[kernel-qemu] no embedded /bin/init"),
        Err(err) => println!("[kernel-qemu] failed to install /bin/init: {}", err),
    }
    kernel.proc_init();
    kernel::install_qemu_wait_kernel(kernel);
    let current = kernel.cur_task(0).map(|task| task.id()).unwrap_or(0);
    println!(
        "[kernel-qemu] kernel timer backend installed current_task={}",
        current
    );
    kernel
}

// AGENT: install the linked init payload through the same path-backed file store
// used by exec and file handles, instead of treating it as raw block data.
fn install_embedded_root_init(
    kernel: &kernel::Kernel,
    init_elf: &[u8],
) -> Result<bool, &'static str> {
    if init_elf.is_empty() {
        return Ok(false);
    }
    kernel.install_exec_file("/bin/init", Vec::from(init_elf))?;
    Ok(true)
}

// AGENT: switch from bare addressing to an Sv39 root that keeps the current
// low-linked kernel alive while adding the high-half physical direct map.
fn install_kernel_page_table(frame_pool: &kernel::FramePool) {
    let page_table =
        kernel::build_kernel_page_table(frame_pool, QEMU_VIRT_RAM_START, QEMU_VIRT_RAM_END)
            .expect("kernel Sv39 page table should build");
    page_table
        .activate_kernel_direct_map()
        .expect("kernel Sv39 page table should activate");
    let satp = csr::read_satp();
    let direct_map = kernel::direct_map_active();
    if KERNEL_PAGE_TABLE.init(page_table).is_err() {
        panic!("kernel Sv39 page table installed more than once");
    }
    println!(
        "[kernel-qemu] kernel page table installed satp={:#x} direct_map={}",
        satp, direct_map
    );
}

// AGENT: account for the complete QEMU RAM span in FramePool, retain ordinary
// PgFrame handles for firmware plus the linked boot image, and leave later
// pages available to the live heap after the direct map is active.
fn init_qemu_frame_pool() -> kernel::FramePool {
    let total_pages = (QEMU_VIRT_RAM_END - QEMU_VIRT_RAM_START) / kernel::PAGE_SZ;
    let pool = kernel::FramePool::new(total_pages, QEMU_VIRT_RAM_START);
    let kernel_end = align_up_page(core::ptr::addr_of!(ekernel) as usize);
    let boot_reserved_pages = (kernel_end - QEMU_VIRT_RAM_START) / kernel::PAGE_SZ;
    let boot_reserved_frames = pool
        .alloc_pg_frames(boot_reserved_pages)
        .expect("QEMU RAM should contain the firmware and linked kernel");
    assert!(
        boot_reserved_frames
            .iter()
            .enumerate()
            .all(|(id, frame)| frame.id() == id),
        "fresh FramePool should allocate the boot prefix first"
    );
    if BOOT_RESERVED_FRAMES.init(boot_reserved_frames).is_err() {
        panic!("QEMU boot frames retained more than once");
    }
    println!(
        "[kernel-qemu] frame pool ready base={:#x} end={:#x} boot_reserved_pages={} free_pages={}",
        QEMU_VIRT_RAM_START,
        QEMU_VIRT_RAM_END,
        boot_reserved_pages,
        pool.free_count(),
    );
    pool
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

// AGENT: page-align linker symbols before seeding physical frame ranges.
fn align_up_page(addr: usize) -> usize {
    kernel::checked_align_up(addr, kernel::PAGE_SZ)
        .expect("linked kernel end address overflowed page alignment")
}

// AGENT: Keep panic output observable in QEMU before powering off.
#[panic_handler]
fn panic(info: &PanicInfo<'_>) -> ! {
    println!("[kernel-qemu] panic: {}", info);
    sbi::shutdown()
}
