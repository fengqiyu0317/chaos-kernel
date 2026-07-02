use super::*;

pub fn run_all(kernel: &Kernel) {
    checkpoint_round_trip_restores_memory_and_trap_frame(kernel);
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

    {
        let mut addr_space = current.process.addr_space.lock().unwrap();
        addr_space
            .map_region(
                VmRegion::new(data_addr, PAGE_SZ, VM_READ | VM_WRITE),
                &kernel.pool,
            )
            .expect("checkpoint data page should map");
        addr_space
            .map_region(
                VmRegion::new(stack_base, PAGE_SZ, VM_READ | VM_WRITE | VM_GROWSDOWN),
                &kernel.pool,
            )
            .expect("checkpoint stack page should map");
        addr_space
            .write_user_bytes(data_addr, &pattern, &kernel.pool)
            .expect("checkpoint data page should be writable");
    }

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
        .find(restored_id)
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
    assert_eq!(restored.take_restored_trap_frame(), Some(frame));
}
