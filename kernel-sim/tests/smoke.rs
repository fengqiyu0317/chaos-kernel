// AGENT
use kernel_sim::{
    EpData, EpEvent, FLike, Kernel, PageTableEntry, PgFrame, SchedulePolicy, TaskRunState,
    TaskTable, VmRegion, N_FRAMES, N_PROC, O_CLOEXEC, PAGE_SZ, SIGUSR1, SYS_EPOLL_CREATE,
    SYS_EPOLL_CTL, SYS_EXIT, SYS_FORK, SYS_FUTEX, SYS_GETPID, SYS_KILL, SYS_OPEN, SYS_SIGACTION,
    SYS_SIGRETURN, USR_STK_OFF, USR_STK_SZ, VM_EXEC, VM_READ, VM_SHARED, VM_WRITE,
};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Barrier};
use std::thread;
use std::time::Duration;

#[repr(C)]
#[derive(Clone, Copy)]
struct UserSigAction {
    sa_handler: usize,
    sa_sigaction: usize,
    sa_mask: u64,
    sa_flags: i32,
}

#[test]
fn boot_kernel_in_standalone_runtime() {
    let kernel = Kernel::new(N_FRAMES);
    kernel.proc_init();

    assert_eq!(kernel.cur_task(0).expect("init should be current").id(), 1);

    let pid = kernel
        .dispatch_syscall(SYS_GETPID, 0, 0, 0, 0, 0, 0)
        .expect("getpid should succeed in standalone runtime");

    assert_eq!(pid, 1);
}

#[test]
// AGENT
fn syscall_fork_creates_child_task_and_enqueues_it() {
    let kernel = Kernel::new(N_FRAMES);
    kernel.proc_init();

    let child = kernel
        .dispatch_syscall(SYS_FORK, 0, 0, 0, 0, 0, 0)
        .expect("fork syscall should create child task");
    let child_task = kernel
        .tasks
        .find(child)
        .expect("fork syscall should register child task");

    assert_eq!(kernel.tasks.count(), 2);
    assert_eq!(
        child_task
            .parent
            .lock()
            .unwrap()
            .as_ref()
            .expect("child should remember parent")
            .id(),
        1
    );
    assert_eq!(child_task.sched_state(), TaskRunState::Runnable);
    assert_eq!(kernel.run_queue.len(), 1);
}

#[test]
// AGENT
fn fork_copies_context_address_space_cwd_and_kernel_stack() {
    let kernel = Kernel::new(N_FRAMES);
    kernel.proc_init();
    let parent = kernel.cur_task(0).expect("init should be current");
    let parent_token = parent.vm_token.load(Ordering::Relaxed);
    {
        parent.info.lock().unwrap().fds = vec![String::from("fd:tracked")];
        parent.sched.lock().unwrap().policy = SchedulePolicy::with_prio(-4);
        parent.sig_state.lock().unwrap().sig_raise(SIGUSR1);
        let mut thd = parent.thd_ctx.lock().unwrap();
        let ctx = thd.as_mut().expect("parent context should exist");
        ctx.uctx.set_ip(0x1234);
        ctx.uctx.r[0] = 99;
        ctx.uctx.r[3] = 0x7777;
        ctx.clear_tid = 42;
        ctx.smask = 0x55;
    }
    {
        *parent.cwd.lock().unwrap() = String::from("caf\u{e9}/fork");
        let mut addr_space = parent.addr_space.lock().unwrap();
        addr_space.vm_map.brk = 0x0060_0000;
        addr_space
            .vm_map
            .insert(VmRegion::new(0x5000_0000, PAGE_SZ, VM_READ | VM_WRITE))
            .expect("test mapping should not overlap");
        addr_space.page_table.lock().unwrap().insert(
            0x5000_0000,
            PageTableEntry::new(17, PgFrame::with_rc(1), VM_READ | VM_WRITE),
        );
    }

    let child = kernel.do_fork(1).expect("fork should create child");
    let child_task = kernel
        .tasks
        .find(child)
        .expect("child should be registered");

    assert_ne!(child_task.vm_token.load(Ordering::Relaxed), parent_token);
    assert!(child_task.kstk.lock().unwrap().is_some());
    assert_eq!(&*child_task.cwd.lock().unwrap(), "caf\u{e9}/fork");
    assert_eq!(
        child_task.info.lock().unwrap().fds,
        vec![String::from("fd:tracked")]
    );
    {
        let child_policy = child_task.sched_policy();
        assert_eq!(child_policy.prio, -4);
        assert_eq!(child_policy.nice, -4);
    }
    assert_eq!(child_task.sig_state.lock().unwrap().pending, 0);
    {
        let child_addr_space = child_task.addr_space.lock().unwrap();
        assert_eq!(child_addr_space.vm_map.brk, 0x0060_0000);
        assert!(child_addr_space.vm_map.find(0x5000_0000).is_some());
        let child_pte = child_addr_space
            .page_table
            .lock()
            .unwrap()
            .get(&0x5000_0000)
            .expect("child should have a COW PTE")
            .clone();
        assert!(child_pte.cow);
        assert!(!child_pte.writable);
        assert_eq!(child_pte.frame.count(), 2);
    }
    {
        let parent_addr_space = parent.addr_space.lock().unwrap();
        let parent_pte = parent_addr_space
            .page_table
            .lock()
            .unwrap()
            .get(&0x5000_0000)
            .expect("parent should have a COW PTE")
            .clone();
        assert!(parent_pte.cow);
        assert!(!parent_pte.writable);
        assert_eq!(parent_pte.frame.count(), 2);
    }
    {
        parent.addr_space.lock().unwrap().vm_map.brk = 0x0070_0000;
        let child_addr_space = child_task.addr_space.lock().unwrap();
        assert_eq!(child_addr_space.vm_map.brk, 0x0060_0000);
    }
    {
        let thd = child_task.thd_ctx.lock().unwrap();
        let ctx = thd.as_ref().expect("child context should exist");
        assert_eq!(ctx.uctx.ip, 0x1234);
        assert_eq!(ctx.uctx.r[0], 0);
        assert_eq!(ctx.uctx.r[3], 0x7777);
        assert_eq!(ctx.clear_tid, 42);
        assert_eq!(ctx.smask, 0x55);
    }
    {
        let thd = parent.thd_ctx.lock().unwrap();
        let ctx = thd.as_ref().expect("parent context should still exist");
        assert_eq!(ctx.uctx.r[0], 99);
    }
}

