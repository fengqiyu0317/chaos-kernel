#![feature(alloc_error_handler)]
#![no_std]
#![no_main]

extern crate alloc;

use alloc::boxed::Box;
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::arch::global_asm;
#[cfg(feature = "qemu-boot-smoke")]
use core::hint::spin_loop;
use core::panic::PanicInfo;
#[cfg(feature = "qemu-boot-smoke")]
use core::sync::atomic::Ordering;

use crate::irq_lock::IrqOnceCell;

mod console;
mod context;
mod csr;
mod drivers;
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

// AGENT: embed the separately linked fixed-address RISC-V init ELF produced by
// build.rs and install it through the ordinary path-backed exec file store.
const ROOT_INIT_ELF: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/root-init"));
// AGENT: embed a distinct exec target so /bin/init can prove that a successful
// execve replaces its image instead of recursively executing itself.
const EXEC_SMOKE_ELF: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/exec-smoke"));

// AGENT: Enter the M9 QEMU carrier with cleared global state, then install the
// kernel trap vector before heap, frame, page-table, or Kernel initialization.
#[no_mangle]
pub extern "C" fn rust_main(hartid: usize, dtb_pa: usize) -> ! {
    clear_bss();
    trap::init_kernel_trap_vector();
    println!(
        "[kernel-qemu] trap vector installed stvec={:#x}",
        csr::read_stvec()
    );
    heap::init();
    let frame_pool = Arc::new(init_qemu_frame_pool());
    install_kernel_page_table(frame_pool.as_ref());
    heap::promote(frame_pool.clone());

    println!("[kernel-qemu] boot hart={} dtb={:#x}", hartid, dtb_pa);
    // AGENT: isolate the raw-sector persistence proof from FileStorage and the
    // still-non-mountable FileNode metadata format.
    #[cfg(feature = "qemu-virtio-blk-smoke")]
    run_virtio_blk_raw_smoke(frame_pool.as_ref().clone());
    // AGENT: keep recoverable ChaosFs validation separate from the raw-sector
    // transport smoke and exercise two boots through an explicit format/mount split.
    #[cfg(feature = "qemu-chaosfs-smoke")]
    run_chaosfs_persistence_smoke(frame_pool.as_ref().clone());
    #[cfg(feature = "qemu-boot-smoke")]
    heap::smoke_check();
    kernel::kernel_core::init_timer_wheel();
    #[cfg(feature = "qemu-mm-selftest")]
    {
        println!("[kernel-qemu] mm bits selftest start");
        mm_bits::tests::run_all();
        // AGENT: exercise strict Sv39 PTE classification in the existing MM
        // boot selftest after the kernel page table and direct map are active.
        kernel::mm::sv39::tests::run_all();
        println!("[kernel-qemu] mm bits selftest passed");
    }
    // AGENT: mirror the MM optional boot self-test path for QEMU sync primitives.
    #[cfg(feature = "qemu-sync-selftest")]
    {
        println!("[kernel-qemu] sync selftest start");
        kernel::kernel_core::sync::tests::run_all(frame_pool.as_ref());
        println!("[kernel-qemu] sync selftest passed");
    }
    // AGENT: expose focused RunQueue regressions through the same optional
    // QEMU boot self-test path.
    #[cfg(feature = "qemu-sched-selftest")]
    {
        println!("[kernel-qemu] context selftest start");
        context::tests::run_all();
        println!("[kernel-qemu] context selftest passed");
        println!("[kernel-qemu] sched selftest start");
        kernel::proc::sched::tests::run_all(frame_pool.as_ref());
        println!("[kernel-qemu] sched selftest passed");
    }
    // AGENT: run ProcInit stack-writing checks only after the real QEMU frame
    // pool and direct map are installed.
    #[cfg(feature = "qemu-proc-selftest")]
    {
        println!("[kernel-qemu] proc selftest start");
        kernel::proc::process_tests::run_all(frame_pool.as_ref());
        kernel::syscall::proc_tests::run_all(frame_pool.as_ref());
        println!("[kernel-qemu] proc selftest passed");
    }
    // AGENT: isolate selftests that intentionally mutate current-task, restored
    // process, and run-queue state from the production Kernel built below.
    #[cfg(any(feature = "qemu-fs-selftest", feature = "qemu-checkpoint-selftest"))]
    let selftest_kernel = init_qemu_selftest_backend(frame_pool.as_ref().clone());
    // AGENT: filesystem syscall selftests cover ABI errno encoding before using
    // the installed Kernel, current task, frame pool, Sv39 mappings, and usercopy.
    #[cfg(feature = "qemu-fs-selftest")]
    {
        println!("[kernel-qemu] fs syscall selftest start");
        syscall_abi::tests::run_all();
        kernel::syscall::tests::run_all(selftest_kernel);
        println!("[kernel-qemu] fs syscall selftest passed");
    }
    // AGENT: checkpoint selftests run after Kernel/FramePool setup because the
    // snapshot path copies real resident pages and restores them into a task.
    #[cfg(feature = "qemu-checkpoint-selftest")]
    {
        println!("[kernel-qemu] checkpoint selftest start");
        kernel::kernel_core::checkpoint_tests::run_all(selftest_kernel);
        println!("[kernel-qemu] checkpoint selftest passed");
    }
    // AGENT: validate the complete kernel-satp -> user-satp -> kernel-satp
    // trampoline path only after other installed-kernel state tests finish.
    #[cfg(feature = "qemu-sched-selftest")]
    {
        println!("[kernel-qemu] user satp selftest start");
        trap::tests::user_satp_exit_group_round_trip(frame_pool.as_ref());
        println!("[kernel-qemu] user signal round-trip selftest start");
        trap::tests::user_signal_round_trip(frame_pool.as_ref());
        println!("[kernel-qemu] user signal round-trip selftest passed");
        println!("[kernel-qemu] user satp selftest passed");
    }
    // AGENT: construct production task/process/run-queue state only after every
    // boot selftest has finished creating or scheduling disposable tasks.
    let (kernel, init_installed) = init_qemu_kernel_backend(frame_pool.as_ref().clone());
    // AGENT: build the authoritative init address space and user frame only
    // after boot selftests finish mutating disposable test state, so every
    // feature combination enters the same clean init image.
    let init_ready = prepare_root_init_task(kernel, init_installed);
    #[cfg(feature = "qemu-boot-smoke")]
    let timer_probe = arm_timer_wheel_probe();
    timer::init();
    #[cfg(feature = "qemu-boot-smoke")]
    {
        wait_for_first_timer_tick();
        wait_for_timer_wheel_probe(&timer_probe);
    }
    if init_ready {
        println!("[kernel-qemu] CPU0 scheduler start");
        kernel.run_cpu0();
    }
    // AGENT: run_cpu0() never returns, so this fallback means /bin/init could
    // not be prepared as a runnable user task.
    println!("[kernel-qemu] no runnable /bin/init; shutting down");

    sbi::shutdown()
}

