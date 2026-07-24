use super::*;

pub fn run_all(kernel: &Kernel) {
    periodic_timer_preserves_phase_after_delayed_advance();
    checkpoint_stdio_round_trip_preserves_typed_terminal_objects(kernel);
    checkpoint_round_trip_restores_memory_and_trap_frame(kernel);
    checkpoint_round_trip_restores_task_timer(kernel);
}

// AGENT: a delayed wheel pass must keep a periodic timer on its original phase
// and schedule the first future interval instead of drifting from the late tick.
#[cfg_attr(test, test)]
fn periodic_timer_preserves_phase_after_delayed_advance() {
    let start = CLK.load(Ordering::Relaxed);
    let first_deadline = start.checked_add(1).expect("test clock should have room");
    let delayed_now = start.checked_add(7).expect("test clock should have room");
    let expected_next = start.checked_add(10).expect("test clock should have room");
    let mut timers = TimerWheel::new();
    let timer_id = timers.register_timer(first_deadline, 3, TimerTarget::Noop);

    CLK.store(delayed_now, Ordering::Relaxed);
    let fired = timers.advance();

    assert_eq!(fired.len(), 1);
    assert_eq!(fired[0].id, timer_id);
    let rescheduled = timers.slots[expected_next % TIMER_WHEEL_SIZE]
        .iter()
        .find(|entry| entry.id == timer_id)
        .expect("periodic timer should be rescheduled");
    assert_eq!(rescheduled.deadline, expected_next);
    CLK.store(start, Ordering::Relaxed);
}

// AGENT: prove first-version checkpoint keeps stdio roles while reconstructing
// typed terminal objects, and rejects a redirected regular file it cannot save.
#[cfg_attr(test, test)]
fn checkpoint_stdio_round_trip_preserves_typed_terminal_objects(kernel: &Kernel) {
    let source = kernel
        .tasks
        .spawn()
        .expect("checkpoint stdio source should spawn");
    crate::kernel::proc::task::fd::install_initial_stdio(&source)
        .expect("checkpoint stdio should install");

    let saved = source
        .snapshot_checkpoint_fds()
        .expect("typed terminal stdio should snapshot");
    assert_eq!(saved.len(), 3);
    assert_eq!(saved[0].kind, SavedFdKind::Stdin);
    assert_eq!(saved[1].kind, SavedFdKind::Stdout);
    assert_eq!(saved[2].kind, SavedFdKind::Stderr);
    assert!(saved.iter().all(|entry| entry.offset == 0));

    let restored = kernel
        .tasks
        .spawn()
        .expect("checkpoint stdio target should spawn");
    restored
        .restore_checkpoint_fds(&saved)
        .expect("typed terminal stdio should restore");
    for fd in 0..=2 {
        let entry = restored
            .get_fd_entry(fd)
            .expect("restored stdio fd should exist");
        assert!(entry.is_tty());
        assert!(!entry.is_regular_file());
        assert_eq!(entry.offset(), 0);
        assert_eq!(entry.seek(FSeek::Start(0)), Err("espipe"));
    }

    let mut stdin_byte = [0u8; 1];
    assert_eq!(
        restored
            .get_fd_entry(0)
            .expect("restored stdin should exist")
            .read(&mut stdin_byte),
        Ok(0)
    );
    assert_eq!(
        restored
            .get_fd_entry(1)
            .expect("restored stdout should exist")
            .read(&mut stdin_byte),
        Err("ebadf")
    );

    source.close_fd(1).expect("stdout redirect should close");
    let redirected = source
        .add_file_with_status(
            FLike::File(FHandle::new(FInstance::new("/tmp/redirected"))),
            FdOpt {
                rd: false,
                wr: true,
                ap: false,
                nb: false,
            },
            false,
        )
        .expect("redirected stdout should reuse fd 1");
    assert_eq!(redirected, 1);
    assert_eq!(source.snapshot_checkpoint_fds(), Err("enotsup"));
}

// AGENT: prepare the minimal user mappings required by checkpoint validation
// without failing when another checkpoint selftest already installed them.
fn ensure_checkpoint_regions(kernel: &Kernel, current: &Task, data_addr: usize, pattern: &[u8]) {
    let stack_base = USR_STK_OFF;
    let mut addr_space = current.process.addr_space.lock().unwrap();
    if addr_space.mapped_region(data_addr).is_none() {
        addr_space
            .map_region(
                VmRegion::new(data_addr, PAGE_SZ, VM_READ | VM_WRITE),
                &kernel.pool,
            )
            .expect("checkpoint data page should map");
    }
    if addr_space.mapped_region(stack_base).is_none() {
        addr_space
            .map_region(
                VmRegion::new(stack_base, PAGE_SZ, VM_READ | VM_WRITE | VM_GROWSDOWN),
                &kernel.pool,
            )
            .expect("checkpoint stack page should map");
    }
    addr_space
        .write_user_bytes(data_addr, pattern, &kernel.pool)
        .expect("checkpoint data page should be writable");
}