#[test]
// AGENT
fn cow_write_fault_copies_child_page_and_keeps_parent_shared() {
    let kernel = Kernel::new(N_FRAMES);
    kernel.proc_init();
    let parent = kernel.cur_task(0).expect("init should be current");
    let page = 0x5100_0000;
    {
        let mut addr_space = parent.addr_space.lock().unwrap();
        addr_space
            .vm_map
            .insert(VmRegion::new(page, PAGE_SZ, VM_READ | VM_WRITE))
            .expect("test mapping should not overlap");
        addr_space.page_table.lock().unwrap().insert(
            page,
            PageTableEntry::new(33, PgFrame::with_rc(1), VM_READ | VM_WRITE),
        );
    }

    let child = kernel.do_fork(1).expect("fork should create child");
    let child_task = kernel
        .tasks
        .find(child)
        .expect("child should be registered");
    kernel.set_cur(0, Some(child_task.clone()));

    assert!(kernel.handle_pgfault_ext(page, 0x2));

    let child_addr_space = child_task.addr_space.lock().unwrap();
    let child_pte = child_addr_space
        .page_table
        .lock()
        .unwrap()
        .get(&page)
        .expect("child should retain a mapped PTE")
        .clone();
    assert!(child_pte.writable);
    assert!(!child_pte.cow);
    assert_ne!(child_pte.frame_id, 33);
    assert_eq!(child_pte.frame.count(), 1);
    assert_eq!(child_addr_space.cow_sharers(), 0);
    drop(child_addr_space);

    let parent_addr_space = parent.addr_space.lock().unwrap();
    let parent_pte = parent_addr_space
        .page_table
        .lock()
        .unwrap()
        .get(&page)
        .expect("parent should keep the original PTE")
        .clone();
    assert_eq!(parent_pte.frame_id, 33);
    assert!(parent_pte.cow);
    assert!(!parent_pte.writable);
    assert_eq!(parent_pte.frame.count(), 1);
}