// AGENT: Install a leaked Kernel as the QEMU scheduler/timer backend so real
// timer interrupts can drive migrated Kernel::schedule_tick() state.
fn init_qemu_kernel_backend(frame_pool: kernel::FramePool) -> (&'static kernel::Kernel, bool) {
    let root_block = init_root_block_device(frame_pool.clone());
    let kernel = Box::leak(Box::new(kernel::Kernel::new_with_block_device(
        frame_pool, root_block,
    )));
    let init_installed = match install_embedded_user_images(kernel, ROOT_INIT_ELF, EXEC_SMOKE_ELF) {
        Ok(true) => {
            println!("[kernel-qemu] installed embedded /bin/init");
            println!("[kernel-qemu] installed embedded /bin/exec-smoke");
            true
        }
        Ok(false) => {
            println!("[kernel-qemu] no embedded /bin/init");
            false
        }
        Err(err) => {
            println!("[kernel-qemu] failed to install /bin/init: {}", err);
            false
        }
    };
    kernel.proc_init();
    kernel::install_kernel(kernel);
    let current = kernel.cur_task(0).map(|task| task.id()).unwrap_or(0);
    println!(
        "[kernel-qemu] kernel timer backend installed current_task={}",
        current
    );
    (kernel, init_installed)
}

// AGENT: production boot requires a real VirtIO block device; the RAM backend
// is available only when the caller explicitly opts into its fallback feature.
fn init_root_block_device(frame_pool: kernel::FramePool) -> Arc<dyn kernel::BlockDevice> {
    #[cfg(feature = "ram-block-device")]
    {
        let _ = frame_pool;
        println!("[kernel-qemu] root block backend=ram-block-device feature fallback");
        Arc::new(kernel::RamBlockDevice::empty())
    }
    #[cfg(not(feature = "ram-block-device"))]
    {
        drivers::virtio_blk::probe_root_block(frame_pool)
            .expect("production boot requires a probed virtio-blk device")
    }
}