// AGENT: prove the first checkpoint vertical slice can copy current-task VMA
// metadata, resident page bytes, and a complete saved trap frame into a new pid.
#[cfg_attr(test, test)]
fn checkpoint_round_trip_restores_memory_and_trap_frame(kernel: &Kernel) {
    let current = kernel
        .cur_task(0)
        .expect("proc_init should install current");
    let data_addr = 0x5000_0000usize;
    let stack_base = USR_STK_OFF;
    let stack_top = stack_base + PAGE_SZ;
    let pattern = [0x31u8, 0x41, 0x59, 0x26, 0x53, 0x58, 0x97, 0x93];

    ensure_checkpoint_regions(kernel, &current, data_addr, &pattern);

    let mut regs = [0u64; 32];
    regs[2] = stack_top as u64;
    regs[10] = 0x2a;
    let frame = SavedTrapFrame {
        regs,
        sstatus: 0x20,
        sepc: 0x1000_0004,
    };

    let image = kernel
        .checkpoint_current_image(0, frame.clone())
        .expect("current task should checkpoint");
    let bytes = image
        .encode_first_version()
        .expect("checkpoint image should encode");
    let decoded =
        CheckpointImage::decode_first_version(&bytes).expect("checkpoint image should decode");
    let restored_id = kernel
        .restore_process_from_image(decoded)
        .expect("checkpoint image should restore");
    assert_ne!(restored_id, current.id());

    let restored = kernel
        .tasks
        .find_task(restored_id)
        .expect("restored task should be registered");
    let mut restored_pattern = [0u8; 8];
    restored
        .process
        .addr_space
        .lock()
        .unwrap()
        .read_user_bytes(data_addr, &mut restored_pattern)
        .expect("restored page should be readable");
    assert_eq!(restored_pattern, pattern);
    assert_eq!(
        restored
            .snapshot_user_trap_frame()
            .expect("restored task should own a complete trap frame")
            .to_saved_checkpoint_frame(),
        frame
    );
}

// AGENT: prove checkpoint/restore freezes a task-bound timer's remaining delay
// and rebinds its target to the new task id instead of the original process.
#[cfg_attr(test, test)]
fn checkpoint_round_trip_restores_task_timer(kernel: &Kernel) {
    let current = kernel
        .cur_task(0)
        .expect("proc_init should install current");
    let data_addr = 0x5000_1000usize;
    let pattern = [0x5au8, 0xa5, 0x33, 0xcc];
    ensure_checkpoint_regions(kernel, &current, data_addr, &pattern);

    let deadline = CLK.load(Ordering::Relaxed).saturating_add(2);
    let original_timer_id = {
        let mut timers = global_timer_wheel().lock();
        timers.register_timer(
            deadline,
            0,
            TimerTarget::SignalTask {
                task_id: current.id(),
                signo: SIGUSR1 as i32,
                sender_tid: -1,
            },
        )
    };

    let mut regs = [0u64; 32];
    regs[2] = (USR_STK_OFF + PAGE_SZ) as u64;
    let frame = SavedTrapFrame {
        regs,
        sstatus: 0x20,
        sepc: 0x1000_0104,
    };

    let image = kernel
        .checkpoint_current_image(0, frame)
        .expect("current task should checkpoint timer");
    assert_eq!(image.timers.len(), 1);
    assert_eq!(
        image.timers[0].target_kind,
        SavedTimerTargetKind::SignalTask
    );
    assert_eq!(image.timers[0].remaining_ticks, 2);

    assert!(global_timer_wheel().lock().cancel(original_timer_id));
    kernel.schedule_tick(0);
    let restored_id = kernel
        .restore_process_from_image(image)
        .expect("checkpoint timer should restore");
    let restored = kernel
        .tasks
        .find_task(restored_id)
        .expect("restored timer target should exist");

    let restored_timers = global_timer_wheel()
        .lock()
        .snapshot_checkpoint_timers(restored_id)
        .expect("restored timer should be serializable");
    assert_eq!(restored_timers.len(), 1);
    assert_eq!(
        restored_timers[0].target_kind,
        SavedTimerTargetKind::SignalTask
    );
    assert_eq!(restored_timers[0].remaining_ticks, 2);

    kernel.schedule_tick(0);
    assert!(!restored.has_interrupting_signal());
    kernel.schedule_tick(0);
    assert!(restored.has_interrupting_signal());
}