#[test]
// AGENT
fn unmap_range_returns_unmapped_page_count_and_splits_region() {
    let kernel = Kernel::new(N_FRAMES);
    kernel.proc_init();
    let task = kernel.cur_task(0).expect("init should be current");
    let base = 0x5300_0000;
    let frames = [60, 61, 62];
    {
        let mut addr_space = task.addr_space.lock().unwrap();
        addr_space
            .vm_map
            .insert(VmRegion::new(base, PAGE_SZ * 3, VM_READ | VM_WRITE))
            .expect("test mapping should not overlap");
        let mut pt = addr_space.page_table.lock().unwrap();
        for (idx, frame_id) in frames.into_iter().enumerate() {
            pt.insert(
                base + idx * PAGE_SZ,
                PageTableEntry::new(frame_id, PgFrame::with_rc(1), VM_READ | VM_WRITE),
            );
        }
    }

    let unmapped = task
        .addr_space
        .lock()
        .unwrap()
        .unmap_range(base + PAGE_SZ, PAGE_SZ);

    let addr_space = task.addr_space.lock().unwrap();
    assert_eq!(unmapped, 1);
    assert_eq!(addr_space.vm_map.regions.len(), 2);
    assert!(addr_space.vm_map.find(base).is_some());
    assert!(addr_space.vm_map.find(base + PAGE_SZ).is_none());
    assert!(addr_space.vm_map.find(base + PAGE_SZ * 2).is_some());
    let pt = addr_space.page_table.lock().unwrap();
    assert!(pt.contains_key(&base));
    assert!(!pt.contains_key(&(base + PAGE_SZ)));
    assert!(pt.contains_key(&(base + PAGE_SZ * 2)));
}

#[test]
// AGENT
fn fork_keeps_shared_writable_mapping_without_cow() {
    let kernel = Kernel::new(N_FRAMES);
    kernel.proc_init();
    let parent = kernel.cur_task(0).expect("init should be current");
    let page = 0x5200_0000;
    {
        let mut addr_space = parent.addr_space.lock().unwrap();
        addr_space
            .vm_map
            .insert(VmRegion::new(page, PAGE_SZ, VM_READ | VM_WRITE | VM_SHARED))
            .expect("test mapping should not overlap");
        addr_space.page_table.lock().unwrap().insert(
            page,
            PageTableEntry::new(44, PgFrame::with_rc(1), VM_READ | VM_WRITE | VM_SHARED),
        );
    }

    let child = kernel.do_fork(1).expect("fork should create child");
    let child_task = kernel
        .tasks
        .find(child)
        .expect("child should be registered");

    let child_addr_space = child_task.addr_space.lock().unwrap();
    let child_pte = child_addr_space
        .page_table
        .lock()
        .unwrap()
        .get(&page)
        .expect("child should inherit shared PTE")
        .clone();
    assert!(child_pte.writable);
    assert!(!child_pte.cow);
    assert_eq!(child_pte.frame.count(), 2);

    let parent_addr_space = parent.addr_space.lock().unwrap();
    let parent_pte = parent_addr_space
        .page_table
        .lock()
        .unwrap()
        .get(&page)
        .expect("parent should keep shared PTE")
        .clone();
    assert!(parent_pte.writable);
    assert!(!parent_pte.cow);
    assert_eq!(parent_pte.frame.count(), 2);
}

#[test]
// AGENT
fn fork_preserves_cloexec_and_epoll_state() {
    let kernel = Kernel::new(N_FRAMES);
    kernel.proc_init();
    let fd = kernel
        .dispatch_syscall(SYS_OPEN, 0x1000, O_CLOEXEC, 0, 0, 0, 0)
        .expect("open should create cloexec file");
    let epfd = kernel
        .dispatch_syscall(SYS_EPOLL_CREATE, 1, 0, 0, 0, 0, 0)
        .expect("epoll_create should create epoll fd");
    let ev = EpEvent {
        events: EpEvent::IN,
        data: EpData { ptr: 0x55 },
    };
    kernel
        .dispatch_syscall(
            SYS_EPOLL_CTL,
            epfd,
            1,
            fd,
            &ev as *const EpEvent as usize,
            0,
            0,
        )
        .expect("epoll_ctl should register fd");

    let child = kernel.do_fork(1).expect("fork should create child");
    let child_task = kernel
        .tasks
        .find(child)
        .expect("child should be registered");

    match child_task.get_file(fd).expect("child should inherit fd") {
        FLike::File(f) => assert!(f.cloexec),
        _ => panic!("expected inherited regular file"),
    }
    let modified_ev = EpEvent {
        events: EpEvent::OUT,
        data: EpData { ptr: 0xaa },
    };
    kernel
        .dispatch_syscall(
            SYS_EPOLL_CTL,
            epfd,
            3,
            fd,
            &modified_ev as *const EpEvent as usize,
            0,
            0,
        )
        .expect("parent epoll_ctl should update shared epoll instance");
    let ep = child_task.ep_inst.lock().unwrap();
    let inst = ep.get(&epfd).expect("child should inherit epoll instance");
    assert_eq!(
        inst.events
            .lock()
            .unwrap()
            .get(&fd)
            .expect("child epoll instance should share watched fd")
            .data
            .ptr,
        0xaa
    );
}