// AGENT: read a host-seeded sector, persist a second sector with an explicit
// device flush, and report whether that output survived a previous QEMU boot.
#[cfg(feature = "qemu-virtio-blk-smoke")]
fn run_virtio_blk_raw_smoke(frame_pool: kernel::FramePool) {
    const INPUT_BLOCK: usize = 8;
    const OUTPUT_BLOCK: usize = 9;
    const INPUT_MAGIC: &[u8] = b"CHAOS-VIRTIO-INPUT-v1";
    const OUTPUT_MAGIC: &[u8] = b"CHAOS-VIRTIO-PERSIST-v1";

    let device = drivers::virtio_blk::probe_root_block(frame_pool)
        .expect("virtio-blk raw smoke requires a block device");
    assert!(
        device.block_count() > OUTPUT_BLOCK,
        "virtio-blk smoke image is too small"
    );

    let input = device
        .read_block(INPUT_BLOCK)
        .expect("virtio-blk smoke input read failed");
    assert_eq!(
        &input[..INPUT_MAGIC.len()],
        INPUT_MAGIC,
        "virtio-blk smoke input magic mismatch"
    );
    println!("[virtio-blk-smoke] input magic ok block={}", INPUT_BLOCK);

    let old_output = device
        .read_block(OUTPUT_BLOCK)
        .expect("virtio-blk smoke persisted-sector read failed");
    if &old_output[..OUTPUT_MAGIC.len()] == OUTPUT_MAGIC {
        println!(
            "[virtio-blk-smoke] persisted magic ok block={}",
            OUTPUT_BLOCK
        );
    }

    let mut output = alloc::vec![0u8; kernel::BLOCK_CACHE_BLOCK_SIZE];
    output[..OUTPUT_MAGIC.len()].copy_from_slice(OUTPUT_MAGIC);
    device
        .write_block(OUTPUT_BLOCK, &output)
        .expect("virtio-blk smoke output write failed");
    device
        .flush()
        .expect("virtio-blk smoke device flush failed");
    let reread = device
        .read_block(OUTPUT_BLOCK)
        .expect("virtio-blk smoke output reread failed");
    assert_eq!(
        &reread[..OUTPUT_MAGIC.len()],
        OUTPUT_MAGIC,
        "virtio-blk smoke output magic mismatch after flush"
    );
    println!("[virtio-blk-smoke] write flushed block={}", OUTPUT_BLOCK);
    sbi::shutdown()
}

// AGENT: format only a blank VirtIO device on the first boot, then require the
// second boot to mount its superblock, recover a nested file, and allocate new
// blocks without overwriting the recovered file.
#[cfg(feature = "qemu-chaosfs-smoke")]
fn run_chaosfs_persistence_smoke(frame_pool: kernel::FramePool) {
    const SOURCE: &str = "virtio0";
    const TARGET: &str = "/mnt";
    const OLD_FILE: &str = "/mnt/a/file";
    const NEW_FILE: &str = "/mnt/a/new";
    const OLD_MAGIC: &[u8] = b"CHAOSFS-PERSIST-v1";

    let device = drivers::virtio_blk::probe_root_block(frame_pool)
        .expect("ChaosFs persistence smoke requires a block device");
    let recovered = match kernel::ChaosFs::mount(kernel::ROOT_FS_ID, device.clone(), 1) {
        Ok(fs) => Some(fs),
        Err("enodev") => None,
        Err(error) => panic!("ChaosFs smoke mount failed: {}", error),
    };
    let fs = match recovered {
        Some(fs) => fs,
        None => {
            assert!(
                kernel::ChaosFs::superblock_is_blank(device.as_ref())
                    .expect("ChaosFs smoke should read its blank superblock"),
                "ChaosFs smoke refuses to format a nonblank unknown device"
            );
            let fs = kernel::ChaosFs::format(kernel::ROOT_FS_ID, device, 1)
                .expect("blank ChaosFs smoke device should format");
            println!("[chaosfs-smoke] formatted source={}", SOURCE);
            fs
        }
    };

    let namespace_root = kernel::FsInstance::new(2, kernel::FileStorage::standalone());
    let vfs = kernel::Vfs::new(namespace_root);
    vfs.install_directory(TARGET)
        .expect("ChaosFs smoke mountpoint should install");
    vfs.register_source(SOURCE, fs.clone())
        .expect("ChaosFs smoke source should register");
    vfs.mount_source(
        SOURCE,
        TARGET,
        kernel::FsKind::ChaosFs,
        kernel::MountFlags::empty(),
    )
    .expect("ChaosFs smoke source should mount");

    if vfs.resolve(OLD_FILE).is_err() {
        vfs.create_directory("/mnt/a")
            .expect("ChaosFs smoke directory should create");
        let mut old_data = alloc::vec![0x5a; kernel::BLOCK_CACHE_BLOCK_SIZE * 3 + 37];
        old_data[..OLD_MAGIC.len()].copy_from_slice(OLD_MAGIC);
        vfs.install_regular(OLD_FILE, &old_data, false)
            .expect("ChaosFs smoke file should create");
        fs.flush()
            .expect("ChaosFs smoke first boot should flush filesystem metadata");
        println!(
            "[chaosfs-smoke] persisted file written bytes={}",
            old_data.len()
        );
        sbi::shutdown()
    }

    let mut expected = alloc::vec![0x5a; kernel::BLOCK_CACHE_BLOCK_SIZE * 3 + 37];
    expected[..OLD_MAGIC.len()].copy_from_slice(OLD_MAGIC);
    let recovered_file = vfs
        .resolve(OLD_FILE)
        .expect("ChaosFs smoke persisted file should resolve");
    let mut actual = alloc::vec![0u8; expected.len()];
    assert_eq!(
        recovered_file.path_ref.read_at(0, &mut actual),
        Ok(expected.len()),
        "ChaosFs smoke persisted file length mismatch"
    );
    assert_eq!(actual, expected, "ChaosFs smoke persisted bytes mismatch");
    println!("[chaosfs-smoke] recovered file bytes={}", actual.len());

    let new_data = alloc::vec![0xa5; kernel::BLOCK_CACHE_BLOCK_SIZE * 2 + 19];
    vfs.install_regular(NEW_FILE, &new_data, false)
        .expect("ChaosFs smoke new file should allocate after recovery");
    fs.flush()
        .expect("ChaosFs smoke second boot should flush new allocation");
    let mut old_after_new = alloc::vec![0u8; expected.len()];
    assert_eq!(
        vfs.resolve(OLD_FILE)
            .expect("ChaosFs smoke old file should remain reachable")
            .path_ref
            .read_at(0, &mut old_after_new),
        Ok(expected.len())
    );
    assert_eq!(old_after_new, expected);
    println!("[chaosfs-smoke] allocator preserved recovered file");
    sbi::shutdown()
}