#[test]
// AGENT
fn do_exec_commits_new_address_space_context_and_cloexec() {
    let kernel = Kernel::new(N_FRAMES);
    kernel.proc_init();
    let task = kernel.cur_task(0).expect("init should be current");
    let keep_fd = kernel
        .dispatch_syscall(SYS_OPEN, 0x2000, 0, 0, 0, 0, 0)
        .expect("open should create a non-cloexec fd");
    let close_fd = kernel
        .dispatch_syscall(SYS_OPEN, 0x3000, O_CLOEXEC, 0, 0, 0, 0)
        .expect("open should create a cloexec fd");
    let old_token = task.vm_token.load(Ordering::Relaxed);
    {
        let mut addr_space = task.addr_space.lock().unwrap();
        addr_space
            .map_region(
                VmRegion::new(0x5300_0000, PAGE_SZ, VM_READ | VM_WRITE),
                &kernel.pool,
            )
            .expect("old mapping should be created");
    }
    {
        let mut thd = task.thd_ctx.lock().unwrap();
        let ctx = thd.as_mut().expect("thread context should exist");
        ctx.uctx.set_ip(0x1111);
        ctx.uctx.set_sp(0x2222);
        ctx.clear_tid = 77;
    }

    kernel
        .do_exec(
            1,
            "/bin/next",
            vec![String::from("next")],
            vec![String::from("A=B")],
        )
        .expect("exec should commit the prepared image");

    assert_eq!(&*task.exec_path.lock().unwrap(), "/bin/next");
    assert!(task.get_file(keep_fd).is_some());
    assert!(task.get_file(close_fd).is_none());
    assert_ne!(task.vm_token.load(Ordering::Relaxed), old_token);
    {
        let addr_space = task.addr_space.lock().unwrap();
        assert!(addr_space.vm_map.find(0x5300_0000).is_none());
        let text = addr_space
            .vm_map
            .find(0x0040_0000)
            .expect("exec should map the text segment");
        assert_ne!(text.flags & VM_EXEC, 0);
        assert!(addr_space.vm_map.find(USR_STK_OFF).is_some());
        assert!(addr_space
            .vm_map
            .find(USR_STK_OFF + USR_STK_SZ - 1)
            .is_some());
        assert_eq!(addr_space.vm_map.brk, 0x0040_1000);
        assert!(addr_space
            .page_table
            .lock()
            .unwrap()
            .contains_key(&0x0040_0000));
    }
    {
        let thd = task.thd_ctx.lock().unwrap();
        let ctx = thd.as_ref().expect("thread context should exist");
        let sp = *ctx
            .uctx
            .r
            .last()
            .expect("context should have a stack register") as usize;
        assert_eq!(ctx.uctx.ip, 0x0040_0000);
        assert!(sp >= USR_STK_OFF && sp <= USR_STK_OFF + USR_STK_SZ);
        assert_eq!(ctx.clear_tid, 0);
        assert!(ctx.sig_frames.is_empty());
    }
}

#[test]
// AGENT
fn do_exec_failure_preserves_old_image_and_cloexec_fds() {
    let kernel = Kernel::new(N_FRAMES);
    kernel.proc_init();
    let task = kernel.cur_task(0).expect("init should be current");
    let close_fd = kernel
        .dispatch_syscall(SYS_OPEN, 0x4000, O_CLOEXEC, 0, 0, 0, 0)
        .expect("open should create a cloexec fd");
    *task.exec_path.lock().unwrap() = String::from("/bin/old");
    {
        let mut addr_space = task.addr_space.lock().unwrap();
        addr_space
            .map_region(
                VmRegion::new(0x5400_0000, PAGE_SZ, VM_READ | VM_WRITE),
                &kernel.pool,
            )
            .expect("old mapping should be created");
    }
    {
        let mut thd = task.thd_ctx.lock().unwrap();
        let ctx = thd.as_mut().expect("thread context should exist");
        ctx.uctx.set_ip(0x1111);
        ctx.uctx.set_sp(0x2222);
        ctx.clear_tid = 123;
    }
    let old_token = task.vm_token.load(Ordering::Relaxed);
    let free_before = kernel.pool.free_count();

    let err = kernel
        .do_exec(1, "/bin/too-big", vec!["x".repeat(USR_STK_SZ)], Vec::new())
        .expect_err("oversized initial stack should fail before commit");

    assert_eq!(err, "e2big");
    assert_eq!(kernel.pool.free_count(), free_before);
    assert_eq!(&*task.exec_path.lock().unwrap(), "/bin/old");
    assert_eq!(task.vm_token.load(Ordering::Relaxed), old_token);
    match task
        .get_file(close_fd)
        .expect("cloexec fd should survive failed exec")
    {
        FLike::File(f) => assert!(f.cloexec),
        _ => panic!("expected regular file"),
    }
    {
        let addr_space = task.addr_space.lock().unwrap();
        assert!(addr_space.vm_map.find(0x5400_0000).is_some());
        assert!(addr_space.vm_map.find(0x0040_0000).is_none());
        assert_eq!(addr_space.rss_pages(), 1);
    }
    {
        let thd = task.thd_ctx.lock().unwrap();
        let ctx = thd.as_ref().expect("thread context should exist");
        assert_eq!(ctx.uctx.ip, 0x1111);
        assert_eq!(*ctx.uctx.r.last().unwrap(), 0x2222);
        assert_eq!(ctx.clear_tid, 123);
    }
}