// AGENT: provide a disposable installed Kernel for boot selftests that require
// live usercopy or checkpoint state without contaminating production scheduling.
#[cfg(any(feature = "qemu-fs-selftest", feature = "qemu-checkpoint-selftest"))]
fn init_qemu_selftest_backend(frame_pool: kernel::FramePool) -> &'static kernel::Kernel {
    let kernel = Box::leak(Box::new(kernel::Kernel::new(frame_pool)));
    kernel.proc_init();
    kernel::install_kernel(kernel);
    kernel
}

// AGENT: perform the final transactional init exec after optional boot tests so
// their address-space and trap-frame fixtures cannot leak into normal scheduling.
fn prepare_root_init_task(kernel: &kernel::Kernel, init_installed: bool) -> bool {
    if !init_installed {
        return false;
    }
    let prepared = kernel
        .cur_task(0)
        .ok_or("esrch")
        .and_then(|init| crate::kernel::proc::task::fd::install_initial_stdio(&init))
        .and_then(|()| {
            kernel.do_exec(
                kernel::INIT_PID,
                "/bin/init",
                alloc::vec![Vec::from(&b"/bin/init"[..])],
                Vec::new(),
            )
        });
    match prepared {
        Ok(()) => true,
        Err(err) => {
            println!("[kernel-qemu] failed to prepare init task: {}", err);
            false
        }
    }
}

// AGENT: install both linked user payloads through the same path-backed file
// store, then publish one complete namespace before either image can execute.
fn install_embedded_user_images(
    kernel: &kernel::Kernel,
    init_elf: &[u8],
    exec_smoke_elf: &[u8],
) -> Result<bool, &'static str> {
    if init_elf.is_empty() || exec_smoke_elf.is_empty() {
        return Ok(false);
    }
    // AGENT: establish the embedded root image's directory skeleton before the
    // strict path store installs `/bin/init`; init uses `/tmp` for its openat probe.
    kernel.install_directory("/bin")?;
    kernel.install_directory("/tmp")?;
    kernel.install_exec_file("/bin/init", Vec::from(init_elf))?;
    kernel.install_exec_file("/bin/exec-smoke", Vec::from(exec_smoke_elf))?;
    // AGENT: publish the complete boot namespace through the ChaosFs inode table
    // and bitmap before init begins, so a later boot can mount rather than rebuild it.
    kernel.vfs.root_fs().flush()?;
    Ok(true)
}

// AGENT: switch from bare addressing to an Sv39 root that keeps the current
// low-linked kernel alive while adding the high-half physical direct map.
fn install_kernel_page_table(frame_pool: &kernel::FramePool) {
    let page_table = kernel::build_kernel_page_table(
        frame_pool,
        QEMU_VIRT_RAM_START,
        QEMU_VIRT_RAM_END,
        trap::trampoline_paddr(),
        &[(
            drivers::QEMU_VIRTIO_MMIO_START,
            drivers::QEMU_VIRTIO_MMIO_SIZE,
        )],
    )
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

// AGENT: Compile the one-tick timer-wheel probe only into explicit boot-smoke
// images; normal kernels do not need this diagnostic target.
#[cfg(feature = "qemu-boot-smoke")]
fn arm_timer_wheel_probe() -> kernel::WaitToken {
    let kernel = kernel::global_kernel().expect("timer probe needs the global Kernel");
    let task_id = kernel
        .cur_task(0)
        .expect("timer probe needs the CPU0 current task")
        .id();
    let token = kernel::WaitToken::for_task(task_id);
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

// AGENT: Compile the real timer-interrupt observation loop only into explicit
// boot-smoke images.
#[cfg(feature = "qemu-boot-smoke")]
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

// AGENT: Compile the migrated timer-wheel observation loop only into explicit
// boot-smoke images.
#[cfg(feature = "qemu-boot-smoke")]
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