#[test]
// AGENT
fn fork_returns_eagain_when_process_table_is_full() {
    let kernel = Kernel::new(N_FRAMES);
    kernel.proc_init();
    for _ in kernel.tasks.count()..N_PROC {
        kernel.tasks.spawn("filler");
    }

    let err = kernel
        .do_fork(1)
        .expect_err("fork should fail when process table is full");

    assert_eq!(err, "eagain");
    assert_eq!(kernel.tasks.count(), N_PROC);
}

#[test]
// AGENT
fn concurrent_fork_respects_process_table_limit() {
    let tasks = Arc::new(TaskTable::new());
    let root = tasks.spawn_root();
    for _ in tasks.count()..(N_PROC - 1) {
        tasks.spawn("filler");
    }

    let workers = 8;
    let barrier = Arc::new(Barrier::new(workers));
    let handles: Vec<_> = (0..workers)
        .map(|_| {
            let tasks = tasks.clone();
            let root = root.clone();
            let barrier = barrier.clone();
            thread::spawn(move || {
                barrier.wait();
                tasks.fork_task(&root).map(|task| task.id())
            })
        })
        .collect();

    let results: Vec<_> = handles
        .into_iter()
        .map(|handle| handle.join().expect("fork worker should not panic"))
        .collect();
    let successes = results.iter().filter(|result| result.is_ok()).count();

    assert_eq!(successes, 1);
    assert!(results
        .iter()
        .filter_map(|result| result.as_ref().err())
        .all(|err| *err == "eagain"));
    assert_eq!(tasks.count(), N_PROC);
    assert_eq!(root.n_children(), 1);
}

#[test]
// AGENT
fn default_signal_action_terminates_current_task() {
    let kernel = Kernel::new(N_FRAMES);
    kernel.proc_init();
    let child = kernel.do_fork(1).expect("fork should create child task");

    kernel
        .dispatch_syscall(SYS_KILL, 1, SIGUSR1 as usize, 0, 0, 0, 0)
        .expect("kill should enqueue and deliver the signal");

    let init = kernel
        .tasks
        .find(1)
        .expect("init task should still be reaped later");
    assert!(init.done());
    assert_eq!(*init.exit_code.lock().unwrap(), 128 + SIGUSR1 as usize);
    assert_eq!(
        kernel.cur_task(0).expect("child should be scheduled").id(),
        child
    );
}

#[test]
// AGENT
fn custom_signal_handler_updates_context_and_sigreturn_restores_it() {
    let kernel = Kernel::new(N_FRAMES);
    kernel.proc_init();
    let task = kernel.cur_task(0).expect("init should be current");
    {
        let mut thd = task.thd_ctx.lock().unwrap();
        let ctx = thd.as_mut().expect("thread context should exist");
        ctx.uctx.set_ip(0x1234);
        ctx.uctx.r[3] = 0x7777;
    }

    let act = UserSigAction {
        sa_handler: 0x5555,
        sa_sigaction: 0,
        sa_mask: 1u64 << SIGUSR1,
        sa_flags: 0,
    };
    let act_addr = &act as *const UserSigAction as usize;
    kernel
        .dispatch_syscall(SYS_SIGACTION, SIGUSR1 as usize, act_addr, 0, 0, 0, 0)
        .expect("sigaction should install handler");

    kernel
        .dispatch_syscall(SYS_KILL, 1, SIGUSR1 as usize, 0, 0, 0, 0)
        .expect("kill should enter signal handler");

    {
        let thd = task.thd_ctx.lock().unwrap();
        let ctx = thd.as_ref().expect("thread context should exist");
        assert_eq!(ctx.uctx.ip, 0x5555);
        assert_eq!(ctx.uctx.r[0], SIGUSR1 as u64);
        assert_eq!(ctx.uctx.r[1], u64::MAX);
        assert_eq!(ctx.uctx.r[2], 0x1234);
        assert_eq!(ctx.sig_frames.len(), 1);
        assert_ne!(*task.sig_mask.lock().unwrap() & (1u64 << SIGUSR1), 0);
    }

    kernel
        .dispatch_syscall(SYS_SIGRETURN, 0, 0, 0, 0, 0, 0)
        .expect("sigreturn should restore interrupted context");

    let thd = task.thd_ctx.lock().unwrap();
    let ctx = thd.as_ref().expect("thread context should exist");
    assert_eq!(ctx.uctx.ip, 0x1234);
    assert_eq!(ctx.uctx.r[3], 0x7777);
    assert_eq!(ctx.sig_frames.len(), 0);
    assert_eq!(*task.sig_mask.lock().unwrap(), 0);
}

#[test]
fn forked_task_enters_run_queue_and_receives_cpu_after_slice() {
    let kernel = Kernel::new(N_FRAMES);
    kernel.proc_init();
    let child = kernel.do_fork(1).expect("fork should create child task");

    assert_eq!(kernel.run_queue.len(), 1);
    assert_eq!(kernel.cur_task(0).expect("init should run first").id(), 1);

    for _ in 0..10 {
        kernel.schedule_tick(0);
    }

    let current = kernel
        .cur_task(0)
        .expect("scheduler should pick runnable child");
    assert_eq!(current.id(), child);
    assert_eq!(current.sched_state(), TaskRunState::Running);
}

#[test]
fn single_current_task_keeps_running_across_ticks() {
    let kernel = Kernel::new(N_FRAMES);
    kernel.proc_init();

    for _ in 0..25 {
        kernel.schedule_tick(0);
    }

    assert_eq!(
        kernel.cur_task(0).expect("init should remain current").id(),
        1
    );
    assert_eq!(kernel.run_queue.len(), 0);
}

#[test]
fn exiting_current_task_switches_to_next_runnable_task() {
    let kernel = Kernel::new(N_FRAMES);
    kernel.proc_init();
    let child = kernel.do_fork(1).expect("fork should create child task");

    kernel
        .dispatch_syscall(SYS_EXIT, 0, 0, 0, 0, 0, 0)
        .expect("exit should succeed");

    let current = kernel
        .cur_task(0)
        .expect("child should run after init exits");
    assert_eq!(current.id(), child);
    assert_eq!(current.sched_state(), TaskRunState::Running);
}

#[test]
fn futex_wait_returns_eagain_when_value_changed() {
    let kernel = Kernel::new(N_FRAMES);
    kernel.proc_init();
    let futex_word = AtomicU32::new(0);
    let uaddr = &futex_word as *const AtomicU32 as usize;

    let err = kernel
        .dispatch_syscall(SYS_FUTEX, uaddr, 0, 1, 0, 0, 0)
        .expect_err("wait should not sleep when the futex word differs");

    assert_eq!(err, "eagain");
}

#[test]
fn futex_wait_sleeps_until_wake() {
    let kernel = Arc::new(Kernel::new(N_FRAMES));
    kernel.proc_init();
    let futex_word = Arc::new(AtomicU32::new(1));
    let timeout = Arc::new([1usize, 0usize]);
    let uaddr = futex_word.as_ref() as *const AtomicU32 as usize;
    let timeout_addr = timeout.as_ptr() as usize;

    let waiter_kernel = kernel.clone();
    let waiter_word = futex_word.clone();
    let waiter = thread::spawn(move || {
        let uaddr = waiter_word.as_ref() as *const AtomicU32 as usize;
        waiter_kernel
            .dispatch_syscall(SYS_FUTEX, uaddr, 0, 1, timeout_addr, 0, 0)
            .expect("wait should be woken before the timeout")
    });

    thread::sleep(Duration::from_millis(25));
    futex_word.store(0, Ordering::SeqCst);
    let woken = kernel
        .dispatch_syscall(SYS_FUTEX, uaddr, 1, 1, 0, 0, 0)
        .expect("wake should succeed");

    assert_eq!(woken, 1);
    assert_eq!(waiter.join().expect("waiter thread should finish"), 0);
}

#[test]
fn futex_wake_zero_wakes_nobody() {
    let kernel = Kernel::new(N_FRAMES);
    kernel.proc_init();
    let futex_word = AtomicU32::new(1);
    let uaddr = &futex_word as *const AtomicU32 as usize;

    let woken = kernel
        .dispatch_syscall(SYS_FUTEX, uaddr, 1, 0, 0, 0, 0)
        .expect("wake with count zero should succeed");

    assert_eq!(woken, 0);
}

#[test]
fn futex_requeue_wakes_and_moves_waiters() {
    let kernel = Arc::new(Kernel::new(N_FRAMES));
    kernel.proc_init();
    let src = Arc::new(AtomicU32::new(1));
    let dst = Arc::new(AtomicU32::new(0));
    let timeout = Arc::new([1usize, 0usize]);
    let src_addr = src.as_ref() as *const AtomicU32 as usize;
    let dst_addr = dst.as_ref() as *const AtomicU32 as usize;
    let timeout_addr = timeout.as_ptr() as usize;

    let first_kernel = kernel.clone();
    let first_src = src.clone();
    let first = thread::spawn(move || {
        let src_addr = first_src.as_ref() as *const AtomicU32 as usize;
        first_kernel
            .dispatch_syscall(SYS_FUTEX, src_addr, 0, 1, timeout_addr, 0, 0)
            .expect("first waiter should be woken")
    });

    let second_kernel = kernel.clone();
    let second_src = src.clone();
    let second = thread::spawn(move || {
        let src_addr = second_src.as_ref() as *const AtomicU32 as usize;
        second_kernel
            .dispatch_syscall(SYS_FUTEX, src_addr, 0, 1, timeout_addr, 0, 0)
            .expect("second waiter should be requeued then woken")
    });

    thread::sleep(Duration::from_millis(25));
    let affected = kernel
        .dispatch_syscall(SYS_FUTEX, src_addr, 3, 1, 1, dst_addr, 0)
        .expect("requeue should succeed");
    // AGENT: FUTEX_REQUEUE returns the number of waiters directly woken, not
    // the number moved to the destination futex.
    assert_eq!(affected, 1);

    let woken = kernel
        .dispatch_syscall(SYS_FUTEX, dst_addr, 1, 1, 0, 0, 0)
        .expect("wake on destination should find the requeued waiter");
    assert_eq!(woken, 1);
    assert_eq!(first.join().expect("first waiter should finish"), 0);
    assert_eq!(second.join().expect("second waiter should finish"), 0);
}

#[test]
fn futex_wake_op_updates_uaddr2_and_conditionally_wakes_both_queues() {
    const FUTEX_OP_ADD: usize = 1;
    const FUTEX_OP_CMP_EQ: usize = 0;

    let kernel = Arc::new(Kernel::new(N_FRAMES));
    kernel.proc_init();
    let src = Arc::new(AtomicU32::new(1));
    let dst = Arc::new(AtomicU32::new(0));
    let timeout = Arc::new([1usize, 0usize]);
    let src_addr = src.as_ref() as *const AtomicU32 as usize;
    let dst_addr = dst.as_ref() as *const AtomicU32 as usize;
    let timeout_addr = timeout.as_ptr() as usize;

    let first_kernel = kernel.clone();
    let first_src = src.clone();
    let first = thread::spawn(move || {
        let src_addr = first_src.as_ref() as *const AtomicU32 as usize;
        first_kernel
            .dispatch_syscall(SYS_FUTEX, src_addr, 0, 1, timeout_addr, 0, 0)
            .expect("source waiter should be woken")
    });

    let second_kernel = kernel.clone();
    let second_dst = dst.clone();
    let second = thread::spawn(move || {
        let dst_addr = second_dst.as_ref() as *const AtomicU32 as usize;
        second_kernel
            .dispatch_syscall(SYS_FUTEX, dst_addr, 0, 0, timeout_addr, 0, 0)
            .expect("destination waiter should be conditionally woken")
    });

    thread::sleep(Duration::from_millis(25));
    let encoded = (FUTEX_OP_ADD << 28) | (FUTEX_OP_CMP_EQ << 24) | (1 << 12);
    let woken = kernel
        .dispatch_syscall(SYS_FUTEX, src_addr, 5, 1, 1, dst_addr, encoded)
        .expect("wake-op should succeed");

    assert_eq!(dst.load(Ordering::SeqCst), 1);
    assert_eq!(woken, 2);
    assert_eq!(first.join().expect("source waiter should finish"), 0);
    assert_eq!(second.join().expect("destination waiter should finish"), 0);
}

#[test]
fn futex_wake_op_sign_extends_oparg_and_cmparg() {
    const FUTEX_OP_ADD: usize = 1;
    const FUTEX_OP_CMP_EQ: usize = 0;

    let kernel = Kernel::new(N_FRAMES);
    kernel.proc_init();
    let src = AtomicU32::new(0);
    let dst = AtomicU32::new(u32::MAX);
    let src_addr = &src as *const AtomicU32 as usize;
    let dst_addr = &dst as *const AtomicU32 as usize;
    let encoded = (FUTEX_OP_ADD << 28) | (FUTEX_OP_CMP_EQ << 24) | (0xFFF << 12) | 0xFFF;

    let woken = kernel
        .dispatch_syscall(SYS_FUTEX, src_addr, 5, 0, 0, dst_addr, encoded)
        .expect("wake-op should accept signed 12-bit operands");

    assert_eq!(woken, 0);
    assert_eq!(dst.load(Ordering::SeqCst), u32::MAX.wrapping_sub(1));
}

#[test]
fn futex_wake_op_invalid_cmp_does_not_wake_waiters() {
    const FUTEX_OP_ADD: usize = 1;
    const FUTEX_OP_CMP_INVALID: usize = 6;

    let kernel = Arc::new(Kernel::new(N_FRAMES));
    kernel.proc_init();
    let src = Arc::new(AtomicU32::new(1));
    let dst = AtomicU32::new(0);
    let timeout = Arc::new([0usize, 100_000_000usize]);
    let src_addr = src.as_ref() as *const AtomicU32 as usize;
    let dst_addr = &dst as *const AtomicU32 as usize;
    let timeout_addr = timeout.as_ptr() as usize;

    let waiter_kernel = kernel.clone();
    let waiter_src = src.clone();
    let waiter = thread::spawn(move || {
        let src_addr = waiter_src.as_ref() as *const AtomicU32 as usize;
        waiter_kernel.dispatch_syscall(SYS_FUTEX, src_addr, 0, 1, timeout_addr, 0, 0)
    });

    thread::sleep(Duration::from_millis(25));
    let encoded = (FUTEX_OP_ADD << 28) | (FUTEX_OP_CMP_INVALID << 24) | (1 << 12);
    let err = kernel
        .dispatch_syscall(SYS_FUTEX, src_addr, 5, 1, 0, dst_addr, encoded)
        .expect_err("invalid wake-op comparison should fail");

    assert_eq!(err, "einval");
    assert_eq!(dst.load(Ordering::SeqCst), 1);
    assert_eq!(
        waiter.join().expect("waiter thread should finish"),
        Err("timeout")
    );
}

#[test]
fn futex_cmp_requeue_returns_eagain_when_source_value_changed() {
    let kernel = Kernel::new(N_FRAMES);
    kernel.proc_init();
    let src = AtomicU32::new(1);
    let dst = AtomicU32::new(0);
    let src_addr = &src as *const AtomicU32 as usize;
    let dst_addr = &dst as *const AtomicU32 as usize;

    let err = kernel
        .dispatch_syscall(SYS_FUTEX, src_addr, 9, 1, 1, dst_addr, 0)
        .expect_err("cmp-requeue should fail when the source word differs");

    assert_eq!(err, "eagain");
}

#[test]
fn futex_cmp_requeue_wakes_and_moves_after_compare() {
    let kernel = Arc::new(Kernel::new(N_FRAMES));
    kernel.proc_init();
    let src = Arc::new(AtomicU32::new(1));
    let dst = Arc::new(AtomicU32::new(0));
    let timeout = Arc::new([1usize, 0usize]);
    let src_addr = src.as_ref() as *const AtomicU32 as usize;
    let dst_addr = dst.as_ref() as *const AtomicU32 as usize;
    let timeout_addr = timeout.as_ptr() as usize;

    let first_kernel = kernel.clone();
    let first_src = src.clone();
    let first = thread::spawn(move || {
        let src_addr = first_src.as_ref() as *const AtomicU32 as usize;
        first_kernel
            .dispatch_syscall(SYS_FUTEX, src_addr, 0, 1, timeout_addr, 0, 0)
            .expect("first waiter should be woken")
    });

    let second_kernel = kernel.clone();
    let second_src = src.clone();
    let second = thread::spawn(move || {
        let src_addr = second_src.as_ref() as *const AtomicU32 as usize;
        second_kernel
            .dispatch_syscall(SYS_FUTEX, src_addr, 0, 1, timeout_addr, 0, 0)
            .expect("second waiter should be requeued then woken")
    });

    thread::sleep(Duration::from_millis(25));
    let affected = kernel
        .dispatch_syscall(SYS_FUTEX, src_addr, 9, 1, 1, dst_addr, 1)
        .expect("cmp-requeue should succeed when the source word matches");
    assert_eq!(affected, 2);

    let woken = kernel
        .dispatch_syscall(SYS_FUTEX, dst_addr, 1, 1, 0, 0, 0)
        .expect("wake on destination should find the cmp-requeued waiter");
    assert_eq!(woken, 1);
    assert_eq!(first.join().expect("first waiter should finish"), 0);
    assert_eq!(second.join().expect("second waiter should finish"), 0);
}
