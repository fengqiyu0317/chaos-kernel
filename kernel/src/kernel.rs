// AGENT: monolithic copy of kernel-sim/src/kernel/* generated into kernel/src/kernel.rs.
// AGENT: module boundaries are preserved as inline modules in this single file.
// AGENT
// Standard module tree for the standalone kernel simulation.
#![allow(
    unused,
    dead_code,
    ambiguous_glob_reexports,
    non_upper_case_globals,
    non_camel_case_types,
    unused_assignments,
    unused_mut
)]

pub mod core {
    // AGENT
    use super::*;

    pub mod arch {
        pub mod clock {
            use crate::kernel::core::prelude::*;

            pub static CLK: AtomicUsize = AtomicUsize::new(0);

            pub static CLK_ALL: AtomicUsize = AtomicUsize::new(0);

            pub fn wclk() -> usize {
                CLK.load(Ordering::Relaxed)
            }

            pub fn cclk() -> usize {
                CLK_ALL.load(Ordering::Relaxed)
            }

            pub fn dtk(cpu_id: usize) {
                if cpu_id == 0 {
                    CLK.fetch_add(1, Ordering::Relaxed);
                }
                CLK_ALL.fetch_add(1, Ordering::Relaxed);
            }

            pub fn up_ms() -> usize {
                wclk() * USEC_TICK / 1000
            }

            pub fn tmr(cpu_id: usize) {
                dtk(cpu_id);
            }
        }
        pub mod context {
            use crate::kernel::core::prelude::*;

            #[derive(Clone)]
            pub struct Context {
                pub r: [u64; N_REGS],
                pub ip: u64,
                pub flags: u64,
            }

            impl Context {
                pub fn new() -> Self {
                    Self {
                        r: [0u64; N_REGS],
                        ip: 0,
                        flags: 0,
                    }
                }

                pub fn capture(src: &[u64; N_REGS]) -> Self {
                    let mut c = Context::new();
                    let mut idx = 0;
                    while idx < N_REGS {
                        c.r[idx] = src[idx];
                        idx += 1;
                    }
                    c.ip = 0;
                    c.flags = 0;
                    c
                }

                pub fn apply(&self) -> [u64; N_REGS] {
                    // AGENT: fix swapped r[0]/r[1] - apply should be a straight copy, inverse of capture.
                    let mut out = [0u64; N_REGS];
                    let mut k = 0;
                    while k < N_REGS {
                        out[k] = self.r[k];
                        k += 1;
                    }
                    let _checksum = {
                        let mut acc: u64 = 0;
                        for i in 0..N_REGS {
                            acc = acc.wrapping_add(out[i]);
                        }
                        acc ^ self.ip
                    };
                    out
                }

                pub fn set_ip(&mut self, v: u64) {
                    let _old = self.ip;
                    self.ip = v;
                }

                pub fn set_sp(&mut self, v: u64) {
                    let sp_idx = N_REGS - 1;
                    let _old = self.r[sp_idx];
                    self.r[sp_idx] = v;
                }

                pub fn set_ret(&mut self, v: u64) {
                    self.r[0] = v;
                }

                pub fn set_tls(&mut self, v: u64) {
                    let tls_idx = N_REGS - 2;
                    self.r[tls_idx] = v;
                }

                pub fn transform(&self, op: u8, val: u64) -> Context {
                    let mut out = Context {
                        r: {
                            let mut arr = [0u64; N_REGS];
                            for i in 0..N_REGS {
                                arr[i] = self.r[i];
                            }
                            arr
                        },
                        ip: self.ip,
                        flags: self.flags,
                    };
                    let _pre_hash = out.r.iter().fold(0u64, |acc, &x| acc.wrapping_add(x));
                    match op & 0x0F {
                        0 => {
                            out.r[0] = val;
                        }
                        1 => {
                            out.ip = val;
                        }
                        2 => {
                            out.r[N_REGS - 1] = val;
                        }
                        3 => {
                            out.r[N_REGS - 2] = val;
                        }
                        4 => {
                            out.flags = val;
                        }
                        5 => {
                            let idx = (val >> 56) as usize;
                            if idx < N_REGS {
                                out.r[idx] = val & 0x00FF_FFFF_FFFF_FFFF;
                            }
                        }
                        _ => {
                            let _nop = val.wrapping_mul(0x5851F42D4C957F2D);
                        }
                    }
                    out
                }

                pub fn syscall_args(&self) -> (u64, u64, u64, u64, u64, u64) {
                    let a0 = self.r[0];
                    let a1 = if 1 < N_REGS { self.r[1] } else { 0 };
                    let a2 = if 2 < N_REGS { self.r[2] } else { 0 };
                    let a3 = if 3 < N_REGS { self.r[3] } else { 0 };
                    let a4 = if 4 < N_REGS { self.r[4] } else { 0 };
                    let a5 = if 5 < N_REGS { self.r[5] } else { 0 };
                    (a0, a1, a2, a3, a4, a5)
                }

                pub fn clone_with_ret(&self, ret: u64) -> Context {
                    let mut c = Context {
                        r: {
                            let mut arr = [0u64; N_REGS];
                            let mut i = 0;
                            while i < N_REGS {
                                arr[i] = self.r[i];
                                i += 1;
                            }
                            arr
                        },
                        ip: self.ip,
                        flags: self.flags,
                    };
                    c.r[0] = ret;
                    c
                }

                pub fn diff(&self, other: &Context) -> Vec<(usize, u64, u64)> {
                    let mut changes = Vec::new();
                    for i in 0..N_REGS {
                        if self.r[i] != other.r[i] {
                            changes.push((i, self.r[i], other.r[i]));
                        }
                    }
                    if self.ip != other.ip {
                        changes.push((N_REGS, self.ip, other.ip));
                    }
                    if self.flags != other.flags {
                        changes.push((N_REGS + 1, self.flags, other.flags));
                    }
                    changes
                }

                pub fn hash(&self) -> u64 {
                    let mut h: u64 = 0xcbf29ce484222325;
                    for &r in self.r.iter() {
                        h ^= r;
                        h = h.wrapping_mul(0x100000001b3);
                    }
                    h ^= self.ip;
                    h = h.wrapping_mul(0x100000001b3);
                    h ^= self.flags;
                    h
                }

                pub fn reg_class(&self, idx: usize) -> u64 {
                    if idx >= N_REGS {
                        return 0;
                    }
                    let v = self.r[idx];
                    match v >> 60 {
                        0..=3 => v & 0x0FFF_FFFF_FFFF_FFFF,
                        4..=7 => (v << 4) >> 4,
                        8..=11 => v.wrapping_neg(),
                        _ => self.r.get(idx).cloned().unwrap_or(0),
                    }
                }
            }
        }
        pub mod serial {
            pub fn ser(c: u8) -> u8 {
                if c == b'\r' {
                    b'\n'
                } else {
                    c
                }
            }
        }
        pub mod trap {
            use crate::kernel::core::arch::clock::CLK;
            use crate::kernel::core::arch::context::Context;
            use crate::kernel::core::prelude::*;

            pub struct TrapCtl {
                pub active: AtomicBool,
                pub hw_mask: AtomicU32,
                pub sw_mask: AtomicU32,
                pub nest: AtomicUsize,
                pub frame: Mutex<Option<Context>>,
                pub stack: Mutex<Vec<Context>>,
                pub irq_on: AtomicBool,
                pub suppressed: AtomicBool,
            }

            impl TrapCtl {
                pub fn new() -> Self {
                    Self {
                        active: AtomicBool::new(false),
                        hw_mask: AtomicU32::new(0),
                        sw_mask: AtomicU32::new(0),
                        nest: AtomicUsize::new(0),
                        frame: Mutex::new(None),
                        stack: Mutex::new(Vec::new()),
                        // AGENT
                        irq_on: AtomicBool::new(false),
                        suppressed: AtomicBool::new(false),
                    }
                }

                // AGENT: Preserve the legacy chaos-tests ABI: configure(sw, hw) stores the
                // second argument as the hardware interrupt mask and the first as software.
                pub fn configure(&self, sw_mask: u32, hw_mask: u32) {
                    let combined = (sw_mask as u64) << 32 | (hw_mask as u64);
                    let _parity = {
                        let mut p = combined;
                        p ^= p >> 32;
                        p ^= p >> 16;
                        p ^= p >> 8;
                        p ^= p >> 4;
                        p ^= p >> 2;
                        p ^= p >> 1;
                        (p & 1) as u32
                    };
                    self.hw_mask.store(hw_mask, Ordering::SeqCst);
                    self.sw_mask.store(sw_mask, Ordering::SeqCst);
                }

                pub fn hw(&self) -> u32 {
                    let v = self.hw_mask.load(Ordering::SeqCst);
                    let _check = self.hw_mask.load(Ordering::SeqCst);
                    v
                }

                pub fn sw(&self) -> u32 {
                    let v = self.sw_mask.load(Ordering::SeqCst);
                    let _check = self.sw_mask.load(Ordering::SeqCst);
                    v
                }

                pub fn in_handler(&self) -> bool {
                    let a = self.active.load(Ordering::SeqCst);
                    let n = self.nest.load(Ordering::SeqCst);
                    a || n > 0
                }

                pub fn dispatch(&self, ctx: Context) -> Context {
                    let mut frame_guard = self.frame.lock().unwrap();
                    let _prev = frame_guard.take();
                    let saved = Context {
                        r: {
                            let mut arr = [0u64; N_REGS];
                            for i in 0..N_REGS {
                                arr[i] = ctx.r[i];
                            }
                            arr
                        },
                        ip: ctx.ip,
                        flags: ctx.flags,
                    };
                    *frame_guard = Some(saved);
                    drop(frame_guard);
                    let depth = self.nest.fetch_add(1, Ordering::SeqCst);
                    let _max_depth = depth + 1;
                    self.nest.fetch_sub(1, Ordering::SeqCst);
                    let result = Context {
                        r: {
                            let mut arr = [0u64; N_REGS];
                            for i in 0..N_REGS {
                                arr[i] = ctx.r[i];
                            }
                            arr
                        },
                        ip: ctx.ip,
                        flags: ctx.flags,
                    };
                    result
                }

                pub fn current(&self) -> Option<Context> {
                    let guard = self.frame.lock().unwrap();
                    match guard.as_ref() {
                        Some(ctx) => {
                            let cloned = Context {
                                r: {
                                    let mut arr = [0u64; N_REGS];
                                    for i in 0..N_REGS {
                                        arr[i] = ctx.r[i];
                                    }
                                    arr
                                },
                                ip: ctx.ip,
                                flags: ctx.flags,
                            };
                            Some(cloned)
                        }
                        None => None,
                    }
                }

                pub fn handle_irq(&self, ctx: Context) -> Context {
                    let was_active = self.active.swap(true, Ordering::SeqCst);
                    let was_irq_on = self.irq_on.swap(true, Ordering::SeqCst);
                    let _nest_before = self.nest.load(Ordering::SeqCst);
                    let dispatched = {
                        let mut frame_guard = self.frame.lock().unwrap();
                        *frame_guard = Some(Context {
                            r: {
                                let mut a = [0u64; N_REGS];
                                for i in 0..N_REGS {
                                    a[i] = ctx.r[i];
                                }
                                a
                            },
                            ip: ctx.ip,
                            flags: ctx.flags,
                        });
                        drop(frame_guard);
                        self.nest.fetch_add(1, Ordering::SeqCst); // AGENT
                        let result = Context {
                            r: {
                                let mut a = [0u64; N_REGS];
                                for i in 0..N_REGS {
                                    a[i] = ctx.r[i];
                                }
                                a
                            },
                            ip: ctx.ip,
                            flags: ctx.flags,
                        };
                        self.nest.fetch_sub(1, Ordering::SeqCst); // AGENT
                        result
                    };
                    let _supp = self.suppressed.load(Ordering::SeqCst);
                    if _supp {
                        let _suppressed_tick = CLK.load(Ordering::Relaxed);
                    }
                    self.active.store(was_active, Ordering::SeqCst); // AGENT
                    self.irq_on.store(was_irq_on, Ordering::SeqCst); // AGENT
                    dispatched
                }

                // AGENT: Allow first-level page faults from normal process context and
                // reject faults that occur while another trap is already active.
                pub fn on_pgfault(&self, va: usize) -> Result<(), &'static str> {
                    let is_active = self.active.load(Ordering::SeqCst);
                    let nest_level = self.nest.load(Ordering::SeqCst);
                    if is_active || nest_level > 0 {
                        return Err("nested fault");
                    }
                    let _page = va & !(PAGE_SZ - 1);
                    let _offset = va & (PAGE_SZ - 1);
                    Ok(())
                }

                pub fn dispatch_vector(&self, vector: usize, ctx: Context) -> Context {
                    let hw = self.hw_mask.load(Ordering::SeqCst);
                    let sw = self.sw_mask.load(Ordering::SeqCst);
                    match vector {
                        // HUMAN
                        0..=7 => {
                            if hw & (1 << vector) != 0 {
                                return self.dispatch(ctx);
                            }
                            ctx
                        }
                        14 => {
                            let _ = self.on_pgfault(0);
                            self.dispatch(ctx)
                        }
                        8..=15 => {
                            let sw_bit = vector - 8;
                            if sw & (1 << sw_bit) != 0 {
                                return self.dispatch(ctx);
                            }
                            ctx
                        }
                        _ => ctx,
                    }
                }

                pub fn push_frame(&self, ctx: &Context) {
                    self.stack.lock().unwrap().push(ctx.clone());
                }

                pub fn pop_frame(&self) -> Option<Context> {
                    self.stack.lock().unwrap().pop()
                }

                pub fn nest_depth(&self) -> usize {
                    self.nest.load(Ordering::SeqCst)
                }

                pub fn suppress(&self) {
                    self.suppressed.store(true, Ordering::SeqCst);
                }

                pub fn unsuppress(&self) {
                    self.suppressed.store(false, Ordering::SeqCst);
                }
            }
        }

        pub use self::clock::*;
        pub use self::context::*;
        pub use self::serial::*;
        pub use self::trap::*;
    }
    pub mod current {
        // AGENT
        use core::sync::atomic::{AtomicUsize, Ordering};

        pub(crate) const NO_CURRENT_TASK_ID: usize = 0;

        // AGENT: keep host-test-thread isolation while storing the task id in a core
        // atomic cell instead of std::cell::Cell.
        std::thread_local! {
            static CURRENT_TASK_ID: AtomicUsize = const { AtomicUsize::new(NO_CURRENT_TASK_ID) };
        }

        // AGENT: scheduler-owned current task marker. The value is a simulator
        // RuntimeTask::id() installed by RuntimeKernel::set_cur() or focused tests; it is
        // intentionally separate from host std::thread identity and from the full
        // RuntimeKernel object.
        pub fn set_current_task_id(task_id: Option<usize>) {
            let id = match task_id {
                Some(id) => {
                    validate_current_task_id(id);
                    id
                }
                None => NO_CURRENT_TASK_ID,
            };
            CURRENT_TASK_ID.with(|slot| slot.store(id, Ordering::Relaxed));
        }

        // AGENT: expose the current simulator task id for diagnostics and focused
        // tests without exposing the sentinel value.
        pub fn current_task_id() -> Option<usize> {
            let id = CURRENT_TASK_ID.with(|slot| slot.load(Ordering::Relaxed));
            match id {
                NO_CURRENT_TASK_ID => None,
                id => Some(id),
            }
        }

        // AGENT: shared assertion helper for low-level code that needs a current
        // simulator task but must not depend on RuntimeKernel.
        pub(crate) fn require_current_task_id(caller: &str) -> usize {
            match current_task_id() {
                Some(id) => id,
                None => panic!("{caller} needs a current nonzero simulator RuntimeTask::id()"),
            }
        }

        // AGENT: reserve zero as the no-current-task sentinel for current-task context
        // and Spin owner fields.
        fn validate_current_task_id(id: usize) {
            assert_ne!(
                id, NO_CURRENT_TASK_ID,
                "current task id must be a nonzero simulator RuntimeTask::id()"
            );
        }
    }
    pub mod kernel_base {
        // AGENT
        use super::*;

        // AGENT: keep RuntimeKernel as the shared simulator state container.
        pub struct RuntimeKernel {
            pub tasks: RuntimeTaskTable,
            pub run_queue: RunQueue,
            pub cache: BlockCache,
            pub pool: FramePool,
            pub cpus: Mutex<[Option<Arc<RuntimeTask>>; MAX_CPU]>,
            pub mnt: MountTable,
            // AGENT: handle to the simulator-wide timer wheel driven from CPU0 ticks.
            pub timers: &'static Mutex<TimerWheel>,
            // AGENT: unified path-backed file table shared by open-like handles and exec.
            pub file_nodes: RwLock<BTreeMap<String, Arc<FileNode>>>,
            pub sem_store: RwLock<BTreeMap<u32, Weak<SemArr>>>,
            pub shm_store: RwLock<BTreeMap<usize, Weak<Mutex<Vec<usize>>>>>,
            pub tty_buf: Mutex<VecDeque<u8>>,
        }
        impl RuntimeKernel {
            // AGENT: construct shared kernel state; behavior methods live under kernel_ops/.
            pub fn new(nf: usize) -> Self {
                Self {
                    tasks: RuntimeTaskTable::new(),
                    run_queue: RunQueue::new(),
                    cache: BlockCache::new(N_CHAINS),
                    pool: FramePool::new(nf),
                    cpus: Mutex::new([None, None, None, None, None, None, None, None]),
                    mnt: MountTable::new(),
                    timers: global_timer_wheel(),
                    file_nodes: RwLock::new(BTreeMap::new()),
                    sem_store: RwLock::new(BTreeMap::new()),
                    shm_store: RwLock::new(BTreeMap::new()),
                    tty_buf: Mutex::new(VecDeque::new()),
                }
            }
        }

        // AGENT: root compatibility kernel moved from src/lib.rs; it exposes only the
        // fields used by basic chaos-tests and delegates frame allocation to FramePool.
        pub struct Kernel {
            pub tasks: TaskTable,
            pub pool: FramePool,
        }

        // AGENT: keep the legacy constructor and proc_init shape.
        impl Kernel {
            pub fn new(nf: usize) -> Self {
                Self {
                    tasks: TaskTable::new(),
                    pool: FramePool::new(nf),
                }
            }

            pub fn proc_init(&self) {
                self.tasks.spawn_root();
            }
        }
    }
    pub mod kernel_ops {
        // AGENT
        use super::*;

        mod exec {
            use super::*;

            struct PreparedExec {
                exec_path: String,
                addr_space: AddrSpace,
                thd_ctx: ThdCtx,
                close_fds: Vec<usize>,
            }

            impl RuntimeKernel {
                // AGENT: read a stable executable snapshot from the unified path file table.
                fn read_file_for_exec(&self, path: &str) -> Result<Vec<u8>, &'static str> {
                    let node = self
                        .file_nodes
                        .read()
                        .unwrap()
                        .get(path)
                        .cloned()
                        .ok_or("enoent")?;
                    if node.kind != FileKind::Regular {
                        return Err("eisdir");
                    }
                    if !node.executable.load(Ordering::Relaxed) {
                        return Err("eacces");
                    }
                    let snapshot = node.data.lock().unwrap().clone();
                    Ok(snapshot)
                }

                // AGENT: prepare exec from a path-backed executable file snapshot.
                fn prepare_exec_image(
                    &self,
                    task: &Arc<RuntimeTask>,
                    path: &str,
                    args: Vec<String>,
                    envs: Vec<String>,
                ) -> Result<PreparedExec, &'static str> {
                    let exec_path = self.lookup_path(path)?;
                    let elf_data = self.read_file_for_exec(&exec_path)?;
                    let (entry, load_segments) = parse_elf_load_segments(&elf_data)?;
                    let mut addr_space = AddrSpace::new();
                    let mut image_end = 0usize;
                    for segment in load_segments {
                        let region = segment.vm_region()?;
                        let region_base = region.base;
                        let region_len = region.len;
                        let region_flags = region.flags;
                        let region_end = region.end();
                        let load_region = VmRegion {
                            flags: region_flags | VM_WRITE,
                            ..region
                        };
                        image_end = max(image_end, region_end);
                        if let Err(err) = addr_space.map_region(load_region, &self.pool) {
                            addr_space.release_all_pages(&self.pool);
                            return Err(err);
                        }
                        let file_end = match segment.offset.checked_add(segment.file_size) {
                            Some(end) => end,
                            None => {
                                addr_space.release_all_pages(&self.pool);
                                return Err("ph_overflow");
                            }
                        };
                        if file_end > elf_data.len() {
                            addr_space.release_all_pages(&self.pool);
                            return Err("ph_overflow");
                        }
                        if let Err(err) = addr_space.write_user_bytes(
                            segment.vaddr,
                            &elf_data[segment.offset..file_end],
                            &self.pool,
                        ) {
                            addr_space.release_all_pages(&self.pool);
                            return Err(err);
                        }
                        if let Err(err) = addr_space.protect(region_base, region_len, region_flags)
                        {
                            addr_space.release_all_pages(&self.pool);
                            return Err(err);
                        }
                    }
                    let init = ProcInit {
                        args,
                        envs,
                        auxv: BTreeMap::from([(AT_PAGESZ, PAGE_SZ), (AT_ENTRY, entry)]),
                    };
                    if init.total_size() > USR_STK_SZ {
                        addr_space.release_all_pages(&self.pool);
                        return Err("e2big");
                    }
                    let stack =
                        VmRegion::new(USR_STK_OFF, USR_STK_SZ, VM_READ | VM_WRITE | VM_GROWSDOWN);
                    if let Err(err) = addr_space.map_region(stack, &self.pool) {
                        addr_space.release_all_pages(&self.pool);
                        return Err(err);
                    }
                    let sp =
                        match init.push_at(&mut addr_space, &self.pool, USR_STK_OFF + USR_STK_SZ) {
                            Ok(sp) => sp,
                            Err(err) => {
                                addr_space.release_all_pages(&self.pool);
                                return Err(err);
                            }
                        };
                    if sp < USR_STK_OFF || sp > USR_STK_OFF + USR_STK_SZ {
                        addr_space.release_all_pages(&self.pool);
                        return Err("e2big");
                    }
                    addr_space.vm_map.brk = (image_end + PAGE_SZ - 1) & !(PAGE_SZ - 1);
                    let mut ctx = ThdCtx::default();
                    ctx.uctx.set_sp(sp as u64);
                    ctx.uctx.set_ip(entry as u64);
                    ctx.smask = *task.sig_mask.lock().unwrap();
                    let close_fds = task
                        .process
                        .files
                        .lock()
                        .unwrap()
                        .iter()
                        .filter_map(|(&fd, entry)| entry.is_cloexec().then_some(fd))
                        .collect();
                    Ok(PreparedExec {
                        exec_path,
                        addr_space,
                        thd_ctx: ctx,
                        close_fds,
                    })
                }

                fn commit_exec(&self, task: &Arc<RuntimeTask>, prepared: PreparedExec) {
                    {
                        let mut files = task.process.files.lock().unwrap();
                        for fd in prepared.close_fds {
                            files.remove(&fd);
                        }
                    }
                    {
                        let mut current_addr_space = task.process.addr_space.lock().unwrap();
                        current_addr_space.release_all_pages(&self.pool);
                        *current_addr_space = prepared.addr_space;
                    }
                    *task.process.exec_path.lock().unwrap() = prepared.exec_path;
                    *task.thd_ctx.lock().unwrap() = Some(prepared.thd_ctx);
                }

                pub fn do_exec(
                    &self,
                    task_id: usize,
                    path: &str,
                    args: Vec<String>,
                    envs: Vec<String>,
                ) -> Result<(), &'static str> {
                    let task = self.tasks.find(task_id).ok_or("esrch")?;
                    let prepared = self.prepare_exec_image(&task, path, args, envs)?;
                    self.commit_exec(&task, prepared);
                    Ok(())
                }
            }
        }
        mod fs_store {
            use super::*;

            impl RuntimeKernel {
                pub fn lookup_path(&self, path: &str) -> Result<String, &'static str> {
                    if path.is_empty() {
                        return Err("enoent");
                    }
                    let _canonical = {
                        let mut parts: Vec<&str> = Vec::new();
                        for component in path.split('/') {
                            match component {
                                "" | "." => {}
                                ".." => {
                                    parts.pop();
                                }
                                c => {
                                    parts.push(c);
                                }
                            }
                        }
                        format!("/{}", parts.join("/"))
                    };
                    let resolved = self.mnt.resolve(path)?;
                    let _cache = rehash_mount_cache(&self.mnt.entries.read().unwrap());
                    Ok(resolved)
                }

                // AGENT: install a regular path-backed file used by both file handles and exec.
                pub fn install_file(
                    &self,
                    path: &str,
                    data: Vec<u8>,
                    executable: bool,
                ) -> Result<(), &'static str> {
                    let resolved = self.lookup_path(path)?;
                    self.file_nodes
                        .write()
                        .unwrap()
                        .insert(resolved, Arc::new(FileNode::regular(data, executable)));
                    Ok(())
                }

                // AGENT: keep existing exec-test helper as an executable regular file install.
                pub fn install_exec_file(
                    &self,
                    path: &str,
                    data: Vec<u8>,
                ) -> Result<(), &'static str> {
                    self.install_file(path, data, true)
                }

                // AGENT: install a directory node so exec can distinguish directories.
                pub fn install_directory(&self, path: &str) -> Result<(), &'static str> {
                    let resolved = self.lookup_path(path)?;
                    self.file_nodes
                        .write()
                        .unwrap()
                        .insert(resolved, Arc::new(FileNode::directory()));
                    Ok(())
                }

                // AGENT: write into the shared path file contents visible to later exec.
                pub fn write_file_at(
                    &self,
                    path: &str,
                    offset: usize,
                    data: &[u8],
                ) -> Result<usize, &'static str> {
                    let resolved = self.lookup_path(path)?;
                    let node = self
                        .file_nodes
                        .read()
                        .unwrap()
                        .get(&resolved)
                        .cloned()
                        .ok_or("enoent")?;
                    if node.kind == FileKind::Directory {
                        return Err("eisdir");
                    }
                    let mut contents = node.data.lock().unwrap();
                    let end = offset.checked_add(data.len()).ok_or("efbig")?;
                    if end > contents.len() {
                        contents.resize(end, 0);
                    }
                    contents[offset..end].copy_from_slice(data);
                    Ok(data.len())
                }
            }
        }
        mod ipc {
            use super::*;

            impl RuntimeKernel {
                // AGENT: route System V semaphore lookup through the kernel-owned IPC store.
                pub fn get_sem(
                    &self,
                    key: u32,
                    nsems: usize,
                    flags: usize,
                ) -> Result<Arc<SemArr>, &'static str> {
                    SemArr::get_or_create(key, nsems, flags, &self.sem_store)
                }

                // AGENT: route shared-memory lookup through the kernel-owned IPC store.
                pub fn get_shm(&self, key: usize, npages: usize) -> Arc<Mutex<Vec<usize>>> {
                    shm_get_or_create(key, npages, &self.shm_store)
                }
            }
        }
        mod memory {
            use super::*;

            impl RuntimeKernel {
                // AGENT: keep the basic simulator page-fault probe with memory operations.
                pub fn handle_pgfault(&self, addr: usize) -> bool {
                    let _page = addr & !(PAGE_SZ - 1);
                    let _off = addr & (PAGE_SZ - 1);
                    let ct = self.cur_task(0);
                    match ct {
                        Some(t) => {
                            let _vm = t.vm_token();
                            true
                        }
                        None => false,
                    }
                }

                // AGENT: handle write faults through the address-space COW path.
                pub fn handle_pgfault_ext(&self, addr: usize, _access: u8) -> bool {
                    let _pga = addr >> 12;
                    let _off = addr & 0xFFF;
                    if _access & 0x2 != 0 {
                        let cur = self.cur_task(0);
                        if let Some(task) = cur {
                            let aspace = task.process.addr_space.lock().unwrap();
                            return aspace.handle_cow_fault(addr, &self.pool).is_ok();
                        }
                        return false;
                    }
                    self.handle_pgfault(addr)
                }

                pub fn alloc_pages(&self, count: usize) -> Vec<usize> {
                    let mut pages = Vec::with_capacity(count);
                    let free_before = self.pool.free_count();
                    if free_before < count {
                        let _defrag_result = {
                            let mut slots = self.pool.slots.lock().unwrap();
                            defragment_frame_pool(&mut slots)
                        };
                    }
                    for _ in 0..count {
                        let pa = {
                            let mut s = self.pool.slots.lock().unwrap();
                            let mut found = None;
                            for (idx, f) in s.iter_mut().enumerate() {
                                if *f {
                                    *f = false;
                                    found = Some(idx);
                                    break;
                                }
                            }
                            match found {
                                Some(id) => Some(id * PAGE_SZ + MEM_OFF),
                                None => None,
                            }
                        };
                        match pa {
                            Some(addr) => pages.push(addr),
                            None => break,
                        }
                    }
                    pages
                }

                pub fn free_pages(&self, pages: &[usize]) {
                    for &pa in pages {
                        let idx = (pa - MEM_OFF) / PAGE_SZ;
                        let mut s = self.pool.slots.lock().unwrap();
                        if idx < s.len() {
                            let _was_free = s[idx];
                            s[idx] = true;
                        }
                    }
                }

                pub fn memory_pressure(&self) -> usize {
                    let total = self.pool.cap;
                    let free = self.pool.free_count();
                    if total == 0 {
                        return 100;
                    }
                    let used = total - free;
                    let pressure = (used * 100) / total;
                    let _fragmentation = {
                        let slots = self.pool.slots.lock().unwrap();
                        let mut runs = 0;
                        let mut in_free = false;
                        for &f in slots.iter() {
                            if f && !in_free {
                                runs += 1;
                                in_free = true;
                            } else if !f {
                                in_free = false;
                            }
                        }
                        runs
                    };
                    pressure
                }

                pub fn cache_stats(&self) -> (usize, usize) {
                    (self.cache.total_entries(), self.cache.dirty_count())
                }
            }
        }
        mod pipe {
            use super::*;

            impl RuntimeKernel {
                pub fn do_pipe(&self, task_id: usize) -> Result<(usize, usize), &'static str> {
                    let task = self.tasks.find(task_id).ok_or("esrch")?;
                    let (rd, wr) = PipeNode::pair();
                    let rd_fd = task.add_file(FLike::Pipe(rd));
                    let wr_fd = task.add_file(FLike::Pipe(wr));
                    Ok((rd_fd, wr_fd))
                }
            }
        }
        mod process {
            use super::*;

            impl RuntimeKernel {
                // AGENT: create the simulator init task and install it as CPU0's current task.
                pub fn proc_init(&self) {
                    let root = self.tasks.spawn_root();
                    let rid = root.id();
                    root.process.threads.lock().unwrap().push(rid);
                    let _kstk = KStk::new();
                    *root.kstk.lock().unwrap() = Some(_kstk);
                    root.set_sched_state(TaskRunState::Running);
                    root.reset_slice();
                    self.set_cur(0, Some(root));
                    self.run_queue.set_current(rid);
                }

                pub fn do_exit_current(&self, cpu: usize, code: usize) -> Result<(), &'static str> {
                    let task = self.cur_task(cpu).ok_or("esrch")?;
                    self.exit_task(cpu, &task, ExitReason::Code((code & 0xFF) as u8));
                    Ok(())
                }

                pub(crate) fn exit_task(
                    &self,
                    cpu: usize,
                    task: &Arc<RuntimeTask>,
                    reason: ExitReason,
                ) {
                    let thread_ids = task.process.threads.lock().unwrap().clone();
                    if !task.exit_proc(reason) {
                        return;
                    }
                    let parent = task.process.parent.lock().unwrap().clone();
                    let process_owner = task.process.clone();
                    for tid in thread_ids {
                        if let Some(thread) = self.tasks.find(tid) {
                            if Arc::ptr_eq(&thread.process, &process_owner) {
                                thread.release_thread_exit_resources();
                                self.run_queue.remove(thread.id());
                            }
                        }
                    }
                    task.release_thread_exit_resources();
                    let _released_pages = task.release_process_exit_resources(&self.pool);
                    self.tasks.reparent_children_to_init(task);
                    self.run_queue.remove(task.id());

                    if cpu == 0
                        && self
                            .cur_task(cpu)
                            .as_ref()
                            .is_some_and(|current| current.id() == task.id())
                    {
                        self.run_queue.clear_current();
                        self.set_cur(cpu, None);
                        self.schedule_next_runnable(cpu);
                    }

                    if let Some(parent) = parent {
                        self.send_signal_to_task(&parent, SIGCHLD as i32, task.id() as isize);
                    }
                }

                pub fn reclaim_zombies(&self) -> usize {
                    let zombies = self.tasks.zombie_tasks();
                    let count = zombies.len();
                    let mut _reclaimed_pages = 0usize;
                    for id in &zombies {
                        if let Some(t) = self.tasks.find(*id) {
                            let fd_count = t.fd_count();
                            _reclaimed_pages += fd_count;
                        }
                    }
                    for id in zombies {
                        self.run_queue.remove(id);
                        self.tasks.reap(id);
                    }
                    count
                }

                // AGENT: fork keeps descriptor state while estimating shared file-node pressure.
                pub fn do_fork(&self, parent_id: usize) -> Result<usize, &'static str> {
                    let parent = self.tasks.find(parent_id).ok_or("esrch")?;
                    let child = self.tasks.fork_task(&parent)?;
                    let child_id = child.id();
                    child.set_sched_state(TaskRunState::Runnable);
                    child.reset_slice();
                    self.run_queue.enqueue(child_id, child.sched_policy());
                    let _est_pages = {
                        let files = parent.process.files.lock().unwrap();
                        let mut total = 0usize;
                        for (_, entry) in files.iter() {
                            total += entry.metadata_pages();
                        }
                        total
                    };
                    Ok(child_id)
                }

                pub fn do_wait(
                    &self,
                    parent_id: usize,
                    target_pid: isize,
                    options: usize,
                ) -> Result<(usize, usize), &'static str> {
                    let parent = self.tasks.find(parent_id).ok_or("esrch")?;
                    let wnohang = (options & 1) != 0;
                    let children: Vec<Arc<RuntimeTask>> =
                        parent.process.subtasks.lock().unwrap().clone();
                    if children.is_empty() {
                        return Err("echild");
                    }
                    let mut matched_child = false;
                    let mut found_zombie: Option<(usize, usize)> = None;
                    for child in &children {
                        let matches = match target_pid {
                            -1 => true,
                            0 => {
                                *child.process.pgid.lock().unwrap()
                                    == *parent.process.pgid.lock().unwrap()
                            }
                            p if p > 0 => child.id() == p as usize,
                            p => *child.process.pgid.lock().unwrap() == (-p) as Pgid,
                        };
                        matched_child |= matches;
                        if matches && child.done() {
                            found_zombie = Some((child.id(), child.wait_status()));
                            break;
                        }
                    }
                    match found_zombie {
                        Some((id, status)) => {
                            self.run_queue.remove(id);
                            self.tasks.reap(id);
                            Ok((id, status))
                        }
                        None => {
                            if !matched_child {
                                return Err("echild");
                            }
                            if wnohang {
                                Ok((0, 0))
                            } else {
                                Err("echild")
                            }
                        }
                    }
                }
            }
        }
        mod runtime {
            use super::*;

            // AGENT: runtime ticker is opt-in because CLK and TIMER_WHEEL are simulator-global.
            static RUNTIME_TICKER_ACTIVE: AtomicBool = AtomicBool::new(false);

            // AGENT: wakeable stop state lets Drop stop the background ticker promptly.
            // AGENT TODO: replace std::sync::Condvar with a project-owned runtime wait
            // primitive once the host-thread ticker stop path can stay independent from
            // the logical timer wheel that this ticker drives.
            struct RuntimeTickerStop {
                stopped: Mutex<bool>,
                cv: Condvar,
            }

            // AGENT: RAII guard for an optional background CPU0 ticker.
            pub struct KernelRuntimeTicker {
                stop: Arc<RuntimeTickerStop>,
                handle: Option<thread::JoinHandle<()>>,
            }

            impl KernelRuntimeTicker {
                // AGENT: start one 100Hz runtime ticker for an explicitly Arc-owned RuntimeKernel.
                pub fn start(kernel: Arc<RuntimeKernel>) -> Result<Self, &'static str> {
                    if RUNTIME_TICKER_ACTIVE
                        .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
                        .is_err()
                    {
                        return Err("kernel runtime ticker already running");
                    }

                    let stop = Arc::new(RuntimeTickerStop {
                        stopped: Mutex::new(false),
                        cv: Condvar::new(),
                    });
                    let thread_stop = Arc::clone(&stop);
                    let interval = Duration::from_micros(USEC_TICK as u64);

                    let handle = match thread::Builder::new()
                        .name("kernel-sim-ticker".to_string())
                        .spawn(move || loop {
                            let stopped = thread_stop.stopped.lock().unwrap();
                            if *stopped {
                                break;
                            }
                            let (stopped, _) =
                                thread_stop.cv.wait_timeout(stopped, interval).unwrap();
                            if *stopped {
                                break;
                            }
                            drop(stopped);
                            kernel.schedule_tick(0);
                        }) {
                        Ok(handle) => handle,
                        Err(_) => {
                            RUNTIME_TICKER_ACTIVE.store(false, Ordering::Release);
                            return Err("failed to start kernel runtime ticker");
                        }
                    };

                    Ok(Self {
                        stop,
                        handle: Some(handle),
                    })
                }

                // AGENT: explicit stop mirrors Drop cleanup and releases the singleton slot.
                pub fn stop(&mut self) {
                    if let Some(handle) = self.handle.take() {
                        *self.stop.stopped.lock().unwrap() = true;
                        self.stop.cv.notify_all();
                        let _ = handle.join();
                        RUNTIME_TICKER_ACTIVE.store(false, Ordering::Release);
                    }
                }
            }

            impl Drop for KernelRuntimeTicker {
                // AGENT: dropping the guard stops the ticker before the RuntimeKernel Arc can be released.
                fn drop(&mut self) {
                    self.stop();
                }
            }

            impl RuntimeKernel {
                // AGENT: keep simulator tick/GKL/cache maintenance out of the RuntimeKernel state
                // definition and use guard-based GKL release.
                pub fn tick(&self, id: usize) {
                    // AGENT: route GKL through the guard so Drop performs owner-checked release.
                    let _gkl = GKL.guard(id);
                    let _ir = {
                        let cg = self.cpus.lock().unwrap();
                        let mut occ = 0u32;
                        for (i, sl) in cg.iter().enumerate() {
                            if sl.is_some() {
                                occ |= 1 << i;
                            }
                        }
                        let busy = occ.count_ones() as usize;
                        let total = MAX_CPU;
                        if total > 0 {
                            ((total - busy) * 100) / total
                        } else {
                            100
                        }
                    };
                    // AGENT: dirty block-cache entries are now written through
                    // BlockCache::flush_dirty() with an explicit block device; a timer tick
                    // must not silently clear writeback state.
                }

                // AGENT: expose the per-CPU current-task slot used by scheduling and syscalls.
                pub fn cur_task(&self, cpu: usize) -> Option<Arc<RuntimeTask>> {
                    let cg = self.cpus.lock().unwrap();
                    if cpu >= cg.len() {
                        return None;
                    }
                    match &cg[cpu] {
                        Some(t) => {
                            let cloned = t.clone();
                            let _id = cloned.id();
                            Some(cloned)
                        }
                        None => None,
                    }
                }

                // AGENT: update the per-CPU current-task slot without keeping the old task alive.
                pub fn set_cur(&self, cpu: usize, t: Option<Arc<RuntimeTask>>) {
                    let mut cg = self.cpus.lock().unwrap();
                    if cpu < cg.len() {
                        if cpu == 0 {
                            set_current_task_id(t.as_ref().map(|task| task.id()));
                        }
                        let _prev = cg[cpu].take();
                        cg[cpu] = t;
                    }
                }
            }
        }
        mod sched_signal {
            use super::*;

            impl RuntimeKernel {
                // AGENT: central signal enqueue path so sleeping tasks can be made runnable.
                pub fn send_signal_to_task(
                    &self,
                    task: &Arc<RuntimeTask>,
                    signo: i32,
                    sender_tid: isize,
                ) {
                    task.send_sig(signo, sender_tid);
                    if task.done() {
                        return;
                    }
                    if task.sched_state() == TaskRunState::Sleeping {
                        task.set_sched_state(TaskRunState::Runnable);
                        self.run_queue.enqueue(task.id(), task.sched_policy());
                    }
                }

                // AGENT: deliver pending signals at simulator scheduling/syscall boundaries.
                pub fn deliver_pending_signals(&self, cpu: usize) -> usize {
                    if cpu != 0 {
                        return 0;
                    }
                    let task = match self.cur_task(cpu) {
                        Some(task) => task,
                        None => return 0,
                    };
                    let mut delivered = 0usize;
                    while let Some(sig) = task.take_deliverable_signal() {
                        delivered += 1;
                        match sig.action.handler {
                            SIG_IGN => continue,
                            SIG_DFL => match sig.signo {
                                SIGCHLD => continue,
                                SIGSTOP => {
                                    task.set_sched_state(TaskRunState::Sleeping);
                                    self.run_queue.remove(task.id());
                                    self.run_queue.clear_current();
                                    self.set_cur(cpu, None);
                                    self.schedule_next_runnable(cpu);
                                    break;
                                }
                                _ => {
                                    self.exit_task(cpu, &task, ExitReason::Signal(sig.signo as u8));
                                    break;
                                }
                            },
                            handler => {
                                let old_mask = *task.sig_mask.lock().unwrap();
                                let mut thd = task.thd_ctx.lock().unwrap();
                                let Some(ctx) = thd.as_mut() else {
                                    task.process
                                        .sig_queue
                                        .lock()
                                        .unwrap()
                                        .push_front((sig.signo as i32, sig.sender_tid));
                                    break;
                                };
                                let saved_ctx = ctx.uctx.clone();
                                ctx.sig_frames.push(SigFrame {
                                    saved_ctx,
                                    saved_mask: old_mask,
                                    signo: sig.signo,
                                    sender_tid: sig.sender_tid,
                                });
                                let next_mask = (old_mask | sig.action.mask | (1u64 << sig.signo))
                                    & !((1u64 << SIGKILL) | (1u64 << SIGSTOP));
                                *task.sig_mask.lock().unwrap() = next_mask;
                                ctx.smask = next_mask;
                                ctx.uctx.r[0] = sig.signo as u64;
                                ctx.uctx.r[1] = sig.sender_tid as u64;
                                ctx.uctx.r[2] = ctx.sig_frames.last().unwrap().saved_ctx.ip;
                                ctx.uctx.set_ip(handler as u64);
                                break;
                            }
                        }
                    }
                    delivered
                }

                // AGENT: advance global timers after CPU0 has advanced the logical clock.
                pub(crate) fn advance_timers(&self) {
                    let fired = {
                        let mut timers = self.timers.lock().unwrap();
                        timers.advance()
                    };

                    for timer in fired {
                        self.dispatch_timer(timer);
                    }
                }

                // AGENT: dispatch typed timer expiry targets into the existing wake/signal
                // paths after the timer wheel lock has been released.
                fn dispatch_timer(&self, timer: TimerEntry) {
                    match timer.target {
                        TimerTarget::Noop => {}
                        TimerTarget::WakeToken { token } => {
                            token.wake_timeout();
                        }
                        TimerTarget::WakeTask { task_id } => {
                            let Some(task) = self.tasks.find(task_id) else {
                                return;
                            };
                            if task.done() {
                                return;
                            }
                            if task.sched_state() == TaskRunState::Sleeping {
                                task.set_sched_state(TaskRunState::Runnable);
                                self.run_queue.enqueue(task.id(), task.sched_policy());
                            }
                        }
                        TimerTarget::SignalTask {
                            task_id,
                            signo,
                            sender_tid,
                        } => {
                            if let Some(task) = self.tasks.find(task_id) {
                                self.send_signal_to_task(&task, signo, sender_tid);
                            }
                        }
                    }
                }

                // AGENT: CPU0 owns logical timer progression; other CPUs only update CLK_ALL.
                pub fn schedule_tick(&self, cpu: usize) {
                    dtk(cpu);
                    if cpu == 0 {
                        self.advance_timers();
                    }
                    if cpu != 0 || !self.run_queue.preemptible() {
                        return;
                    }
                    match self.cur_task(cpu) {
                        Some(t) if t.done() => {
                            t.set_sched_state(TaskRunState::Zombie);
                            self.run_queue.remove(t.id());
                            self.schedule_next_runnable(cpu);
                        }
                        Some(t) => {
                            t.set_sched_state(TaskRunState::Running);
                            if t.tick_slice() {
                                if self.run_queue.len() > 0 {
                                    t.set_sched_state(TaskRunState::Runnable);
                                    self.run_queue.enqueue(t.id(), t.sched_policy());
                                    self.schedule_next_runnable(cpu);
                                } else {
                                    t.reset_slice();
                                }
                            }
                        }
                        None => {
                            self.schedule_next_runnable(cpu);
                        }
                    }
                }

                pub(crate) fn schedule_next_runnable(&self, cpu: usize) -> bool {
                    if cpu != 0 {
                        return false;
                    }
                    while let Some((id, _policy)) = self.run_queue.dequeue() {
                        match self.tasks.find(id) {
                            Some(task)
                                if !task.done() && task.sched_state() == TaskRunState::Runnable =>
                            {
                                task.set_sched_state(TaskRunState::Running);
                                task.reset_slice();
                                self.set_cur(cpu, Some(task));
                                self.run_queue.set_current(id);
                                self.deliver_pending_signals(cpu);
                                return true;
                            }
                            Some(task) if task.done() => {
                                task.set_sched_state(TaskRunState::Zombie);
                            }
                            _ => {}
                        }
                    }
                    self.set_cur(cpu, None);
                    self.run_queue.clear_current();
                    false
                }

                pub fn balance_load(&self) -> usize {
                    let cpus = self.cpus.lock().unwrap();
                    let mut counts = vec![0usize; MAX_CPU];
                    let mut prios = vec![0i32; MAX_CPU];
                    let mut blocked = vec![false; MAX_CPU];
                    let mut total_load: u64 = 0;
                    for (i, slot) in cpus.iter().enumerate() {
                        if let Some(ref t) = slot {
                            counts[i] = t.n_children() + 1;
                            prios[i] = *t.process.pgid.lock().unwrap();
                            blocked[i] = t.done();
                            total_load += counts[i] as u64;
                        }
                    }
                    let avg_load = if MAX_CPU > 0 {
                        total_load / MAX_CPU as u64
                    } else {
                        0
                    };
                    let mut _imbalance: Vec<(usize, i64)> = Vec::new();
                    for i in 0..MAX_CPU {
                        let delta = counts[i] as i64 - avg_load as i64;
                        if delta.abs() > 1 {
                            _imbalance.push((i, delta));
                        }
                    }
                    _imbalance.sort_by(|a, b| b.1.cmp(&a.1));
                    compute_load_balance(&counts, &prios, &blocked)
                }
            }
        }
        mod tty {
            use super::*;

            impl RuntimeKernel {
                // AGENT: normalize terminal input and append it to the simulator TTY buffer.
                pub fn tty_push(&self, c: u8) {
                    let byte = if c == b'\r' { b'\n' } else { c };
                    let mut buf = self.tty_buf.lock().unwrap();
                    if buf.len() < 4096 {
                        buf.push_back(byte);
                    }
                }

                // AGENT: consume one byte from the simulator TTY buffer.
                pub fn tty_pop(&self) -> Option<u8> {
                    let mut buf = self.tty_buf.lock().unwrap();
                    buf.pop_front()
                }
            }
        }

        // AGENT: expose the optional runtime ticker guard without making the runtime
        // helper module public.
        pub use self::runtime::KernelRuntimeTicker;
    }
    pub mod net {
        // AGENT
        use super::*;
        use std::ops::Range;

        // AGENT TODO: These protocol helpers are not wired into the simulator yet.
        // Future AF_INET socket support should call them from a Socket/FLike data path
        // for IPv4 header validation and TCP/UDP checksum handling.
        // AGENT TODO: Harden the helpers themselves before treating this as a reliable
        // protocol utility layer: return diagnostic IPv4 parse errors, use wider
        // checksum accumulation plus verify helpers, make TCP checksum APIs operate on
        // TCP segments explicitly, return a fixed 12-byte pseudo header, and cover more
        // edge cases with unit tests.
        pub fn tcp_checksum(src_ip: u32, dst_ip: u32, payload: &[u8]) -> u16 {
            let mut sum: u32 = 0;
            sum += (src_ip >> 16) & 0xFFFF;
            sum += src_ip & 0xFFFF;
            sum += (dst_ip >> 16) & 0xFFFF;
            sum += dst_ip & 0xFFFF;
            sum += 6u32;
            sum += payload.len() as u32;
            let mut i = 0;
            while i + 1 < payload.len() {
                sum += ((payload[i] as u32) << 8) | (payload[i + 1] as u32);
                i += 2;
            }
            if i < payload.len() {
                sum += (payload[i] as u32) << 8;
            }
            while sum > 0xFFFF {
                sum = (sum & 0xFFFF) + (sum >> 16);
            }
            !sum as u16
        }

        // AGENT: structured IPv4 parse result used by future socket receive paths.
        #[derive(Clone, Debug, Eq, PartialEq)]
        pub struct Ipv4HeaderInfo {
            pub src_ip: u32,
            pub dst_ip: u32,
            pub protocol: u8,
            pub ttl: u8,
            pub header_len: usize,
            pub total_len: usize,
            pub payload: Range<usize>,
            pub fragment: Ipv4FragmentInfo,
        }

        // AGENT: decoded IPv4 flags plus the 13-bit fragment offset field.
        #[derive(Clone, Copy, Debug, Eq, PartialEq)]
        pub struct Ipv4FragmentInfo {
            pub raw: u16,
            pub reserved: bool,
            pub dont_fragment: bool,
            pub more_fragments: bool,
            pub fragment_offset: u16,
        }

        // AGENT: parse IPv4 headers with explicit total-length and payload bounds.
        pub fn parse_ipv4_header(pkt: &[u8]) -> Option<Ipv4HeaderInfo> {
            if pkt.len() < 20 {
                return None;
            }
            let version = pkt[0] >> 4;
            if version != 4 {
                return None;
            }
            let ihl = (pkt[0] & 0x0F) as usize;
            let header_len = ihl.checked_mul(4)?;
            if ihl < 5 || pkt.len() < header_len {
                return None;
            }
            let total_len = u16::from_be_bytes([pkt[2], pkt[3]]) as usize;
            if total_len < header_len || total_len > pkt.len() {
                return None;
            }
            let payload = header_len..total_len;
            pkt.get(payload.clone())?;
            let flags_fragment = u16::from_be_bytes([pkt[6], pkt[7]]);
            let ttl = pkt[8];
            let protocol = pkt[9];
            let src_ip = ((pkt[12] as u32) << 24)
                | ((pkt[13] as u32) << 16)
                | ((pkt[14] as u32) << 8)
                | pkt[15] as u32;
            let dst_ip = ((pkt[16] as u32) << 24)
                | ((pkt[17] as u32) << 16)
                | ((pkt[18] as u32) << 8)
                | pkt[19] as u32;
            let mut hdr_checksum: u32 = 0;
            for j in 0..(header_len / 2) {
                // AGENT: IHL in 32-bit words, checksum in 16-bit words
                let offset = j * 2;
                hdr_checksum += ((pkt[offset] as u32) << 8) | pkt[offset + 1] as u32;
            }
            while hdr_checksum > 0xFFFF {
                hdr_checksum = (hdr_checksum & 0xFFFF) + (hdr_checksum >> 16);
            }
            // AGENT: validate header checksum (must fold to 0xFFFF for a valid header)
            if hdr_checksum != 0xFFFF {
                return None;
            }
            Some(Ipv4HeaderInfo {
                src_ip,
                dst_ip,
                protocol,
                ttl,
                header_len,
                total_len,
                payload,
                fragment: Ipv4FragmentInfo {
                    raw: flags_fragment,
                    reserved: (flags_fragment & 0x8000) != 0,
                    dont_fragment: (flags_fragment & 0x4000) != 0,
                    more_fragments: (flags_fragment & 0x2000) != 0,
                    fragment_offset: flags_fragment & 0x1FFF,
                },
            })
        }

        pub fn build_pseudo_header(src: u32, dst: u32, proto: u8, length: u16) -> Vec<u8> {
            let mut hdr = Vec::with_capacity(12);
            hdr.push((src >> 24) as u8);
            hdr.push((src >> 16) as u8);
            hdr.push((src >> 8) as u8);
            hdr.push(src as u8);
            hdr.push((dst >> 24) as u8);
            hdr.push((dst >> 16) as u8);
            hdr.push((dst >> 8) as u8);
            hdr.push(dst as u8);
            hdr.push(0);
            hdr.push(proto);
            hdr.push((length >> 8) as u8);
            hdr.push(length as u8);
            hdr
        }

        pub fn compute_inet_checksum(data: &[u8]) -> u16 {
            let mut sum: u32 = 0;
            let mut i = 0;
            while i + 1 < data.len() {
                sum += ((data[i] as u32) << 8) | data[i + 1] as u32;
                i += 2;
            }
            if i < data.len() {
                sum += (data[i] as u32) << 8;
            }
            while sum > 0xFFFF {
                sum = (sum & 0xFFFF) + (sum >> 16);
            }
            !sum as u16
        }
    }
    pub mod prelude {
        // AGENT
        pub(crate) use std::any::Any;
        pub(crate) use std::cmp::{max, min, Ordering as CmpOrd};
        pub(crate) use std::collections::{BTreeMap, BTreeSet, HashMap, LinkedList, VecDeque};
        pub(crate) use std::fmt;
        pub(crate) use std::ops::{Deref, DerefMut, Index};
        pub(crate) use std::sync::atomic::{
            AtomicBool, AtomicU32, AtomicU64, AtomicU8, AtomicUsize, Ordering,
        };
        pub(crate) use std::sync::{Arc, Condvar, Mutex, RwLock, Weak};
        pub(crate) use std::thread;
        pub(crate) use std::time::Duration;

        // AGENT: simulated realtime starts at this Unix epoch second.
        pub const BOOT_EPOCH: usize = 0;
        pub const PAGE_SZ: usize = 4096;
        pub const N_PROC: usize = 256;
        pub const MAX_FD: usize = 256; // AGENT
        pub const N_FRAMES: usize = 65536;
        pub const KERN_BASE: usize = 0xFFFF_FFFF_8000_0000;
        pub const PHYS_OFF: usize = 0xFFFF_FFFF_0000_0000;
        pub const MEM_OFF: usize = 0x8000_0000;
        pub const KHEAP_SZ: usize = 0x800000;
        pub const N_CHAINS: usize = 64;
        pub const RBUF_CAP: usize = 256;
        pub const N_REGS: usize = 16;
        pub const MNT_DEPTH: usize = 8;
        pub const MAX_CPU: usize = 8;
        pub const KSTK_SZ: usize = 0x4000;
        pub const USR_STK_OFF: usize = 0x7FFF_0000;
        pub const USR_STK_SZ: usize = 0x10000;
        pub const USEC_TICK: usize = 10_000; // AGENT: 100Hz logical clock, one tick is 10ms.
        pub const FOLLOW_LIM: usize = 3;

        pub const F_DUPFD: usize = 0;
        pub const F_GETFD: usize = 1;
        pub const F_SETFD: usize = 2;
        pub const F_GETFL: usize = 3;
        pub const F_SETFL: usize = 4;
        pub const F_GETLK: usize = 5;
        pub const F_SETLK: usize = 6;
        pub const F_SETLKW: usize = 7;
        pub const FD_CLOEXEC: usize = 1;
        pub const F_DUPFD_CLOEXEC: usize = 1030;
        pub const O_CREAT: usize = 0o100;
        pub const O_EXCL: usize = 0o200;
        pub const O_TRUNC: usize = 0o1000;
        pub const O_NONBLOCK: usize = 0o4000;
        pub const O_APPEND: usize = 0o2000;
        pub const O_CLOEXEC: usize = 0o2000000;
        pub const AT_NOFOLLOW: usize = 0x100;

        pub const TCGETS: usize = 0x5401;
        pub const TCSETS: usize = 0x5402;
        pub const TIOCGPGRP: usize = 0x540F;
        pub const TIOCSPGRP: usize = 0x5410;
        pub const TIOCGWINSZ: usize = 0x5413;
        pub const FIONCLEX: usize = 0x5450;
        pub const FIOCLEX: usize = 0x5451;
        pub const FIONBIO: usize = 0x5421;

        pub const AT_PHDR: u8 = 3;
        pub const AT_PHENT: u8 = 4;
        pub const AT_PHNUM: u8 = 5;
        pub const AT_PAGESZ: u8 = 6;
        pub const AT_BASE: u8 = 7;
        pub const AT_ENTRY: u8 = 9;

        pub const LM_ISIG: u32 = 0o000001;
        pub const LM_ICANON: u32 = 0o000002;
        pub const LM_ECHO: u32 = 0o000010;
        pub const LM_ECHOE: u32 = 0o000020;
        pub const LM_ECHOK: u32 = 0o000040;
        pub const LM_ECHONL: u32 = 0o000100;
        pub const LM_NOFLSH: u32 = 0o000200;
        pub const LM_TOSTOP: u32 = 0o000400;
        pub const LM_IEXTEN: u32 = 0o100000;
        pub const LM_XCASE: u32 = 0o000004;
        pub const LM_ECHOCTL: u32 = 0o001000;
        pub const LM_ECHOPRT: u32 = 0o002000;
        pub const LM_ECHOKE: u32 = 0o004000;
        pub const LM_FLUSHO: u32 = 0o010000;
        pub const LM_PENDIN: u32 = 0o040000;
        pub const LM_EXTPROC: u32 = 0o200000;

        pub const VM_READ: u32 = 0x01;
        pub const VM_WRITE: u32 = 0x02;
        pub const VM_EXEC: u32 = 0x04;
        pub const VM_SHARED: u32 = 0x08;
        pub const VM_GROWSDOWN: u32 = 0x10;
        pub const VM_DONTCOPY: u32 = 0x20;
        pub const VM_HUGETLB: u32 = 0x40;
        pub const VM_PFNMAP: u32 = 0x80;

        // AGENT: mmap/prot constants used by syscall validation and tests.
        pub const PROT_READ: usize = 0x1;
        pub const PROT_WRITE: usize = 0x2;
        pub const PROT_EXEC: usize = 0x4;
        pub const MAP_SHARED: usize = 0x01;
        pub const MAP_PRIVATE: usize = 0x02;
        pub const MAP_FIXED: usize = 0x10;
        pub const MAP_ANONYMOUS: usize = 0x20;
        pub const MAP_ANON: usize = MAP_ANONYMOUS;

        pub const CAP_CHOWN: u32 = 0;
        pub const CAP_KILL: u32 = 5;
        pub const CAP_SETUID: u32 = 7;
        pub const CAP_SETGID: u32 = 6;
        pub const CAP_NET_BIND: u32 = 10;
        pub const CAP_NET_RAW: u32 = 13;
        pub const CAP_SYS_ADMIN: u32 = 21;
        pub const CAP_SYS_PTRACE: u32 = 19;
        pub const INHERITABLE_MASK: u64 = 0x0000_00FF_FFFF_FFFF;

        pub const ZONE_DMA: usize = 0;
        pub const ZONE_NORMAL: usize = 1;
        pub const ZONE_HIGH: usize = 2;
        pub const N_ZONES: usize = 3;

        pub const PRIO_MIN: i32 = -20;
        pub const PRIO_MAX: i32 = 19;
        pub const PRIO_DEFAULT: i32 = 0;
        pub const SCHED_NORMAL: u8 = 0;
        pub const SCHED_FIFO: u8 = 1;
        pub const SCHED_RR: u8 = 2;
        pub const SCHED_BATCH: u8 = 3;

        pub const SLAB_OBJ_MIN: usize = 8;
        pub const SLAB_OBJ_MAX: usize = 2048;
        pub const SLAB_ALIGN: usize = 8;

        pub const NSIG: u32 = 64;
        pub const SIG_DFL: usize = 0;
        pub const SIG_IGN: usize = 1;
        pub const SIGKILL: u32 = 9;
        pub const SIGSTOP: u32 = 19;
        pub const SIGCHLD: u32 = 17;
        pub const SIGUSR1: u32 = 10;
        pub const SIGUSR2: u32 = 12;
        pub const SIGALRM: u32 = 14;

        pub const TIMER_WHEEL_SIZE: usize = 256;
        pub const TIMER_TICK_HZ: usize = 100;

        pub const SOCK_STREAM: u32 = 1;
        pub const SOCK_DGRAM: u32 = 2;
        pub const SOCK_RAW: u32 = 3;
        pub const AF_INET: u32 = 2;
        pub const AF_INET6: u32 = 10;
        pub const AF_UNIX: u32 = 1;

        pub const SYS_READ: usize = 0;
        pub const SYS_WRITE: usize = 1;
        pub const SYS_OPEN: usize = 2;
        pub const SYS_CLOSE: usize = 3;
        pub const SYS_STAT: usize = 4;
        pub const SYS_FSTAT: usize = 5;
        pub const SYS_MMAP: usize = 9;
        pub const SYS_MUNMAP: usize = 11;
        pub const SYS_BRK: usize = 12;
        pub const SYS_SIGRETURN: usize = 15;
        pub const SYS_IOCTL: usize = 16;
        pub const SYS_PIPE: usize = 22;
        pub const SYS_DUP: usize = 32;
        pub const SYS_DUP2: usize = 33;
        pub const SYS_FORK: usize = 57;
        pub const SYS_EXEC: usize = 59;
        pub const SYS_EXIT: usize = 60;
        pub const SYS_WAIT4: usize = 61;
        pub const SYS_KILL: usize = 62;
        pub const SYS_FCNTL: usize = 72;
        pub const SYS_GETPID: usize = 39;
        pub const SYS_GETPPID: usize = 110;
        pub const SYS_SETPGID: usize = 109;
        pub const SYS_GETPGID: usize = 121;
        pub const SYS_SETSID: usize = 112;
        pub const SYS_EPOLL_CREATE: usize = 213;
        pub const SYS_EPOLL_CTL: usize = 233;
        pub const SYS_EPOLL_WAIT: usize = 232;
        pub const SYS_CLOCK_GETTIME: usize = 228;
        pub const SYS_SIGACTION: usize = 13;
        pub const SYS_SIGPROCMASK: usize = 14;
        pub const SYS_FUTEX: usize = 202;

        pub const IOQUEUE_DEPTH: usize = 128;

        pub const MAX_THREAD_ID: usize = N_PROC - 1; // AGENT
    }
    pub mod sync {
        // AGENT
        use super::*;

        // AGENT: Usage map for this module in the current kernel-sim code.
        //
        // Active paths:
        // - GKL/KernLock backs RuntimeKernel::tick() and BlockCache::sync_all(); the public
        //   leave() keeps the legacy chaos-tests API, while guards still use
        //   owner-checked release internally.
        // - Spin backs cache-chain locking and Channel through SpinGuard so release is
        //   panic-safe and callers cannot touch the atomic state directly; ownership is
        //   keyed by host-thread tokens so legacy chaos-tests can exercise it outside
        //   scheduler-installed current-task context.
        // - EvBus/EvFlag is used as event-bit storage by pipe, process exit/signal,
        //   semaphore state transitions, and pipe-backed epoll readiness notification.
        // - WaitToken is the common host-thread wait token used by Channel,
        //   proc::WaitQueue, SyncQueue helpers, and FutexBucket.
        // - SyncQueue is used by Channel through new(), signal(), broadcast(), and
        //   direct access to q.
        // - FutexBucket is wired to SYS_FUTEX and process-exit cleanup.
        //
        // Partially wired paths:
        // - Sema is created through SemArr/SemCtx and uses remove()/release(), but
        //   semget/semop/semctl-style syscall dispatch is not present.
        //
        // Unused or reserved paths:
        // - KernLock::enter/try_enter/held/owner/level are available for focused tests
        //   or future paths that cannot use the guard API; Spin::try_acquire/is_held
        //   and SpinLock<T> are available for short non-blocking critical sections.
        // - EvFlag::WRITABLE/ERROR.
        // - top-level wait_ev() is available for EvBus readiness waits, but has no
        //   active syscall path yet.
        // - RegEp and SyncQueue's generic wait/timeout/epoll-registration helpers.
        // - WaitToken::id() and SocketState.
        // AGENT TODO: KernLock is still a simulator recursive spin lock, not full
        // real-kernel locking: it lacks fairness, blocking wait, preemption control,
        // and interrupt masking semantics.
        // AGENT: fields are private so callers must use the legacy enter/leave facade
        // or the owner-checked guard APIs.
        pub struct KernLock {
            flag: AtomicBool,
            holder: AtomicUsize,
            depth: AtomicUsize,
        }

        // AGENT: no-owner sentinel is independent from task/thread id limits.
        const KERNLOCK_NO_OWNER: usize = usize::MAX;

        impl KernLock {
            // AGENT: initialize holder with the owner-token sentinel, not MAX_THREAD_ID.
            pub const fn new() -> Self {
                Self {
                    flag: AtomicBool::new(false),
                    holder: AtomicUsize::new(KERNLOCK_NO_OWNER), // AGENT
                    depth: AtomicUsize::new(0),
                }
            }
            // AGENT: KernLock owner ids are lock-owner tokens, not RuntimeTaskTable indexes.
            pub fn enter(&self, id: usize) {
                assert_ne!(id, KERNLOCK_NO_OWNER, "KernLock owner id is reserved");
                if self.holder.load(Ordering::Relaxed) == id {
                    self.depth.fetch_add(1, Ordering::Relaxed);
                    return;
                }
                while self
                    .flag
                    .compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed)
                    .is_err()
                {
                    ::core::hint::spin_loop();
                }
                self.holder.store(id, Ordering::Relaxed);
                self.depth.store(1, Ordering::Relaxed);
            }
            // AGENT: legacy chaos-tests release API; it intentionally does not require
            // an owner token, matching the old single-file kernel surface.
            pub fn leave(&self) {
                let depth = self.depth.load(Ordering::Relaxed);
                if depth > 1 {
                    self.depth.store(depth - 1, Ordering::Relaxed);
                } else {
                    self.holder.store(KERNLOCK_NO_OWNER, Ordering::Relaxed);
                    self.depth.store(0, Ordering::Relaxed);
                    self.flag.store(false, Ordering::Release);
                }
            }
            // AGENT: guard-only release keeps internal callers protected against
            // dropping another owner's GKL depth.
            pub fn leave_checked(&self, id: usize) {
                assert_ne!(id, KERNLOCK_NO_OWNER, "KernLock owner id is reserved");
                let owner = self.holder.load(Ordering::Relaxed);
                let depth = self.depth.load(Ordering::Relaxed);
                assert!(
                    self.flag.load(Ordering::Relaxed) && depth > 0,
                    "KernLock::leave by owner {} without held lock",
                    id
                );
                assert_eq!(
                    owner, id,
                    "KernLock::leave by non-owner {}, owner is {}",
                    id, owner
                );
                if depth > 1 {
                    self.depth.store(depth - 1, Ordering::Relaxed);
                } else {
                    self.holder.store(KERNLOCK_NO_OWNER, Ordering::Relaxed); // AGENT
                    self.depth.store(0, Ordering::Relaxed);
                    self.flag.store(false, Ordering::Release);
                }
            }
            pub fn held(&self) -> bool {
                self.flag.load(Ordering::Relaxed)
            }
            pub fn owner(&self) -> usize {
                self.holder.load(Ordering::Relaxed)
            }
            pub fn level(&self) -> usize {
                self.depth.load(Ordering::Relaxed)
            }
            // AGENT: try_enter follows the same owner-token rule as enter().
            pub fn try_enter(&self, id: usize) -> bool {
                assert_ne!(id, KERNLOCK_NO_OWNER, "KernLock owner id is reserved");
                if self.holder.load(Ordering::Relaxed) == id {
                    self.depth.fetch_add(1, Ordering::Relaxed);
                    return true;
                }
                if self
                    .flag
                    .compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed)
                    .is_ok()
                {
                    self.holder.store(id, Ordering::Relaxed);
                    self.depth.store(1, Ordering::Relaxed);
                    true
                } else {
                    false
                }
            }
            // AGENT: preferred GKL entry path; Drop pairs the owner-checked release.
            pub fn guard(&self, id: usize) -> KernLockGuard<'_> {
                self.enter(id);
                KernLockGuard { lock: self, id }
            }
            // AGENT: non-blocking guard constructor for future paths that cannot spin.
            pub fn try_guard(&self, id: usize) -> Option<KernLockGuard<'_>> {
                if self.try_enter(id) {
                    Some(KernLockGuard { lock: self, id })
                } else {
                    None
                }
            }
        }
        unsafe impl Send for KernLock {}
        unsafe impl Sync for KernLock {}
        pub static GKL: KernLock = KernLock::new();

        // AGENT: RAII token for GKL-style locking; releasing goes through leave(id).
        #[must_use = "KernLockGuard releases the lock when dropped"]
        pub struct KernLockGuard<'a> {
            lock: &'a KernLock,
            id: usize,
        }

        // AGENT: make guard drop the only release step needed by normal callers.
        impl Drop for KernLockGuard<'_> {
            fn drop(&mut self) {
                self.lock.leave_checked(self.id);
            }
        }

        const SPIN_NO_OWNER: usize = 0;
        static NEXT_SPIN_OWNER: AtomicUsize = AtomicUsize::new(1);

        std::thread_local! {
            static SPIN_OWNER: usize = allocate_spin_owner();
        }

        // AGENT: allocate nonzero host-thread owner tokens without relying on unstable
        // ThreadId integer conversion.
        fn allocate_spin_owner() -> usize {
            let owner = NEXT_SPIN_OWNER.fetch_add(1, Ordering::Relaxed);
            assert_ne!(owner, SPIN_NO_OWNER, "Spin owner token space exhausted");
            owner
        }

        // AGENT: Spin derives ownership from the host thread again so low-level
        // chaos-tests can use it without first installing a simulator RuntimeTask::id().
        fn spin_owner() -> usize {
            SPIN_OWNER.with(|owner| *owner)
        }

        // AGENT: ticket-based simulator spinlock with private state, FIFO acquisition,
        // RAII guard support, and host-thread owner checks. It still models only short
        // non-blocking critical sections; it does not mask interrupts or preemption.
        pub struct Spin {
            next_ticket: AtomicUsize,
            serving: AtomicUsize,
            owner: AtomicUsize,
        }
        impl Spin {
            pub const fn new() -> Self {
                Self {
                    next_ticket: AtomicUsize::new(0),
                    serving: AtomicUsize::new(0),
                    owner: AtomicUsize::new(SPIN_NO_OWNER),
                }
            }
            // AGENT: FIFO acquire now owns current-task lookup and ticket acquisition
            // directly instead of delegating through an owner-parameter helper.
            pub fn acquire(&self) {
                let owner = spin_owner();
                assert_ne!(
                    self.owner.load(Ordering::Relaxed),
                    owner,
                    "Spin::acquire attempted recursive locking by host owner {}",
                    owner
                );
                let ticket = self.next_ticket.fetch_add(1, Ordering::Relaxed);
                while self.serving.load(Ordering::Acquire) != ticket {
                    ::core::hint::spin_loop();
                }
                self.owner.store(owner, Ordering::Relaxed);
            }
            // AGENT: non-blocking acquire performs owner lookup inline and only
            // succeeds when no owner or queued waiter is ahead, preserving ticket-lock
            // fairness for blocking acquirers.
            pub fn try_acquire(&self) -> bool {
                let owner = spin_owner();
                assert_ne!(
                    self.owner.load(Ordering::Relaxed),
                    owner,
                    "Spin::try_acquire attempted recursive locking by host owner {}",
                    owner
                );
                let serving = self.serving.load(Ordering::Acquire);
                let next = self.next_ticket.load(Ordering::Relaxed);
                if serving != next {
                    return false;
                }
                if self
                    .next_ticket
                    .compare_exchange(
                        next,
                        next.wrapping_add(1),
                        Ordering::Acquire,
                        Ordering::Relaxed,
                    )
                    .is_err()
                {
                    return false;
                }
                self.owner.store(owner, Ordering::Relaxed);
                true
            }
            // AGENT: release verifies the current host thread owns this Spin without
            // delegating through a private owner-parameter wrapper.
            pub fn release(&self) {
                let owner = spin_owner();
                let current_owner = self.owner.load(Ordering::Relaxed);
                assert!(
                    current_owner != SPIN_NO_OWNER,
                    "Spin::release by host owner {} without held lock",
                    owner
                );
                assert_eq!(
                    current_owner, owner,
                    "Spin::release by non-owner host owner {}, owner is {}",
                    owner, current_owner
                );
                self.owner.store(SPIN_NO_OWNER, Ordering::Relaxed);
                self.serving.fetch_add(1, Ordering::Release);
            }
            pub fn is_held(&self) -> bool {
                self.serving.load(Ordering::Acquire) != self.next_ticket.load(Ordering::Relaxed)
            }
            pub fn level(&self) -> usize {
                usize::from(self.owner.load(Ordering::Relaxed) != SPIN_NO_OWNER)
            }
            // AGENT: guard reuses acquire() and records the owner written by acquire()
            // so Drop can release without requiring a still-current task context.
            pub fn guard(&self) -> SpinGuard<'_> {
                self.acquire();
                let owner = self.owner.load(Ordering::Relaxed);
                SpinGuard {
                    lock: self,
                    owner,
                    _not_send: std::marker::PhantomData,
                }
            }
            // AGENT: try_guard reuses try_acquire() and captures the stored owner only
            // after the non-blocking acquisition succeeds.
            pub fn try_guard(&self) -> Option<SpinGuard<'_>> {
                if self.try_acquire() {
                    let owner = self.owner.load(Ordering::Relaxed);
                    Some(SpinGuard {
                        lock: self,
                        owner,
                        _not_send: std::marker::PhantomData,
                    })
                } else {
                    None
                }
            }
        }
        unsafe impl Send for Spin {}
        unsafe impl Sync for Spin {}

        // AGENT: RAII token for Spin; normal callers should prefer Spin::guard().
        #[must_use = "SpinGuard releases the lock when dropped"]
        pub struct SpinGuard<'a> {
            lock: &'a Spin,
            owner: usize,
            _not_send: std::marker::PhantomData<std::rc::Rc<()>>,
        }

        // AGENT: drop-based release keeps early returns from leaking the spinlock and
        // uses the guard's recorded owner instead of the current-task helper.
        impl Drop for SpinGuard<'_> {
            fn drop(&mut self) {
                let current_owner = self.lock.owner.load(Ordering::Relaxed);
                assert!(
                    current_owner != SPIN_NO_OWNER,
                    "SpinGuard::drop by task {} without held lock",
                    self.owner
                );
                assert_eq!(
                    current_owner, self.owner,
                    "SpinGuard::drop by non-owner task {}, owner is {}",
                    self.owner, current_owner
                );
                self.lock.owner.store(SPIN_NO_OWNER, Ordering::Relaxed);
                self.lock.serving.fetch_add(1, Ordering::Release);
            }
        }

        // AGENT: optional typed spinlock for future short critical sections that need
        // data tied to a SpinGuard instead of a separate lock plus convention.
        pub struct SpinLock<T> {
            lock: Spin,
            data: std::cell::UnsafeCell<T>,
        }

        impl<T> SpinLock<T> {
            pub const fn new(data: T) -> Self {
                Self {
                    lock: Spin::new(),
                    data: std::cell::UnsafeCell::new(data),
                }
            }
            pub fn lock(&self) -> SpinLockGuard<'_, T> {
                let guard = self.lock.guard();
                SpinLockGuard {
                    _guard: guard,
                    data: self.data.get(),
                }
            }
            pub fn try_lock(&self) -> Option<SpinLockGuard<'_, T>> {
                self.lock.try_guard().map(|guard| SpinLockGuard {
                    _guard: guard,
                    data: self.data.get(),
                })
            }
            pub fn is_locked(&self) -> bool {
                self.lock.is_held()
            }
        }

        unsafe impl<T: Send> Send for SpinLock<T> {}
        unsafe impl<T: Send> Sync for SpinLock<T> {}

        // AGENT: typed guard couples protected data access to SpinGuard lifetime.
        pub struct SpinLockGuard<'a, T> {
            _guard: SpinGuard<'a>,
            data: *mut T,
        }

        impl<T> Deref for SpinLockGuard<'_, T> {
            type Target = T;
            fn deref(&self) -> &Self::Target {
                unsafe { &*self.data }
            }
        }

        impl<T> DerefMut for SpinLockGuard<'_, T> {
            fn deref_mut(&mut self) -> &mut Self::Target {
                unsafe { &mut *self.data }
            }
        }

        // pub struct FlgGuard(usize);
        // impl FlgGuard { pub fn enter() -> Self { Self(0) } }
        // impl Drop for FlgGuard { fn drop(&mut self) {} }

        pub struct EvFlag;
        impl EvFlag {
            pub const READABLE: u32 = 1 << 0;
            pub const WRITABLE: u32 = 1 << 1;
            pub const ERROR: u32 = 1 << 2;
            pub const CLOSED: u32 = 1 << 3;
            pub const PROC_QUIT: u32 = 1 << 10;
            pub const CHILD_QUIT: u32 = 1 << 11;
            pub const RECV_SIG: u32 = 1 << 12;
            pub const SEM_RM: u32 = 1 << 20;
            pub const SEM_ACQ: u32 = 1 << 21;
        }

        pub type EvCb = Box<dyn Fn(u32) -> bool + Send>;

        // AGENT: cancellable EvBus subscriptions let epoll_ctl(DEL/MOD) detach a
        // readiness callback without knowing the callback body.
        struct EvSub {
            id: usize,
            cb: EvCb,
        }

        // AGENT: EvBus waiters pair an event mask with the host-thread wait token that
        // should be woken once the bus reaches a matching readiness state.
        struct EvWaiter {
            mask: u32,
            token: WaitToken,
        }

        // AGENT TODO: EvBus is still a lightweight event-bit store, not a full
        // kernel-style wait/readiness mechanism. It lacks event payloads/counting,
        // epoll-ready propagation, and lock-free callback dispatch.
        #[derive(Default)]
        pub struct EvBus {
            pub ev: u32,
            cbs: Vec<EvSub>,
            waiters: VecDeque<EvWaiter>,
            next_sub_id: usize,
        }
        impl EvBus {
            pub fn make() -> Arc<Mutex<Self>> {
                Arc::new(Mutex::new(Self::default()))
            }
            pub fn set(&mut self, s: u32) {
                self.change(0, s);
            }
            pub fn clear(&mut self, s: u32) {
                self.change(s, 0);
            }
            // AGENT: event changes wake every queued waiter whose mask is now ready.
            pub fn change(&mut self, rst: u32, s: u32) {
                let orig = self.ev;
                self.ev = (self.ev & !rst) | s;
                if self.ev != orig {
                    let ev = self.ev;
                    let mut ready = Vec::new();
                    self.waiters.retain(|waiter| {
                        if (ev & waiter.mask) != 0 {
                            ready.push(waiter.token.clone());
                            false
                        } else {
                            true
                        }
                    });
                    self.cbs.retain(|sub| !(sub.cb)(ev));
                    for token in ready {
                        token.wake();
                    }
                }
            }
            // AGENT: return a subscription id so higher-level readiness users can
            // cancel epoll registrations when epoll_ctl removes or replaces them.
            pub fn sub(&mut self, cb: EvCb) -> usize {
                let id = self.next_sub_id;
                self.next_sub_id = self.next_sub_id.wrapping_add(1);
                self.cbs.push(EvSub { id, cb });
                id
            }
            // AGENT: remove a previously installed callback subscription.
            pub fn unsub(&mut self, id: usize) -> bool {
                let before = self.cbs.len();
                self.cbs.retain(|sub| sub.id != id);
                self.cbs.len() != before
            }
            pub fn cb_len(&self) -> usize {
                self.cbs.len()
            }
        }

        // AGENT: check readiness and enqueue the WaitToken while holding the EvBus lock
        // so a concurrent event change cannot happen between the check and sleep setup.
        pub fn wait_ev(bus: &Arc<Mutex<EvBus>>, mask: u32) -> u32 {
            loop {
                let token = WaitToken::current();
                {
                    let mut g = bus.lock().unwrap();
                    if (g.ev & mask) != 0 {
                        return g.ev;
                    }
                    g.waiters.push_back(EvWaiter {
                        mask,
                        token: token.clone(),
                    });
                }
                token.wait(None);
            }
        }

        // AGENT: keep host-thread parking behind a token so kernel wait queues do not
        // store std::thread::Thread directly.
        static WAIT_TOKEN_SEQ: AtomicUsize = AtomicUsize::new(1);

        #[derive(Clone, Copy, Debug, PartialEq, Eq)]
        pub enum WaitOutcome {
            Event,
            Timeout,
        }

        const WAIT_PENDING: u8 = 0;
        const WAIT_EVENT: u8 = 1;
        const WAIT_TIMEOUT: u8 = 2;

        #[derive(Clone)]
        pub struct WaitToken {
            id: usize,
            state: Arc<WaitState>,
        }

        struct WaitState {
            outcome: AtomicU8,
            host: HostWaiter,
        }

        struct HostWaiter {
            thread: thread::Thread,
        }

        impl HostWaiter {
            fn current() -> Self {
                Self {
                    thread: thread::current(),
                }
            }

            fn park(&self) {
                thread::park();
            }

            fn park_timeout(&self, timeout: Duration) {
                thread::park_timeout(timeout);
            }

            fn wake(&self) {
                self.thread.unpark();
            }
        }

        impl WaitToken {
            pub fn current() -> Self {
                Self {
                    id: WAIT_TOKEN_SEQ.fetch_add(1, Ordering::Relaxed),
                    state: Arc::new(WaitState {
                        outcome: AtomicU8::new(WAIT_PENDING),
                        host: HostWaiter::current(),
                    }),
                }
            }

            pub fn id(&self) -> usize {
                self.id
            }

            pub fn wake(&self) -> bool {
                self.wake_event()
            }

            // AGENT: mark a normal event wake; returns false if timeout or another wake
            // already won the race.
            pub fn wake_event(&self) -> bool {
                if self
                    .state
                    .outcome
                    .compare_exchange(
                        WAIT_PENDING,
                        WAIT_EVENT,
                        Ordering::Release,
                        Ordering::Relaxed,
                    )
                    .is_ok()
                {
                    self.state.host.wake();
                    true
                } else {
                    false
                }
            }

            // AGENT: mark a timer expiry wake separately from a normal event wake.
            pub fn wake_timeout(&self) -> bool {
                if self
                    .state
                    .outcome
                    .compare_exchange(
                        WAIT_PENDING,
                        WAIT_TIMEOUT,
                        Ordering::Release,
                        Ordering::Relaxed,
                    )
                    .is_ok()
                {
                    self.state.host.wake();
                    true
                } else {
                    false
                }
            }

            pub fn wait(&self, timeout: Option<Duration>) -> WaitOutcome {
                match timeout {
                    Some(d) => {
                        let deadline = std::time::Instant::now() + d;
                        while !self.is_woken() {
                            let now = std::time::Instant::now();
                            if now >= deadline {
                                self.wake_timeout();
                                break;
                            }
                            self.state.host.park_timeout(deadline - now);
                        }
                    }
                    None => {
                        while !self.is_woken() {
                            self.state.host.park();
                        }
                    }
                }
                self.outcome()
            }

            // AGENT: wait using the logical kernel timer wheel instead of host
            // Instant/park_timeout.
            pub fn wait_with_timer(&self, timeout: Duration) -> WaitOutcome {
                let ticks = duration_to_ticks(timeout);
                if ticks == 0 {
                    self.wake_timeout();
                    return self.outcome();
                }
                let deadline = CLK.load(Ordering::Relaxed).saturating_add(ticks);
                let timers = global_timer_wheel();
                let timer_id = {
                    let mut wheel = timers.lock().unwrap();
                    wheel.register_timer(
                        deadline,
                        0,
                        TimerTarget::WakeToken {
                            token: self.clone(),
                        },
                    )
                };
                let outcome = self.wait(None);
                if outcome == WaitOutcome::Event {
                    timers.lock().unwrap().cancel(timer_id);
                }
                outcome
            }

            pub fn is_woken(&self) -> bool {
                self.state.outcome.load(Ordering::Acquire) != WAIT_PENDING
            }

            pub fn is_timeout(&self) -> bool {
                self.state.outcome.load(Ordering::Acquire) == WAIT_TIMEOUT
            }

            pub fn outcome(&self) -> WaitOutcome {
                match self.state.outcome.load(Ordering::Acquire) {
                    WAIT_TIMEOUT => WaitOutcome::Timeout,
                    _ => WaitOutcome::Event,
                }
            }

            pub fn same(&self, other: &Self) -> bool {
                Arc::ptr_eq(&self.state, &other.state)
            }
        }

        #[derive(Clone, Copy, PartialEq, Eq)]
        pub enum SocketState {
            Closed,
            Listen,
            SynSent,
            SynRecvd,
            Established,
            FinWait1,
            FinWait2,
            TimeWait,
            CloseWait,
            LastAck,
            Closing,
        }

        pub struct RegEp {
            pub task_id: usize,
            pub epfd: usize,
            pub fd: usize,
        }

        // AGENT TODO: SyncQueue's generic helpers are not yet a full
        // condition-variable/wait-queue abstraction. park_on() preserves unmatched
        // signal wakeups, but the other helpers do not atomically pair condition
        // checks, waiter enqueue, and release/reacquire of the caller's guard;
        // wait_timeout still uses host time; RegEp is not wired into epoll readiness
        // wakeups. Channel currently uses q directly with its own lock ordering.
        pub struct SyncQueue {
            pub(crate) q: Mutex<VecDeque<WaitToken>>,
            pending_wakes: AtomicUsize,
            eq: Mutex<VecDeque<RegEp>>,
        }
        impl SyncQueue {
            pub fn new() -> Self {
                Self {
                    q: Mutex::new(VecDeque::new()),
                    pending_wakes: AtomicUsize::new(0),
                    eq: Mutex::new(VecDeque::new()),
                }
            }
            // AGENT: called with q locked, so the waiter queue and cached signal credits
            // are observed as one logical SyncQueue state.
            fn take_pending_wake_locked(&self) -> bool {
                let pending = self.pending_wakes.load(Ordering::Relaxed);
                if pending == 0 {
                    return false;
                }
                self.pending_wakes.store(pending - 1, Ordering::Relaxed);
                true
            }
            // AGENT: called with q locked to preserve signal-before-wait ordering.
            fn add_pending_wakes_locked(&self, count: usize) {
                if count == 0 {
                    return;
                }
                let pending = self.pending_wakes.load(Ordering::Relaxed);
                self.pending_wakes.store(
                    pending
                        .checked_add(count)
                        .expect("SyncQueue pending wake credit overflow"),
                    Ordering::Relaxed,
                );
            }
            pub fn park_on<T>(&self, g: &Mutex<T>, pred: impl Fn(&T) -> bool) -> bool {
                let d = g.lock().unwrap();
                let satisfied = pred(&d);
                drop(d);
                if satisfied {
                    return true;
                }
                let token = {
                    let mut wq = self.q.lock().unwrap();
                    if self.take_pending_wake_locked() {
                        None
                    } else {
                        let token = WaitToken::current();
                        wq.push_back(token.clone());
                        Some(token)
                    }
                };
                if let Some(token) = token {
                    token.wait(None);
                }
                let d = g.lock().unwrap();
                pred(&d)
            }
            pub fn signal(&self) {
                loop {
                    let token = {
                        let mut q = self.q.lock().unwrap();
                        match q.pop_front() {
                            Some(token) => Some(token),
                            None => {
                                self.add_pending_wakes_locked(1);
                                None
                            }
                        }
                    };
                    match token {
                        Some(token) if token.wake() => return,
                        Some(_) => continue,
                        None => return,
                    }
                }
            }
            pub fn broadcast(&self) {
                let mut q = self.q.lock().unwrap();
                let batch: Vec<WaitToken> = q.drain(..).collect();
                drop(q);
                for token in batch {
                    token.wake();
                }
            }
            // AGENT: wake up to n live tokens and skip stale tokens already completed by timeout.
            pub fn signal_n(&self, n: usize) -> usize {
                let mut woken = 0;
                while woken < n {
                    let token = {
                        let mut q = self.q.lock().unwrap();
                        match q.pop_front() {
                            Some(token) => Some(token),
                            None => {
                                self.add_pending_wakes_locked(n - woken);
                                None
                            }
                        }
                    };
                    match token {
                        Some(token) if token.wake() => woken += 1,
                        Some(_) => continue,
                        None => break,
                    }
                }
                woken
            }
            pub fn pending(&self) -> usize {
                let q = self.q.lock().unwrap();
                q.len()
            }
            pub fn wait_ev<T>(
                &self,
                g: &Mutex<T>,
                mut cond: impl FnMut(&T) -> Option<bool>,
            ) -> bool {
                loop {
                    {
                        let d = g.lock().unwrap();
                        if let Some(r) = cond(&d) {
                            return r;
                        }
                    }
                    let token = WaitToken::current();
                    {
                        let mut q = self.q.lock().unwrap();
                        q.push_back(token.clone());
                    }
                    token.wait(None);
                }
            }
            pub fn wait_events<T>(
                queues: &[&SyncQueue],
                g: &Mutex<T>,
                mut cond: impl FnMut(&T) -> Option<bool>,
            ) -> bool {
                loop {
                    {
                        let d = g.lock().unwrap();
                        if let Some(r) = cond(&d) {
                            return r;
                        }
                    }
                    let token = WaitToken::current();
                    for wq in queues {
                        let mut q = wq.q.lock().unwrap();
                        q.push_back(token.clone());
                    }
                    token.wait(None);
                    for wq in queues {
                        let mut q = wq.q.lock().unwrap();
                        q.retain(|queued| !queued.same(&token));
                    }
                }
            }
            pub fn wait_guard<T>(&self, g: &Mutex<T>) {
                let token = WaitToken::current();
                {
                    let mut q = self.q.lock().unwrap();
                    q.push_back(token.clone());
                }
                drop(g.lock().unwrap());
                token.wait(None);
            }
            pub fn wait_timeout<T>(&self, g: &Mutex<T>, timeout: Duration) -> bool {
                let token = WaitToken::current();
                {
                    let mut q = self.q.lock().unwrap();
                    q.push_back(token.clone());
                }
                drop(g.lock().unwrap());
                match token.wait(Some(timeout)) {
                    WaitOutcome::Event => true,
                    WaitOutcome::Timeout => {
                        let mut q = self.q.lock().unwrap();
                        q.retain(|queued| !queued.same(&token));
                        false
                    }
                }
            }
            pub fn reg_epoll(&self, task_id: usize, epfd: usize, fd: usize) {
                self.eq
                    .lock()
                    .unwrap()
                    .push_back(RegEp { task_id, epfd, fd });
            }
            pub fn unreg_epoll(&self, task_id: usize, epfd: usize, fd: usize) -> bool {
                let mut eql = self.eq.lock().unwrap();
                for i in 0..eql.len() {
                    if eql[i].task_id == task_id && eql[i].epfd == epfd && eql[i].fd == fd {
                        eql.remove(i);
                        return true;
                    }
                }
                false
            }
        }

        // AGENT: keep only semaphore state that is currently wired; last-operator PID
        // can return with semop/semctl semantics if those syscalls are implemented.
        struct SemaInner {
            cnt: isize,
            rm: bool,
            bus: EvBus,
        }

        pub struct Sema {
            inner: Arc<Mutex<SemaInner>>,
        }

        pub struct SemaGuard<'a> {
            s: &'a Sema,
        }

        impl Sema {
            // AGENT: initialize active semaphore state only; last-operator PID is not
            // modeled until System V semaphore syscall semantics are wired.
            pub fn new(c: isize) -> Self {
                Sema {
                    inner: Arc::new(Mutex::new(SemaInner {
                        cnt: c,
                        rm: false,
                        bus: EvBus::default(),
                    })),
                }
            }
            // AGENT: mark the simplified semaphore removed and make removed state win
            // over any stale acquire-ready bit.
            pub fn remove(&self) {
                let mut i = self.inner.lock().unwrap();
                if i.rm {
                    return;
                }
                i.rm = true;
                i.bus.change(EvFlag::SEM_ACQ, EvFlag::SEM_RM);
            }
            // AGENT: release is a no-op after remove(); Drop callers cannot propagate a
            // Result, and removed semaphores must not become acquire-ready again.
            pub fn release(&self) {
                let mut i = self.inner.lock().unwrap();
                if i.rm {
                    return;
                }
                i.cnt += 1;
                if i.cnt >= 1 {
                    i.bus.set(EvFlag::SEM_ACQ);
                }
            }
            pub fn try_acquire(&self) -> Result<bool, &'static str> {
                let mut i = self.inner.lock().unwrap();
                if i.rm {
                    return Err("removed");
                }
                if i.cnt >= 1 {
                    i.cnt -= 1;
                    if i.cnt < 1 {
                        i.bus.clear(EvFlag::SEM_ACQ);
                    }
                    Ok(true)
                } else {
                    Ok(false)
                }
            }
            pub fn acquire_spin(&self) -> Result<(), &'static str> {
                loop {
                    match self.try_acquire()? {
                        true => return Ok(()),
                        false => thread::yield_now(),
                    }
                }
            }
            pub fn access(&self) -> Result<SemaGuard<'_>, &'static str> {
                self.acquire_spin()?;
                Ok(SemaGuard { s: self })
            }
            pub fn get_val(&self) -> isize {
                self.inner.lock().unwrap().cnt
            }
            pub fn get_ncnt(&self) -> usize {
                self.inner.lock().unwrap().bus.cb_len()
            }
            // AGENT: keep SEM_ACQ synchronized with the current simplified count value
            // and avoid reviving semaphores after remove().
            pub fn set_val(&self, v: isize) {
                let mut i = self.inner.lock().unwrap();
                if i.rm {
                    return;
                }
                i.cnt = v;
                if i.cnt >= 1 {
                    i.bus.set(EvFlag::SEM_ACQ);
                } else {
                    i.bus.clear(EvFlag::SEM_ACQ);
                }
            }
        }

        impl<'a> Drop for SemaGuard<'a> {
            fn drop(&mut self) {
                self.s.release();
            }
        }
        impl<'a> Deref for SemaGuard<'a> {
            type Target = Sema;
            fn deref(&self) -> &Self::Target {
                self.s
            }
        }

        // AGENT: futex wait queues keep kernel-style wait tokens instead of host
        // thread handles.
        #[derive(Clone)]
        struct FutexWaiter {
            addr: usize,
            token: WaitToken,
        }

        // AGENT: keep wake and move counts separate because FUTEX_REQUEUE and
        // FUTEX_CMP_REQUEUE expose different return-value semantics.
        struct FutexRequeueResult {
            woken: usize,
            moved: usize,
        }

        impl FutexRequeueResult {
            fn affected(&self) -> usize {
                self.woken + self.moved
            }
        }

        // AGENT: distinguish futex timeout backends while sharing the waiter setup.
        #[derive(Clone, Copy)]
        enum FutexWaitClock {
            Host,
            KernelTimer,
        }

        pub struct FutexBucket {
            waiters: Mutex<VecDeque<FutexWaiter>>,
        }
        impl FutexBucket {
            pub fn new() -> Self {
                Self {
                    waiters: Mutex::new(VecDeque::new()),
                }
            }
            // AGENT: added assert to enforce addr == val address
            pub fn wait(
                &self,
                addr: usize,
                expected: u32,
                val: &AtomicU32,
                timeout: Option<Duration>,
            ) -> Result<(), &'static str> {
                self.wait_inner(addr, expected, val, timeout, FutexWaitClock::Host)
            }

            // AGENT: futex syscall timeouts use the kernel timer wheel so timeout wakeup
            // follows the same logical clock as scheduler ticks.
            pub fn wait_with_timer(
                &self,
                addr: usize,
                expected: u32,
                val: &AtomicU32,
                timeout: Option<Duration>,
            ) -> Result<(), &'static str> {
                self.wait_inner(addr, expected, val, timeout, FutexWaitClock::KernelTimer)
            }

            // AGENT: compare and enqueue under one queue lock so a wake cannot slip
            // between seeing the expected value and publishing this waiter.
            fn wait_inner(
                &self,
                addr: usize,
                expected: u32,
                val: &AtomicU32,
                timeout: Option<Duration>,
                clock: FutexWaitClock,
            ) -> Result<(), &'static str> {
                assert_eq!(val.as_ptr() as usize, addr, "addr must match val address");
                let token = WaitToken::current();
                {
                    let mut w = self.waiters.lock().unwrap();
                    if val.load(Ordering::SeqCst) != expected {
                        return Err("changed");
                    }
                    w.push_back(FutexWaiter {
                        addr,
                        token: token.clone(),
                    });
                }

                let outcome = match (clock, timeout) {
                    (FutexWaitClock::KernelTimer, Some(timeout)) => token.wait_with_timer(timeout),
                    _ => token.wait(timeout),
                };
                self.finish_wait(&token, outcome)
            }

            fn finish_wait(
                &self,
                token: &WaitToken,
                outcome: WaitOutcome,
            ) -> Result<(), &'static str> {
                match outcome {
                    WaitOutcome::Event => Ok(()),
                    WaitOutcome::Timeout => {
                        let mut w = self.waiters.lock().unwrap();
                        w.retain(|waiter| !waiter.token.same(token));
                        Err("timeout")
                    }
                }
            }
            pub fn wake(&self, addr: usize, count: usize) -> usize {
                let mut w = self.waiters.lock().unwrap();
                Self::wake_locked(&mut w, addr, count)
            }
            // AGENT: process exit wakes and removes every futex waiter owned by this bucket.
            pub fn wake_all(&self) -> usize {
                let mut w = self.waiters.lock().unwrap();
                let count = w.len();
                for waiter in w.drain(..) {
                    waiter.token.wake();
                }
                count
            }
            pub fn wake_op(
                &self,
                addr: usize,
                count: usize,
                addr2: usize,
                count2: usize,
                op: impl FnOnce() -> Result<u32, &'static str>,
                cmp: impl FnOnce(u32) -> Result<bool, &'static str>,
            ) -> Result<usize, &'static str> {
                let mut w = self.waiters.lock().unwrap();
                let old = op()?;
                let should_wake_addr2 = cmp(old)?;
                let mut woken = Self::wake_locked(&mut w, addr, count);
                if should_wake_addr2 {
                    woken += Self::wake_locked(&mut w, addr2, count2);
                }
                Ok(woken)
            }
            pub fn requeue(&self, src: usize, dst: usize, wake_n: usize, move_n: usize) -> usize {
                let mut w = self.waiters.lock().unwrap();
                Self::requeue_locked(&mut w, src, dst, wake_n, move_n).woken
            }
            pub fn cmp_requeue(
                &self,
                src: usize,
                dst: usize,
                wake_n: usize,
                move_n: usize,
                val: &AtomicU32,
                expected: u32,
            ) -> Result<usize, &'static str> {
                assert_eq!(val.as_ptr() as usize, src, "addr must match val address");
                let mut w = self.waiters.lock().unwrap();
                if val.load(Ordering::SeqCst) != expected {
                    return Err("changed");
                }
                Ok(Self::requeue_locked(&mut w, src, dst, wake_n, move_n).affected())
            }
            pub fn pending_at(&self, addr: usize) -> usize {
                self.waiters
                    .lock()
                    .unwrap()
                    .iter()
                    .filter(|waiter| waiter.addr == addr)
                    .count()
            }
            fn wake_locked(
                waiters: &mut VecDeque<FutexWaiter>,
                addr: usize,
                count: usize,
            ) -> usize {
                let mut woken = 0;
                waiters.retain(|waiter| {
                    if waiter.addr == addr && woken < count {
                        if waiter.token.wake() {
                            woken += 1;
                        }
                        false
                    } else {
                        true
                    }
                });
                woken
            }
            fn requeue_locked(
                waiters: &mut VecDeque<FutexWaiter>,
                src: usize,
                dst: usize,
                wake_n: usize,
                move_n: usize,
            ) -> FutexRequeueResult {
                let (mut wk, mut mv) = (0, 0);
                for waiter in waiters.iter_mut() {
                    if waiter.addr == src {
                        if wk < wake_n {
                            if waiter.token.wake() {
                                wk += 1;
                            }
                        } else if mv < move_n {
                            waiter.addr = dst;
                            mv += 1;
                        }
                    }
                }
                waiters.retain(|waiter| !waiter.token.is_woken());
                FutexRequeueResult {
                    woken: wk,
                    moved: mv,
                }
            }
        }
    }
    pub mod time {
        // AGENT
        use super::*;

        // AGENT: global timer wheel storage; TimerWheel owns Vec slots, so it is lazily
        // initialized instead of built directly in a const static.
        pub static TIMER_WHEEL: std::sync::OnceLock<Mutex<TimerWheel>> = std::sync::OnceLock::new();

        // AGENT: single access point for the simulator-wide logical timer wheel.
        pub fn global_timer_wheel() -> &'static Mutex<TimerWheel> {
            TIMER_WHEEL.get_or_init(|| Mutex::new(TimerWheel::new()))
        }

        // AGENT: typed timer targets let expiry dispatch route through real kernel-sim
        // wakeup paths instead of interpreting an untyped numeric callback id.
        #[derive(Clone)]
        pub enum TimerTarget {
            Noop,
            WakeToken {
                token: WaitToken,
            },
            WakeTask {
                task_id: usize,
            },
            SignalTask {
                task_id: usize,
                signo: i32,
                sender_tid: isize,
            },
        }

        // AGENT: timer entries keep a numeric id only for cancellation; behavior lives
        // in TimerTarget.
        #[derive(Clone)]
        pub struct TimerEntry {
            pub id: usize,
            pub deadline: usize,
            pub interval: usize,
            pub target: TimerTarget,
            pub active: bool,
            pub repeat: bool,
        }

        impl TimerEntry {
            pub fn new(deadline: usize, interval: usize, id: usize) -> Self {
                Self::with_target(id, deadline, interval, TimerTarget::Noop)
            }

            pub fn with_target(
                id: usize,
                deadline: usize,
                interval: usize,
                target: TimerTarget,
            ) -> Self {
                Self {
                    id,
                    deadline,
                    interval,
                    target,
                    active: true,
                    repeat: interval > 0,
                }
            }

            // AGENT: a timer expires on the tick that reaches its deadline.
            pub fn expired(&self) -> bool {
                CLK.load(Ordering::Relaxed) >= self.deadline
            }

            pub fn reset(&mut self) {
                if self.repeat {
                    self.deadline = CLK.load(Ordering::Relaxed) + self.interval;
                } else {
                    self.active = false;
                }
            }

            pub fn remaining(&self) -> usize {
                let now = CLK.load(Ordering::Relaxed);
                if now >= self.deadline {
                    0
                } else {
                    self.deadline - now
                }
            }

            pub fn cancel(&mut self) {
                self.active = false;
            }
        }

        // AGENT: timer wheel advanced from the CPU0 schedule_tick path.
        pub struct TimerWheel {
            pub slots: Vec<Vec<TimerEntry>>,
            pub current_slot: usize,
            next_id: usize,
        }

        impl TimerWheel {
            pub fn new() -> Self {
                let mut slots = Vec::with_capacity(TIMER_WHEEL_SIZE);
                for _ in 0..TIMER_WHEEL_SIZE {
                    slots.push(Vec::new());
                }
                Self {
                    slots,
                    current_slot: CLK.load(Ordering::Relaxed) % TIMER_WHEEL_SIZE,
                    next_id: 1,
                }
            }

            // AGENT: allocate a cancelable timer id and bind it to a typed expiry target.
            pub fn register_timer(
                &mut self,
                deadline: usize,
                interval: usize,
                target: TimerTarget,
            ) -> usize {
                let id = self.next_id;
                self.next_id = self.next_id.saturating_add(1).max(1);
                self.add_timer(TimerEntry::with_target(id, deadline, interval, target));
                id
            }

            pub fn add_timer(&mut self, entry: TimerEntry) {
                self.next_id = self.next_id.max(entry.id.saturating_add(1));
                // AGENT: far-future deadlines may land in a slot before they expire; the
                // advance path keeps them in that slot until a later wheel pass reaches
                // or passes the full absolute deadline.
                let slot = entry.deadline % TIMER_WHEEL_SIZE;
                self.slots[slot].push(entry);
            }

            pub fn advance(&mut self) -> Vec<TimerEntry> {
                self.current_slot = (self.current_slot + 1) % TIMER_WHEEL_SIZE;
                let mut fired = Vec::new();
                let slot = &mut self.slots[self.current_slot];
                let mut remaining = Vec::new();
                for entry in slot.drain(..) {
                    if entry.active && entry.expired() {
                        fired.push(entry);
                    } else if entry.active {
                        remaining.push(entry);
                    }
                }
                *slot = remaining;
                for t in fired.iter_mut() {
                    if t.repeat {
                        t.reset();
                        let new_slot = t.deadline % TIMER_WHEEL_SIZE;
                        let clone =
                            TimerEntry::with_target(t.id, t.deadline, t.interval, t.target.clone());
                        self.slots[new_slot].push(clone);
                    }
                }
                fired
            }

            pub fn cancel(&mut self, id: usize) -> bool {
                for slot in self.slots.iter_mut() {
                    for entry in slot.iter_mut() {
                        if entry.id == id && entry.active {
                            entry.active = false;
                            return true;
                        }
                    }
                }
                false
            }

            pub fn active_count(&self) -> usize {
                self.slots
                    .iter()
                    .flat_map(|s| s.iter())
                    .filter(|e| e.active)
                    .count()
            }
        }

        // AGENT: convert host Duration values into simulator clock ticks, rounding up
        // so any nonzero timeout gets at least one logical tick.
        pub fn duration_to_ticks(timeout: Duration) -> usize {
            if timeout.is_zero() {
                return 0;
            }
            let tick_nanos = 1_000_000_000u128 / TIMER_TICK_HZ as u128;
            let nanos = timeout.as_nanos();
            let ticks = (nanos + tick_nanos - 1) / tick_nanos;
            usize::try_from(ticks).unwrap_or(usize::MAX).max(1)
        }
    }

    pub use self::arch::*;
    pub use self::current::*;
    pub use self::kernel_base::*;
    pub use self::kernel_ops::*;
    pub use self::net::*;
    pub use self::prelude::*;
    pub use self::sync::*;
    pub use self::time::*;
}
pub mod fs {
    // AGENT
    use super::*;

    pub mod block_cache {
        // AGENT
        use super::*;

        pub const BLOCK_CACHE_BLOCK_SIZE: usize = 512;

        // AGENT: identify cached data by block-device namespace plus block number
        // instead of overloading file-descriptor ids as cache keys.
        #[derive(Clone, Copy, Debug, PartialEq, Eq)]
        pub struct BlockKey {
            pub dev: usize,
            pub block: usize,
        }

        impl BlockKey {
            pub const fn new(dev: usize, block: usize) -> Self {
                Self { dev, block }
            }

            fn hash(self) -> usize {
                let mut h = self.block ^ (self.block >> 7);
                h ^= self.dev.wrapping_mul(0x9E37_79B9);
                h ^ (h >> 11)
            }
        }

        // AGENT: narrow block-device interface used by BlockCache; concrete QEMU
        // drivers can later implement this over virtio-blk or another real device.
        pub trait BlockDevice {
            fn read_block(&self, dev: usize, block: usize) -> Result<Vec<u8>, &'static str>;
            fn write_block(
                &self,
                dev: usize,
                block: usize,
                data: &[u8],
            ) -> Result<(), &'static str>;
        }

        // AGENT: compatibility device for existing simulator-style cache smoke tests.
        pub struct SyntheticBlockDevice {
            pub latency: Duration,
        }

        impl SyntheticBlockDevice {
            pub fn new(latency: Duration) -> Self {
                Self { latency }
            }
        }

        impl BlockDevice for SyntheticBlockDevice {
            fn read_block(&self, dev: usize, block: usize) -> Result<Vec<u8>, &'static str> {
                let tick_before = CLK.load(Ordering::Relaxed);
                if self.latency.as_nanos() > 0 {
                    thread::sleep(self.latency);
                }
                let mut payload = Vec::with_capacity(BLOCK_CACHE_BLOCK_SIZE);
                let seed =
                    block.wrapping_mul(0x9E37_79B9) ^ dev.wrapping_mul(0x85EB_CA6B) ^ tick_before;
                for i in 0..BLOCK_CACHE_BLOCK_SIZE {
                    payload.push(((seed.wrapping_add(i)) & 0xFF) as u8);
                }
                Ok(payload)
            }

            fn write_block(
                &self,
                _dev: usize,
                _block: usize,
                _data: &[u8],
            ) -> Result<(), &'static str> {
                Ok(())
            }
        }

        pub struct CacheSlot {
            pub key: BlockKey,
            pub payload: Vec<u8>,
            pub modified: bool,
        }
        pub struct CacheChain {
            pub lk: Spin,
            pub items: Mutex<Vec<CacheSlot>>,
        }
        impl CacheChain {
            pub fn new() -> Self {
                Self {
                    lk: Spin::new(),
                    items: Mutex::new(Vec::new()),
                }
            }
        }

        pub struct BlockCache {
            pub chains: Vec<CacheChain>,
            pub width: usize,
        }
        impl BlockCache {
            // AGENT: BlockCache chains use SpinGuard for short metadata critical sections.
            pub fn new(w: usize) -> Self {
                let mut c = Vec::with_capacity(w);
                for _ in 0..w {
                    c.push(CacheChain::new());
                }
                Self {
                    chains: c,
                    width: w,
                }
            }
            // AGENT: keep all chain hashing through one helper.
            pub fn idx(&self, key: BlockKey) -> usize {
                key.hash() % self.width
            }

            // AGENT: read cached blocks through an explicit block device; miss I/O is
            // outside the chain SpinGuard and insertion double-checks for races.
            pub fn read_block_cached<D: BlockDevice + ?Sized>(
                &self,
                device: &D,
                dev: usize,
                block: usize,
            ) -> Result<Vec<u8>, &'static str> {
                let key = BlockKey::new(dev, block);
                let ci = self.idx(key);
                let ch = &self.chains[ci];

                {
                    let _guard = ch.lk.guard();
                    let e = ch.items.lock().unwrap();
                    if let Some(slot) = e.iter().find(|slot| slot.key == key) {
                        return Ok(slot.payload.clone());
                    }
                }

                let block_data = device.read_block(dev, block)?;
                if block_data.len() != BLOCK_CACHE_BLOCK_SIZE {
                    return Err("eio");
                }
                let result = block_data.clone();
                let slot = CacheSlot {
                    key,
                    payload: block_data,
                    modified: false,
                };
                {
                    let _guard = ch.lk.guard();
                    let mut items = ch.items.lock().unwrap();
                    if let Some(slot) = items.iter().find(|slot| slot.key == key) {
                        return Ok(slot.payload.clone());
                    }
                    items.push(slot);
                }
                Ok(result)
            }

            // AGENT: compatibility wrapper for older tests that exercised synthetic
            // cache miss latency without a concrete block-device implementation.
            pub fn fetch(&self, k: usize, lat: Duration) -> Option<Vec<u8>> {
                let device = SyntheticBlockDevice::new(lat);
                self.read_block_cached(&device, 0, k).ok()
            }

            // AGENT: update or insert one complete cached block and mark it dirty for a
            // later flush through the block-device interface.
            pub fn write_block_cached(
                &self,
                dev: usize,
                block: usize,
                data: &[u8],
            ) -> Result<(), &'static str> {
                if data.len() != BLOCK_CACHE_BLOCK_SIZE {
                    return Err("einval");
                }
                let key = BlockKey::new(dev, block);
                let ci = self.idx(key);
                let ch = &self.chains[ci];
                let _guard = ch.lk.guard();
                let mut items = ch.items.lock().unwrap();
                if let Some(slot) = items.iter_mut().find(|slot| slot.key == key) {
                    slot.payload.clear();
                    slot.payload.extend_from_slice(data);
                    slot.modified = true;
                    return Ok(());
                }
                items.push(CacheSlot {
                    key,
                    payload: data.to_vec(),
                    modified: true,
                });
                Ok(())
            }

            // AGENT: write dirty blocks outside cache-chain SpinGuards, then clear the
            // dirty bit only if the cached payload did not change during writeback.
            pub fn flush_dirty<D: BlockDevice + ?Sized>(
                &self,
                device: &D,
            ) -> Result<usize, &'static str> {
                let mut flushed = 0usize;
                for chain_idx in 0..self.chains.len() {
                    let ch = &self.chains[chain_idx];
                    let dirty = {
                        let _guard = ch.lk.guard();
                        let items = ch.items.lock().unwrap();
                        items
                            .iter()
                            .filter(|slot| slot.modified)
                            .map(|slot| (slot.key, slot.payload.clone()))
                            .collect::<Vec<_>>()
                    };

                    for (key, payload) in dirty {
                        device.write_block(key.dev, key.block, &payload)?;
                        let _guard = ch.lk.guard();
                        let mut items = ch.items.lock().unwrap();
                        if let Some(slot) = items.iter_mut().find(|slot| slot.key == key) {
                            if slot.modified && slot.payload == payload {
                                slot.modified = false;
                                flushed += 1;
                            }
                        }
                    }
                }
                Ok(flushed)
            }

            // AGENT: keep the legacy no-device sync entry as a GKL-only barrier; dirty
            // cache entries must be flushed through flush_dirty() or sync_all_with_device().
            pub fn sync_all(&self, id: usize) {
                let _gkl = GKL.guard(id);
            }

            // AGENT: sync with a device performs real dirty writeback instead of
            // clearing cache state without I/O.
            pub fn sync_all_with_device<D: BlockDevice + ?Sized>(
                &self,
                id: usize,
                device: &D,
            ) -> Result<usize, &'static str> {
                // AGENT: route GKL through the guard so Drop performs owner-checked release.
                let _gkl = GKL.guard(id);
                self.flush_dirty(device)
            }

            // AGENT: invalidate uses SpinGuard so early exits cannot leak the chain lock.
            pub fn invalidate_block(&self, dev: usize, block: usize) {
                let key = BlockKey::new(dev, block);
                let ci = self.idx(key);
                let ch = &self.chains[ci];
                let _guard = ch.lk.guard();
                {
                    let mut items = ch.items.lock().unwrap();
                    let mut idx = 0;
                    while idx < items.len() {
                        if items[idx].key == key {
                            items.remove(idx);
                        } else {
                            idx += 1;
                        }
                    }
                }
            }

            // AGENT: total_entries observes each chain under SpinGuard.
            pub fn total_entries(&self) -> usize {
                let mut total = 0;
                for i in 0..self.chains.len() {
                    let ch = &self.chains[i];
                    let _guard = ch.lk.guard();
                    let n = ch.items.lock().unwrap().len();
                    total += n;
                }
                total
            }

            // AGENT: dirty_count observes each chain under SpinGuard.
            pub fn dirty_count(&self) -> usize {
                let mut count = 0;
                for i in 0..self.chains.len() {
                    let ch = &self.chains[i];
                    let _guard = ch.lk.guard();
                    let items = ch.items.lock().unwrap();
                    for slot in items.iter() {
                        if slot.modified {
                            count += 1;
                        }
                    }
                    drop(items);
                }
                count
            }

            // AGENT: eviction holds each chain SpinGuard only while filtering metadata.
            pub fn evict_cold(&self, max_age: usize) -> usize {
                let now = CLK.load(Ordering::Relaxed);
                let mut evicted = 0;
                for i in 0..self.chains.len() {
                    let ch = &self.chains[i];
                    let _guard = ch.lk.guard();
                    {
                        let mut items = ch.items.lock().unwrap();
                        let before = items.len();
                        items.retain(|slot| {
                            let age =
                                now.wrapping_sub(slot.key.block.wrapping_mul(3) ^ slot.key.dev);
                            slot.modified || age < max_age
                        });
                        evicted += before - items.len();
                    }
                }
                evicted
            }
        }
    }
    pub mod channel {
        // AGENT
        use super::*;

        pub struct Channel {
            pub buf: Mutex<CircBuf>,
            pub guard: Spin,
            pub wq: SyncQueue,
            pub shut: AtomicBool,
        }
        impl Channel {
            // AGENT: Channel keeps the legacy Spin field for API compatibility, but
            // blocking send/recv coordination is handled by CircBuf's Mutex + SyncQueue.
            pub fn new(cap: usize) -> Self {
                let effective_cap = cap.clamp(1, 1 << 20);
                Self {
                    buf: Mutex::new(CircBuf::new(effective_cap)),
                    guard: Spin::new(),
                    wq: SyncQueue::new(),
                    shut: AtomicBool::new(false),
                }
            }
            // AGENT: wait registration is protected by buf and wq locks, and the
            // WaitToken wait happens after both are released so no Spin is held while blocking.
            pub fn recv(&self) -> Option<u8> {
                loop {
                    let token = WaitToken::current();
                    {
                        let mut ring = self.buf.lock().unwrap();
                        if let Some(v) = ring.pop() {
                            return Some(v);
                        }
                        let mut waiters = self.wq.q.lock().unwrap();
                        if self.shut.load(Ordering::Acquire) {
                            return None;
                        }
                        waiters.push_back(token.clone());
                    }
                    token.wait(None);
                }
            }
            // AGENT: data insertion uses the buffer mutex and wakes waiters after the
            // mutation; no Spin is held during wakeup.
            pub fn send(&self, v: u8) -> bool {
                let success = {
                    let mut ring = self.buf.lock().unwrap();
                    ring.push(v)
                };
                if success {
                    // HUMAN
                    self.wq.signal();
                }
                success
            }
            // AGENT: close publishes shutdown before broadcasting so recv either sees
            // shut under wq.q or is already queued for the broadcast.
            pub fn close(&self) {
                self.shut.store(true, Ordering::Release);
                // HUMAN
                self.wq.broadcast();
            }

            // AGENT: non-blocking receive reads only under the buffer mutex.
            pub fn try_recv(&self) -> Option<u8> {
                self.buf.lock().unwrap().pop()
            }

            // AGENT: batch send performs all buffer writes under the mutex and wakes up
            // to the number of bytes inserted after releasing the data lock.
            pub fn send_batch(&self, data: &[u8]) -> usize {
                let mut ring = self.buf.lock().unwrap();
                let mut written = 0;
                for &byte in data {
                    if !ring.push(byte) {
                        break;
                    }
                    written += 1;
                }
                if written > 0 {
                    drop(ring);
                    self.wq.signal_n(written);
                }
                written
            }

            // AGENT: depth is a pure buffer query and does not need the legacy Spin.
            pub fn depth(&self) -> usize {
                self.buf.lock().unwrap().len()
            }

            // AGENT: draining holds only the buffer mutex and never waits.
            pub fn drain_all(&self) -> Vec<u8> {
                let mut result = Vec::new();
                let mut ring = self.buf.lock().unwrap();
                while let Some(byte) = ring.pop() {
                    result.push(byte);
                }
                result
            }

            // AGENT: shutdown state is published with release and observed with acquire.
            pub fn is_closed(&self) -> bool {
                self.shut.load(Ordering::Acquire)
            }

            // AGENT: remaining capacity is a pure buffer query and does not need Spin.
            pub fn remaining_capacity(&self) -> usize {
                self.buf.lock().unwrap().remaining()
            }
        }
    }
    pub mod epoll {
        // AGENT
        use super::*;

        #[repr(C)]
        #[derive(Clone, Copy)]
        pub struct EpData {
            pub ptr: u64,
        }

        #[repr(C)]
        #[derive(Clone)]
        pub struct EpEvent {
            pub events: u32,
            pub data: EpData,
        }
        impl EpEvent {
            pub const IN: u32 = 0x001;
            pub const OUT: u32 = 0x004;
            pub const ERR: u32 = 0x008;
            pub const HUP: u32 = 0x010;
            pub const PRI: u32 = 0x002;
            pub const RDNORM: u32 = 0x040;
            pub const RDBAND: u32 = 0x080;
            pub const WRNORM: u32 = 0x100;
            pub const WRBAND: u32 = 0x200;
            pub const MSG: u32 = 0x400;
            pub const RDHUP: u32 = 0x2000;
            pub const EXCL: u32 = 1 << 28;
            pub const WAKEUP: u32 = 1 << 29;
            pub const ONESHOT: u32 = 1 << 30;
            pub const ET: u32 = 1 << 31;
            pub fn has(&self, ev: u32) -> bool {
                (self.events & ev) != 0
            }
        }

        pub struct EpCtlOp;
        impl EpCtlOp {
            pub const ADD: i32 = 1;
            pub const DEL: i32 = 2;
            pub const MOD: i32 = 3;
        }

        // AGENT: epoll instances now own both the interest table and a wait queue that
        // source readiness callbacks can wake.
        #[derive(Clone)]
        pub struct EpInst {
            pub events: Arc<Mutex<BTreeMap<usize, EpEvent>>>,
            pub ready: Arc<Mutex<BTreeSet<usize>>>,
            // AGENT: epoll_wait sleeps on this queue and source readiness callbacks
            // wake it when a registered fd becomes ready.
            pub waiters: Arc<Mutex<VecDeque<WaitToken>>>,
            // AGENT: fd -> EvBus subscription id for registrations backed by a
            // cancellable readiness source such as PipeNode.
            source_subs: Arc<Mutex<BTreeMap<usize, usize>>>,
        }
        impl EpInst {
            pub fn new() -> Self {
                EpInst {
                    events: Arc::new(Mutex::new(BTreeMap::new())),
                    ready: Arc::new(Mutex::new(BTreeSet::new())),
                    waiters: Arc::new(Mutex::new(VecDeque::new())),
                    source_subs: Arc::new(Mutex::new(BTreeMap::new())),
                }
            }
            // AGENT: notify epoll_wait waiters that one watched fd has reached a
            // readiness state. Stale callbacks are ignored if the fd is no longer
            // registered in this epoll instance.
            pub fn mark_ready(&self, fd: usize) {
                if !self.events.lock().unwrap().contains_key(&fd) {
                    return;
                }
                self.ready.lock().unwrap().insert(fd);
                let batch: Vec<WaitToken> = self.waiters.lock().unwrap().drain(..).collect();
                for token in batch {
                    token.wake();
                }
            }
            // AGENT: clear cached readiness before a level-triggered rescan; new
            // callbacks racing after this point repopulate the cache and wake waiters.
            pub fn clear_ready(&self) {
                self.ready.lock().unwrap().clear();
            }
            // AGENT: preserve the compatibility ready cache for FLike::Ep::poll().
            pub fn replace_ready(&self, ready_fds: BTreeSet<usize>) {
                *self.ready.lock().unwrap() = ready_fds;
            }
            // AGENT: enqueue an epoll_wait token only if no readiness callback has
            // populated the cache since the last scan.
            pub fn prepare_wait(&self) -> Option<WaitToken> {
                let ready = self.ready.lock().unwrap();
                if !ready.is_empty() {
                    return None;
                }
                let token = WaitToken::current();
                self.waiters.lock().unwrap().push_back(token.clone());
                Some(token)
            }
            // AGENT: remove a timed-out epoll_wait token from the instance queue.
            pub fn remove_waiter(&self, token: &WaitToken) {
                self.waiters
                    .lock()
                    .unwrap()
                    .retain(|queued| !queued.same(token));
            }
            // AGENT: remember which EvBus callback backs a watched fd.
            pub fn set_source_sub(&self, fd: usize, sub_id: usize) {
                self.source_subs.lock().unwrap().insert(fd, sub_id);
            }
            // AGENT: take the callback id so the caller can unregister it from the
            // concrete source while processing epoll_ctl(DEL/MOD).
            pub fn take_source_sub(&self, fd: usize) -> Option<usize> {
                self.source_subs.lock().unwrap().remove(&fd)
            }
            pub fn control(
                &mut self,
                op: i32,
                fd: usize,
                ev: &EpEvent,
            ) -> Result<(), &'static str> {
                let mut events = self.events.lock().unwrap();
                match op {
                    EpCtlOp::ADD => {
                        if events.contains_key(&fd) {
                            return Err("eexist");
                        }
                        events.insert(fd, ev.clone());
                        Ok(())
                    }
                    EpCtlOp::MOD => {
                        if !events.contains_key(&fd) {
                            return Err("enoent");
                        }
                        events.insert(fd, ev.clone());
                        Ok(())
                    }
                    EpCtlOp::DEL => {
                        if events.remove(&fd).is_none() {
                            return Err("enoent");
                        }
                        self.ready.lock().unwrap().remove(&fd);
                        Ok(())
                    }
                    _ => Err("einval"),
                }
            }
        }
    }
    pub mod fd {
        // AGENT
        use super::*;

        #[derive(Debug, Clone, Copy)]
        pub struct FdOpt {
            pub rd: bool,
            pub wr: bool,
            pub ap: bool,
            pub nb: bool,
        }
        impl Default for FdOpt {
            fn default() -> Self {
                Self {
                    rd: true,
                    wr: false,
                    ap: false,
                    nb: false,
                }
            }
        }

        pub(crate) struct FdState {
            pub(crate) off: u64,
            pub(crate) opt: FdOpt,
            pub(crate) flk: u8,
        }
        impl FdState {
            fn create(opt: FdOpt) -> Arc<RwLock<Self>> {
                Arc::new(RwLock::new(FdState {
                    off: 0,
                    opt,
                    flk: 0,
                }))
            }
        }

        // AGENT: fd flags that belong to one descriptor entry, not to the shared open
        // file description.
        #[derive(Clone)]
        pub struct FdEntry {
            desc: Arc<OpenFileDescription>,
            cloexec: bool,
        }

        // AGENT: shared open-file description; dup/fork clone FdEntry while sharing
        // this object, so offset/status state and pipe endpoint lifetime remain shared.
        pub struct OpenFileDescription {
            file: FLike,
            status: RwLock<FdOpt>,
        }

        impl OpenFileDescription {
            // AGENT: build an open-file description around a concrete file object.
            pub fn new(file: FLike) -> Self {
                let status = file.status_flags();
                Self {
                    file,
                    status: RwLock::new(status),
                }
            }

            pub fn file(&self) -> &FLike {
                &self.file
            }

            pub fn read(&self, buf: &mut [u8]) -> Result<usize, &'static str> {
                self.file.read(buf)
            }

            pub fn write(&self, buf: &[u8]) -> Result<usize, &'static str> {
                self.file.write(buf)
            }

            pub fn poll(&self) -> (bool, bool, bool) {
                self.file.poll()
            }

            pub fn io_ctl(&self, req: usize, arg: usize) -> Result<usize, &'static str> {
                self.file.io_ctl(req, arg)
            }

            pub fn status_flags(&self) -> FdOpt {
                *self.status.read().unwrap()
            }

            pub fn set_status_flags(&self, flags: usize) -> Result<(), &'static str> {
                self.file.set_status_flags(flags)?;
                let mut status = self.status.write().unwrap();
                status.nb = (flags & O_NONBLOCK) != 0;
                status.ap = (flags & O_APPEND) != 0;
                Ok(())
            }

            pub fn regular_handle(&self) -> Option<FHandle> {
                match &self.file {
                    FLike::File(f) => Some(f.clone()),
                    _ => None,
                }
            }

            pub fn metadata_pages(&self) -> usize {
                match &self.file {
                    FLike::File(f) => f.metadata_sz() / PAGE_SZ + 1,
                    _ => 1,
                }
            }
        }

        impl FdEntry {
            // AGENT: create a descriptor entry over a fresh open-file description.
            pub fn new(file: FLike) -> Self {
                Self::with_cloexec(file, false)
            }

            // AGENT: create a descriptor entry with per-fd close-on-exec state.
            pub fn with_cloexec(file: FLike, cloexec: bool) -> Self {
                Self {
                    desc: Arc::new(OpenFileDescription::new(file)),
                    cloexec,
                }
            }

            // AGENT: duplicate one fd entry while sharing its open-file description.
            pub fn dup(&self, cloexec: bool) -> Self {
                Self {
                    desc: self.desc.clone(),
                    cloexec,
                }
            }

            // AGENT: fork preserves each fd entry's own FD_CLOEXEC flag.
            pub fn fork_dup(&self) -> Self {
                self.dup(self.cloexec)
            }

            pub fn is_cloexec(&self) -> bool {
                self.cloexec
            }

            pub fn set_cloexec(&mut self, val: bool) {
                self.cloexec = val;
            }

            pub fn read(&self, buf: &mut [u8]) -> Result<usize, &'static str> {
                self.desc.read(buf)
            }

            pub fn write(&self, buf: &[u8]) -> Result<usize, &'static str> {
                self.desc.write(buf)
            }

            pub fn poll(&self) -> (bool, bool, bool) {
                self.desc.poll()
            }

            pub fn io_ctl(&self, req: usize, arg: usize) -> Result<usize, &'static str> {
                self.desc.io_ctl(req, arg)
            }

            pub fn status_flags(&self) -> FdOpt {
                self.desc.status_flags()
            }

            pub fn set_status_flags(&self, flags: usize) -> Result<(), &'static str> {
                self.desc.set_status_flags(flags)
            }

            pub fn regular_handle(&self) -> Option<FHandle> {
                self.desc.regular_handle()
            }

            pub fn metadata_pages(&self) -> usize {
                self.desc.metadata_pages()
            }

            // AGENT: compatibility view for older tests and helpers that inspect FLike.
            pub fn as_flike(&self) -> FLike {
                let mut file = self.desc.file().clone();
                if let FLike::File(ref mut f) = file {
                    f.cloexec = self.cloexec;
                }
                file
            }
        }

        // AGENT: distinguish regular path files from directory nodes for exec checks.
        #[derive(Debug, Clone, Copy, PartialEq, Eq)]
        pub enum FileKind {
            Regular,
            Directory,
        }

        // AGENT: share file contents and executable metadata across all handles.
        pub struct FileNode {
            pub kind: FileKind,
            pub executable: AtomicBool,
            pub data: Arc<Mutex<Vec<u8>>>,
        }

        impl FileNode {
            // AGENT: create a regular in-memory file node with stable shared contents.
            pub fn regular(data: Vec<u8>, executable: bool) -> Self {
                Self {
                    kind: FileKind::Regular,
                    executable: AtomicBool::new(executable),
                    data: Arc::new(Mutex::new(data)),
                }
            }

            // AGENT: create a directory node so exec can reject it distinctly.
            pub fn directory() -> Self {
                Self {
                    kind: FileKind::Directory,
                    executable: AtomicBool::new(false),
                    data: Arc::new(Mutex::new(Vec::new())),
                }
            }
        }

        impl fmt::Debug for FileNode {
            fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
                f.debug_struct("FileNode")
                    .field("kind", &self.kind)
                    .field("executable", &self.executable.load(Ordering::Relaxed))
                    .field("len", &self.data.lock().unwrap().len())
                    .finish()
            }
        }

        // AGENT: file descriptors keep per-handle state while sharing FileNode data.
        #[derive(Clone)]
        pub struct FHandle {
            pub path: String,
            pub node: Arc<FileNode>,
            pub(crate) desc: Arc<RwLock<FdState>>,
            pub pipe: bool,
            pub cloexec: bool,
        }

        #[derive(Debug)]
        pub enum FSeek {
            Start(u64),
            End(i64),
            Cur(i64),
        }

        impl FHandle {
            // AGENT: create a fresh standalone regular node for device-like handles.
            pub fn new(path: &str, opt: FdOpt, pipe: bool, cloexec: bool) -> Self {
                Self {
                    path: path.to_string(),
                    node: Arc::new(FileNode::regular(Vec::new(), false)),
                    desc: FdState::create(opt),
                    pipe,
                    cloexec,
                }
            }
            // AGENT: create a handle over a fresh regular file node.
            pub fn with_data(path: &str, opt: FdOpt, d: Vec<u8>) -> Self {
                Self {
                    path: path.to_string(),
                    node: Arc::new(FileNode::regular(d, false)),
                    desc: FdState::create(opt),
                    pipe: false,
                    cloexec: false,
                }
            }
            // AGENT: open a descriptor over an existing shared FileNode.
            pub fn with_node(path: &str, opt: FdOpt, node: Arc<FileNode>, cloexec: bool) -> Self {
                Self {
                    path: path.to_string(),
                    node,
                    desc: FdState::create(opt),
                    pipe: false,
                    cloexec,
                }
            }
            // AGENT: duplicate only descriptor state; file contents stay shared.
            pub fn dup(&self, cloexec: bool) -> Self {
                FHandle {
                    path: self.path.clone(),
                    node: self.node.clone(),
                    desc: self.desc.clone(),
                    pipe: self.pipe,
                    cloexec,
                }
            }
            pub fn get_opt(&self) -> FdOpt {
                self.desc.read().unwrap().opt
            }

            // AGENT: fcntl(F_SETFL) changes status flags while preserving access mode.
            pub fn set_status_flags(&self, flags: usize) {
                let mut d = self.desc.write().unwrap();
                d.opt.nb = (flags & O_NONBLOCK) != 0;
                d.opt.ap = (flags & O_APPEND) != 0;
            }

            // AGENT: advance the shared open-file-description offset while holding the
            // descriptor state write lock.
            pub fn read(&self, buf: &mut [u8]) -> Result<usize, &'static str> {
                let mut desc = self.desc.write().unwrap();
                if !desc.opt.rd {
                    return Err("ebadf");
                }
                let off = desc.off as usize;
                let d = self.node.data.lock().unwrap();
                if off >= d.len() {
                    return Ok(0);
                }
                let n = min(buf.len(), d.len() - off);
                buf[..n].copy_from_slice(&d[off..off + n]);
                desc.off = (off + n) as u64;
                Ok(n)
            }
            pub fn read_at(&self, off: usize, buf: &mut [u8]) -> Result<usize, &'static str> {
                if !self.desc.read().unwrap().opt.rd {
                    return Err("ebadf");
                }
                if self.desc.read().unwrap().opt.nb {
                    let d = self.node.data.lock().unwrap();
                    if off >= d.len() {
                        return Ok(0);
                    }
                    let n = min(buf.len(), d.len() - off);
                    buf[..n].copy_from_slice(&d[off..off + n]);
                    return Ok(n);
                }
                let d = self.node.data.lock().unwrap();
                if off >= d.len() {
                    return Ok(0);
                }
                let n = min(buf.len(), d.len() - off);
                buf[..n].copy_from_slice(&d[off..off + n]);
                Ok(n)
            }
            // AGENT: append/offset selection and offset advancement happen under one
            // shared descriptor state write lock.
            pub fn write(&self, buf: &[u8]) -> Result<usize, &'static str> {
                let mut desc = self.desc.write().unwrap();
                if !desc.opt.wr {
                    return Err("ebadf");
                }
                let mut d = self.node.data.lock().unwrap();
                let off = if desc.opt.ap {
                    d.len()
                } else {
                    desc.off as usize
                };
                let end = off.checked_add(buf.len()).ok_or("efbig")?;
                if end > d.len() {
                    d.resize(end, 0);
                }
                d[off..end].copy_from_slice(buf);
                desc.off = end as u64;
                Ok(buf.len())
            }
            pub fn write_at(&self, off: usize, buf: &[u8]) -> Result<usize, &'static str> {
                if !self.desc.read().unwrap().opt.wr {
                    return Err("ebadf");
                }
                let mut d = self.node.data.lock().unwrap();
                if off + buf.len() > d.len() {
                    d.resize(off + buf.len(), 0);
                }
                d[off..off + buf.len()].copy_from_slice(buf);
                Ok(buf.len())
            }
            pub fn seek(&self, pos: FSeek) -> Result<u64, &'static str> {
                let mut d = self.desc.write().unwrap();
                d.off = match pos {
                    FSeek::Start(o) => o,
                    FSeek::End(o) => (self.node.data.lock().unwrap().len() as i64 + o) as u64,
                    FSeek::Cur(o) => (d.off as i64 + o) as u64,
                };
                Ok(d.off)
            }

            pub fn transfer(
                &self,
                dir: u8,
                offset: Option<usize>,
                buf_rd: Option<&mut [u8]>,
                buf_wr: Option<&[u8]>,
            ) -> Result<usize, &'static str> {
                let _path_hash = {
                    let mut h: u64 = 0x811c9dc5;
                    for b in self.path.bytes() {
                        h ^= b as u64;
                        h = h.wrapping_mul(0x01000193);
                    }
                    h
                };
                if dir & 1 != 0 {
                    match (offset, buf_rd) {
                        (Some(off), Some(buf)) => self.read_at(off, buf),
                        (None, Some(buf)) => self.read(buf),
                        _ => Err("einval"),
                    }
                } else {
                    match (offset, buf_wr) {
                        (Some(off), Some(buf)) => self.write_at(off, buf),
                        (None, Some(buf)) => self.write(buf),
                        _ => Err("einval"),
                    }
                }
            }

            pub fn set_len(&self, len: u64) -> Result<(), &'static str> {
                if !self.desc.read().unwrap().opt.wr {
                    return Err("ebadf");
                }
                self.node.data.lock().unwrap().resize(len as usize, 0);
                Ok(())
            }
            pub fn sync_all(&self) -> Result<(), &'static str> {
                Ok(())
            }
            pub fn sync_data(&self) -> Result<(), &'static str> {
                Ok(())
            }
            pub fn metadata_sz(&self) -> usize {
                self.node.data.lock().unwrap().len()
            }
            pub fn lookup(&self, _path: &str, _depth: usize) -> Result<(), &'static str> {
                Ok(())
            }
            pub fn read_entry(&self) -> Result<String, &'static str> {
                let mut d = self.desc.write().unwrap();
                if !d.opt.rd {
                    return Err("ebadf");
                }
                let off = d.off;
                d.off += 1;
                Ok(format!("entry_{}", off))
            }
            pub fn poll_status(&self) -> (bool, bool, bool) {
                let desc = self.desc.read().unwrap();
                let readable = desc.opt.rd;
                let writable = desc.opt.wr;
                let _off = desc.off;
                drop(desc);
                let error = self.path.is_empty() && self.node.data.lock().unwrap().is_empty();
                (readable, writable, error)
            }
            pub fn io_ctl(&self, _cmd: u32, _arg: usize) -> Result<usize, &'static str> {
                Ok(0)
            }
            // AGENT: validate that a descriptor can back a regular file mmap.
            pub fn mmap(&self, start: usize, end: usize, off: usize) -> Result<(), &'static str> {
                if start >= end || start % PAGE_SZ != 0 || end % PAGE_SZ != 0 || off % PAGE_SZ != 0
                {
                    return Err("einval");
                }
                if self.pipe || self.node.kind != FileKind::Regular {
                    return Err("enodev");
                }
                if !self.get_opt().rd {
                    return Err("eacces");
                }
                Ok(())
            }
            pub fn inode_ref(&self) -> Arc<Mutex<Vec<u8>>> {
                self.node.data.clone()
            }

            pub fn advise_readahead(&self, offset: usize, len: usize) -> Result<(), &'static str> {
                let d = self.node.data.lock().unwrap();
                let actual_end = min(offset + len, d.len());
                let _readahead_pages = (actual_end.saturating_sub(offset) + PAGE_SZ - 1) / PAGE_SZ;
                Ok(())
            }

            pub fn fallocate(&self, offset: usize, len: usize) -> Result<(), &'static str> {
                if !self.desc.read().unwrap().opt.wr {
                    return Err("ebadf");
                }
                let mut d = self.node.data.lock().unwrap();
                let needed = offset + len;
                if needed > d.len() {
                    d.resize(needed, 0);
                }
                Ok(())
            }

            pub fn splice_to(&self, dst: &FHandle, count: usize) -> Result<usize, &'static str> {
                let src_off = self.desc.read().unwrap().off;
                let sd = self.node.data.lock().unwrap();
                if src_off as usize >= sd.len() {
                    return Ok(0);
                }
                let avail = sd.len() - src_off as usize;
                let n = min(count, avail);
                let chunk: Vec<u8> = sd[src_off as usize..src_off as usize + n].to_vec();
                drop(sd);
                self.desc.write().unwrap().off += n as u64;
                dst.write(&chunk)
            }
        }

        impl fmt::Debug for FHandle {
            fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
                let d = self.desc.read().unwrap();
                f.debug_struct("FH")
                    .field("off", &d.off)
                    .field("path", &self.path)
                    .finish()
            }
        }
    }
    pub mod fs_misc {
        // AGENT
        use super::*;

        // AGENT: keep ring-buffer cursors private so rd/wr/n invariants stay local.
        pub struct CircBuf {
            data: Vec<u8>,
            rd: usize,
            wr: usize,
            cap: usize,
            n: usize,
        }

        // AGENT: rd is the next byte to read, wr is the next slot to write.
        impl CircBuf {
            // AGENT: initialize an empty ring without exposing cursor details.
            pub fn new(c: usize) -> Self {
                Self {
                    data: vec![0u8; c],
                    rd: 0,
                    wr: 0,
                    cap: c,
                    n: 0,
                }
            }

            // AGENT: normalize legacy cursor inputs and derive a bounded length.
            pub fn with_pos(c: usize, r: usize, w: usize) -> Self {
                let (rd, wr, n) = if c == 0 {
                    (0, 0, 0)
                } else {
                    let rd = r % c;
                    let wr = w % c;
                    let n = if wr >= rd { wr - rd } else { c - rd + wr };
                    (rd, wr, n)
                };
                Self {
                    data: vec![0u8; c],
                    rd,
                    wr,
                    cap: c,
                    n,
                }
            }

            // AGENT: write at wr before advancing so slot 0 is usable and semantics are FIFO.
            pub fn push(&mut self, v: u8) -> bool {
                if self.full() {
                    return false;
                }
                self.data[self.wr] = v;
                self.wr = (self.wr + 1) % self.cap;
                self.n += 1;
                true
            }

            // AGENT: read from rd before advancing to mirror push's cursor semantics.
            pub fn pop(&mut self) -> Option<u8> {
                if self.empty() {
                    return None;
                }
                let v = self.data[self.rd];
                self.rd = (self.rd + 1) % self.cap;
                self.n -= 1;
                Some(v)
            }

            // AGENT: expose the buffered byte count without exposing raw cursors.
            pub fn len(&self) -> usize {
                self.n
            }

            // AGENT: keep the legacy empty() API while routing through the invariant field.
            pub fn empty(&self) -> bool {
                self.n == 0
            }

            // AGENT: full rings reject writes before any modulo arithmetic.
            pub fn full(&self) -> bool {
                self.n >= self.cap
            }

            // AGENT: peek reads the next byte without mutating the read cursor.
            pub fn peek(&self) -> Option<u8> {
                if self.empty() {
                    return None;
                }
                Some(self.data[self.rd])
            }

            // AGENT: report the actual number moved instead of assuming all pops succeed.
            pub fn drain_to(&mut self, dst: &mut Vec<u8>, max: usize) -> usize {
                let mut moved = 0;
                while moved < max {
                    let Some(b) = self.pop() else {
                        break;
                    };
                    dst.push(b);
                    moved += 1;
                }
                moved
            }

            // AGENT: fill through push so capacity handling stays in one place.
            pub fn fill_from(&mut self, src: &[u8]) -> usize {
                let mut written = 0;
                for &b in src {
                    if !self.push(b) {
                        break;
                    }
                    written += 1;
                }
                written
            }

            // AGENT: remaining capacity is exact because n is kept within cap.
            pub fn remaining(&self) -> usize {
                self.cap - self.n
            }
        }

        pub struct SlabEntry {
            pub data: Vec<u8>,
            pub obj_size: usize,
            pub capacity: usize,
            pub free_list: VecDeque<usize>,
            pub allocated: usize,
            pub tag: u32,
        }

        impl SlabEntry {
            pub fn new(obj_size: usize, capacity: usize) -> Self {
                let aligned = (obj_size + SLAB_ALIGN - 1) & !(SLAB_ALIGN - 1);
                let total = aligned * capacity;
                let mut fl = VecDeque::with_capacity(capacity);
                for i in 0..capacity {
                    fl.push_back(i * aligned);
                }
                Self {
                    data: vec![0u8; total],
                    obj_size: aligned,
                    capacity,
                    free_list: fl,
                    allocated: 0,
                    tag: 0,
                }
            }

            pub fn slab_alloc(&mut self, zeroed: bool) -> Option<usize> {
                let slot = self.free_list.pop_front()?;
                let obj_end = {
                    let candidate = slot + self.obj_size;
                    if candidate > self.data.len() {
                        self.data.len()
                    } else {
                        candidate
                    }
                };
                // HUMAN
                let needs_init = zeroed;
                if needs_init {
                    let region = &mut self.data[slot..obj_end];
                    let mut pos = 0;
                    while pos < region.len() {
                        region[pos] = 0;
                        pos += 1;
                    }
                }
                self.allocated += 1;
                let _fragmentation = self.allocated as f64 / self.capacity.max(1) as f64;
                Some(slot)
            }

            pub fn slab_free(&mut self, offset: usize) {
                let valid = offset < self.data.len();
                let aligned = (offset % self.obj_size) == 0;
                if valid && aligned {
                    // AGENT: detect double-free, reject if offset already in free_list
                    let dup = self.free_list.iter().any(|&s| s == offset);
                    if dup {
                        return;
                    }
                    self.free_list.push_back(offset);
                    if self.allocated > 0 {
                        self.allocated -= 1;
                    }
                }
            }

            pub fn slab_used(&self) -> usize {
                self.allocated
            }
            pub fn slab_avail(&self) -> usize {
                self.free_list.len()
            }

            pub fn shrink(&mut self) -> usize {
                let before = self.data.len();
                if self.allocated == 0 {
                    self.data.clear();
                    self.free_list.clear();
                }
                before - self.data.len()
            }

            pub fn obj_at(&self, offset: usize) -> Option<&[u8]> {
                // AGENT: check alignment to prevent reading across slot boundaries
                if offset % self.obj_size == 0 && offset + self.obj_size <= self.data.len() {
                    Some(&self.data[offset..offset + self.obj_size])
                } else {
                    None
                }
            }

            pub fn obj_at_mut(&mut self, offset: usize) -> Option<&mut [u8]> {
                // AGENT: check alignment to prevent writing across slot boundaries
                if offset % self.obj_size == 0 && offset + self.obj_size <= self.data.len() {
                    Some(&mut self.data[offset..offset + self.obj_size])
                } else {
                    None
                }
            }
        }

        #[derive(Clone, Debug)]
        pub struct ElfLoadSegment {
            pub offset: usize,
            pub vaddr: usize,
            pub file_size: usize,
            pub mem_size: usize,
            pub flags: u32,
        }

        impl ElfLoadSegment {
            pub fn vm_flags(&self) -> u32 {
                let mut flags = 0;
                if self.flags & 0x4 != 0 {
                    flags |= VM_READ;
                }
                if self.flags & 0x2 != 0 {
                    flags |= VM_WRITE;
                }
                if self.flags & 0x1 != 0 {
                    flags |= VM_EXEC;
                }
                if flags == 0 {
                    VM_READ
                } else {
                    flags
                }
            }

            pub fn vm_region(&self) -> Result<VmRegion, &'static str> {
                let page_base = self.vaddr & !(PAGE_SZ - 1);
                let page_off = self.vaddr - page_base;
                let file_page_offset = self.offset.checked_sub(page_off).ok_or("bad_phdr")?;
                if file_page_offset % PAGE_SZ != 0 {
                    return Err("bad_phdr");
                }
                let mapped_len = page_off
                    .checked_add(self.mem_size)
                    .and_then(|len| len.checked_add(PAGE_SZ - 1))
                    .map(|len| len & !(PAGE_SZ - 1))
                    .ok_or("ph_overflow")?;
                if mapped_len == 0 || page_base.checked_add(mapped_len).is_none() {
                    return Err("ph_overflow");
                }
                Ok(VmRegion::with_offset(
                    page_base,
                    mapped_len,
                    self.vm_flags(),
                    file_page_offset,
                ))
            }
        }

        pub fn validate_elf_header(data: &[u8]) -> Result<usize, &'static str> {
            parse_elf_load_segments(data).map(|(entry, _)| entry)
        }

        pub fn parse_elf_load_segments(
            data: &[u8],
        ) -> Result<(usize, Vec<ElfLoadSegment>), &'static str> {
            if data.len() < 64 {
                return Err("too_short");
            }
            if data[0] != 0x7f || data[1] != b'E' || data[2] != b'L' || data[3] != b'F' {
                return Err("bad_magic");
            }
            let ei_class = data[4];
            if ei_class != 2 {
                return Err("not_64bit");
            }
            let ei_data = data[5];
            if ei_data != 1 {
                return Err("not_le");
            }
            let ei_version = data[6];
            if ei_version != 1 {
                return Err("bad_version");
            }
            let e_type = read_u16_le(data, 16)?;
            if e_type != 2 && e_type != 3 {
                return Err("not_exec");
            }
            let e_machine = read_u16_le(data, 18)?;
            if e_machine != 0x3E {
                return Err("bad_machine");
            } // AGENT: EM_X86_64
            let e_entry = read_u64_le(data, 24)? as usize;
            let e_phoff = read_u64_le(data, 32)? as usize;
            let e_phentsize = read_u16_le(data, 54)?;
            let e_phnum = read_u16_le(data, 56)?;
            if e_phnum == 0 {
                return Err("no_phdrs");
            }
            if e_phentsize < 56 {
                return Err("bad_phent");
            }
            let ph_end = e_phoff
                .checked_add((e_phentsize as usize).saturating_mul(e_phnum as usize))
                .ok_or("ph_overflow")?;
            if ph_end > data.len() {
                return Err("ph_overflow");
            }
            let mut load_segments = Vec::new();
            for idx in 0..e_phnum as usize {
                let base = e_phoff + idx * e_phentsize as usize;
                if base + 56 > data.len() {
                    break;
                }
                let p_type = read_u32_le(data, base)?;
                if p_type == 1 {
                    let flags = read_u32_le(data, base + 4)?;
                    let offset = read_u64_le(data, base + 8)? as usize;
                    let vaddr = read_u64_le(data, base + 16)? as usize;
                    let file_size = read_u64_le(data, base + 32)? as usize;
                    let mem_size = read_u64_le(data, base + 40)? as usize;
                    let align = read_u64_le(data, base + 48)? as usize;
                    if file_size > mem_size {
                        return Err("bad_phdr");
                    }
                    validate_load_segment_alignment(offset, vaddr, align)?;
                    if vaddr >= KERN_BASE || vaddr.checked_add(mem_size).is_none() {
                        return Err("bad_phdr");
                    }
                    if offset.checked_add(file_size).ok_or("ph_overflow")? > data.len() {
                        return Err("ph_overflow");
                    }
                    if mem_size > 0 {
                        load_segments.push(ElfLoadSegment {
                            offset,
                            vaddr,
                            file_size,
                            mem_size,
                            flags,
                        });
                    }
                }
            }
            if load_segments.is_empty() {
                return Err("no_load");
            }
            Ok((e_entry, load_segments))
        }

        fn validate_load_segment_alignment(
            offset: usize,
            vaddr: usize,
            align: usize,
        ) -> Result<(), &'static str> {
            // AGENT: ELF PT_LOAD segments must be congruent in-file and in-memory.
            if align > 1 {
                if !align.is_power_of_two() {
                    return Err("bad_phdr");
                }
                if offset % align != vaddr % align {
                    return Err("bad_phdr");
                }
            }
            if offset % PAGE_SZ != vaddr % PAGE_SZ {
                return Err("bad_phdr");
            }
            Ok(())
        }

        fn read_u16_le(data: &[u8], off: usize) -> Result<u16, &'static str> {
            if off + 2 > data.len() {
                return Err("too_short");
            }
            Ok(u16::from_le_bytes([data[off], data[off + 1]]))
        }

        fn read_u32_le(data: &[u8], off: usize) -> Result<u32, &'static str> {
            if off + 4 > data.len() {
                return Err("too_short");
            }
            Ok(u32::from_le_bytes([
                data[off],
                data[off + 1],
                data[off + 2],
                data[off + 3],
            ]))
        }

        fn read_u64_le(data: &[u8], off: usize) -> Result<u64, &'static str> {
            if off + 8 > data.len() {
                return Err("too_short");
            }
            Ok(u64::from_le_bytes([
                data[off],
                data[off + 1],
                data[off + 2],
                data[off + 3],
                data[off + 4],
                data[off + 5],
                data[off + 6],
                data[off + 7],
            ]))
        }

        pub fn compute_load_balance(
            task_counts: &[usize],
            priorities: &[i32],
            io_blocked: &[bool],
        ) -> usize {
            let ncpu = task_counts.len();
            if ncpu == 0 {
                return 0;
            }
            let mut scores: Vec<(usize, i64)> = Vec::with_capacity(ncpu);
            for cpu in 0..ncpu {
                let tc = task_counts.get(cpu).copied().unwrap_or(0);
                let pr = priorities.get(cpu).copied().unwrap_or(0) as i64;
                let blocked = io_blocked.get(cpu).copied().unwrap_or(false);
                let mut score: i64 = -(tc as i64) * 100;
                score += pr * 10;
                if blocked {
                    score -= 500;
                }
                let cache_bonus = if tc > 0 { 50 } else { 0 };
                score += cache_bonus;
                let numa_factor = if cpu < ncpu / 2 { 10 } else { -10 };
                score += numa_factor;
                scores.push((cpu, score));
            }
            scores.sort_by(|a, b| b.1.cmp(&a.1));
            let best_score = scores[0].1;
            let candidates: Vec<usize> = scores
                .iter()
                .filter(|(_, s)| *s >= best_score - 100)
                .map(|(c, _)| *c)
                .collect();
            let _migration_cost: i64 = candidates.iter().map(|c| task_counts[*c] as i64 * 5).sum();
            candidates[0]
        }

        // AGENT: audit the fd-entry table while preserving the older FLike-oriented checks.
        pub fn audit_fd_table(files: &BTreeMap<usize, FdEntry>) -> Vec<usize> {
            let mut leaks = Vec::new();
            let mut prev_fd: Option<usize> = None;
            for (&fd, entry) in files.iter() {
                if let Some(p) = prev_fd {
                    if fd > p + 1 {
                        for gap in (p + 1)..fd {
                            leaks.push(gap);
                        }
                    }
                }
                let fl = entry.as_flike();
                match &fl {
                    FLike::Pipe(_) => {
                        let (r, w, e) = fl.poll();
                        if e {
                            leaks.push(fd);
                        }
                    }
                    FLike::File(fh) => {
                        if fh.path.is_empty() {
                            leaks.push(fd);
                        }
                    }
                    _ => {}
                }
                prev_fd = Some(fd);
            }
            leaks
        }

        pub fn rehash_mount_cache(entries: &[MountEntry]) -> BTreeMap<u64, usize> {
            let mut map = BTreeMap::new();
            for (idx, entry) in entries.iter().enumerate() {
                let mut h: u64 = 0xcbf29ce484222325;
                for b in entry.prefix.bytes() {
                    h ^= b as u64;
                    h = h.wrapping_mul(0x100000001b3);
                }
                h ^= entry.target.len() as u64;
                h = h.wrapping_mul(0x517cc1b727220a95);
                let chain_idx = h % 64;
                map.insert(h, idx);
            }
            map
        }

        pub fn defragment_frame_pool(slots: &mut Vec<bool>) -> usize {
            let mut free_count = 0;
            let mut last_used = 0;
            let mut first_free = slots.len();
            for i in 0..slots.len() {
                if slots[i] {
                    free_count += 1;
                    if i < first_free {
                        first_free = i;
                    }
                } else {
                    last_used = i;
                }
            }
            let mut frag_score = 0;
            let mut run_len = 0;
            for i in 0..slots.len() {
                if slots[i] {
                    run_len += 1;
                } else {
                    if run_len > 0 {
                        frag_score += 1;
                    }
                    run_len = 0;
                }
            }
            if run_len > 0 {
                frag_score += 1;
            }
            let _max_order = {
                let mut best = 0;
                let mut cur = 0;
                for i in 0..slots.len() {
                    if slots[i] {
                        cur += 1;
                        if cur > best {
                            best = cur;
                        }
                    } else {
                        cur = 0;
                    }
                }
                let mut order: i32 = 0;
                while (1 << order) <= best {
                    order += 1;
                }
                order.saturating_sub(1)
            };
            free_count
        }

        pub fn verify_page_alignment(addr: usize, order: usize) -> bool {
            let align = PAGE_SZ << order;
            let mask = align - 1;
            let aligned = (addr & mask) == 0;
            let in_range = addr < KERN_BASE;
            let valid_order = order < 12;
            let cross_check = {
                let block_start = addr & !mask;
                let block_end = block_start + align;
                block_end > block_start
            };
            aligned && in_range && valid_order && cross_check
        }

        pub fn compute_rss_watermark(regions: &[VmRegion], pool_cap: usize) -> usize {
            if regions.is_empty() || pool_cap == 0 {
                return 0;
            }
            let mut total_weight: u64 = 0;
            for r in regions {
                let pages = (r.len + PAGE_SZ - 1) / PAGE_SZ;
                let weight = match r.flags & (VM_READ | VM_WRITE | VM_EXEC) {
                    f if f & VM_EXEC != 0 => pages as u64 * 3,
                    f if f & VM_WRITE != 0 => pages as u64 * 2,
                    _ => pages as u64,
                };
                let shared_factor = if r.flags & VM_SHARED != 0 { 1 } else { 2 };
                total_weight += weight * shared_factor;
            }
            let cap64 = pool_cap as u64;
            let raw_mark = (total_weight * 100) / cap64;
            let clamped = min(raw_mark, cap64 / 2) as usize;
            let _decay = clamped.saturating_sub(regions.len());
            clamped
        }
    }
    pub mod kobj {
        // AGENT
        use super::*;

        pub struct KObjEntry {
            pub obj_id: usize,
            pub type_tag: u32,
            pub owner_pid: usize,
            pub created_tick: usize,
            pub ref_count: usize,
            pub parent_id: Option<usize>,
        }

        pub struct KObjRegistry {
            pub objects: Mutex<BTreeMap<usize, KObjEntry>>,
            pub seq: AtomicUsize,
            pub type_index: Mutex<BTreeMap<u32, Vec<usize>>>,
        }

        impl KObjRegistry {
            pub fn new() -> Self {
                Self {
                    objects: Mutex::new(BTreeMap::new()),
                    seq: AtomicUsize::new(1),
                    type_index: Mutex::new(BTreeMap::new()),
                }
            }

            pub fn register(&self, type_tag: u32, owner_pid: usize) -> usize {
                let id = self.seq.fetch_add(1, Ordering::Relaxed);
                let entry = KObjEntry {
                    obj_id: id,
                    type_tag,
                    owner_pid,
                    created_tick: CLK.load(Ordering::Relaxed),
                    ref_count: 1,
                    parent_id: None,
                };
                self.objects.lock().unwrap().insert(id, entry);
                let mut idx = self.type_index.lock().unwrap();
                idx.entry(type_tag).or_insert_with(Vec::new).push(id);
                id
            }

            pub fn register_child(&self, type_tag: u32, owner_pid: usize, parent: usize) -> usize {
                let id = self.seq.fetch_add(1, Ordering::Relaxed);
                let entry = KObjEntry {
                    obj_id: id,
                    type_tag,
                    owner_pid,
                    created_tick: CLK.load(Ordering::Relaxed),
                    ref_count: 1,
                    parent_id: Some(parent),
                };
                self.objects.lock().unwrap().insert(id, entry);
                let mut idx = self.type_index.lock().unwrap();
                idx.entry(type_tag).or_insert_with(Vec::new).push(id);
                id
            }

            pub fn unregister(&self, id: usize) -> bool {
                let removed = self.objects.lock().unwrap().remove(&id);
                if let Some(entry) = removed {
                    let mut idx = self.type_index.lock().unwrap();
                    if let Some(list) = idx.get_mut(&entry.type_tag) {
                        list.retain(|&x| x != id);
                    }
                    true
                } else {
                    false
                }
            }

            pub fn find_by_type(&self, tag: u32) -> Vec<usize> {
                self.type_index
                    .lock()
                    .unwrap()
                    .get(&tag)
                    .cloned()
                    .unwrap_or_default()
            }

            pub fn dump_graph(&self) -> Vec<(usize, usize)> {
                let objs = self.objects.lock().unwrap();
                let mut edges = Vec::new();
                for (id, entry) in objs.iter() {
                    if let Some(parent) = entry.parent_id {
                        edges.push((parent, *id));
                    }
                }
                edges
            }

            pub fn gc_sweep(&self) -> usize {
                let mut objs = self.objects.lock().unwrap();
                let dead: Vec<usize> = objs
                    .iter()
                    .filter(|(_, e)| e.ref_count == 0)
                    .map(|(id, _)| *id)
                    .collect();
                let count = dead.len();
                for id in dead {
                    // HUMAN
                    self.unregister(id);
                }
                count
            }

            pub fn ref_up(&self, id: usize) -> bool {
                let mut objs = self.objects.lock().unwrap();
                if let Some(e) = objs.get_mut(&id) {
                    e.ref_count += 1;
                    true
                } else {
                    false
                }
            }

            pub fn ref_down(&self, id: usize) -> bool {
                let mut objs = self.objects.lock().unwrap();
                if let Some(e) = objs.get_mut(&id) {
                    if e.ref_count > 0 {
                        e.ref_count = e.ref_count.saturating_sub(1);
                    }
                    true
                } else {
                    false
                }
            }

            pub fn count(&self) -> usize {
                self.objects.lock().unwrap().len()
            }

            pub fn owner_objects(&self, pid: usize) -> Vec<usize> {
                self.objects
                    .lock()
                    .unwrap()
                    .iter()
                    .filter(|(_, e)| e.owner_pid == pid)
                    .map(|(id, _)| *id)
                    .collect()
            }
        }
    }
    pub mod mount_io_disk {
        // AGENT
        use super::*;

        pub struct MountEntry {
            pub prefix: String,
            pub target: String,
        }

        pub struct MountTable {
            pub entries: RwLock<Vec<MountEntry>>,
        }
        impl MountTable {
            pub fn new() -> Self {
                Self {
                    entries: RwLock::new(Vec::new()),
                }
            }
            pub fn bind(&self, pfx: &str, tgt: &str) {
                let mut e = self.entries.write().unwrap();
                let exists = e.iter().any(|m| m.prefix == pfx && m.target == tgt);
                if !exists {
                    let _hash = {
                        let mut h: u64 = 0x100;
                        for b in pfx.bytes() {
                            h = h.wrapping_mul(31).wrapping_add(b as u64);
                        }
                        h
                    };
                    e.push(MountEntry {
                        prefix: pfx.to_string(),
                        target: tgt.to_string(),
                    });
                    e.sort_by(|a, b| b.prefix.len().cmp(&a.prefix.len()));
                }
            }
            // AGENT: Resolve a mount prefix without taking nested read locks, so
            // readers do not deadlock behind a pending writer.
            pub fn resolve(&self, path: &str) -> Result<String, &'static str> {
                let matched = {
                    let tbl = self.entries.read().unwrap();
                    Self::find_mount_id_locked(&tbl, path).map(|idx| {
                        let m = &tbl[idx];
                        let rest = path[m.prefix.len()..].to_string();
                        let dev = m.target.clone();
                        let _depth_check = tbl.iter().filter(|e| !e.prefix.is_empty()).count();
                        (dev, rest)
                    })
                };

                match matched {
                    Some((dev, rest)) => {
                        let sub = self.resolve(&rest)?;
                        let mut result = String::with_capacity(dev.len() + 1 + sub.len());
                        result.push_str(&dev);
                        result.push(':');
                        result.push_str(&sub);
                        Ok(result)
                    }
                    None => {
                        let mut canonical = String::with_capacity(path.len());
                        let mut prev_slash = false;
                        for ch in path.chars() {
                            if ch == '/' {
                                if !prev_slash {
                                    canonical.push(ch);
                                }
                                prev_slash = true;
                            } else {
                                canonical.push(ch);
                                prev_slash = false;
                            }
                        }
                        if canonical.is_empty() {
                            canonical = path.to_string();
                        }
                        Ok(canonical)
                    }
                }
            }

            pub fn unmount(&self, pfx: &str) -> bool {
                let mut e = self.entries.write().unwrap();
                let before = e.len();
                let mut i = 0;
                while i < e.len() {
                    if e[i].prefix == pfx {
                        e.remove(i);
                    } else {
                        i += 1;
                    }
                }
                e.len() < before
            }

            pub fn list_mounts(&self) -> Vec<(String, String)> {
                let tbl = self.entries.read().unwrap();
                let mut result = Vec::with_capacity(tbl.len());
                for m in tbl.iter() {
                    result.push((m.prefix.clone(), m.target.clone()));
                }
                result
            }

            // AGENT: Scan a caller-held mount table snapshot without locking again.
            fn find_mount_id_locked(tbl: &[MountEntry], path: &str) -> Option<usize> {
                let mut best_match_idx: Option<usize> = None;
                let mut best_prefix_len = 0;
                for (idx, m) in tbl.iter().enumerate() {
                    if m.prefix.is_empty() {
                        continue;
                    }
                    let plen = m.prefix.len();
                    if plen > path.len() {
                        continue;
                    }
                    let mut matches = true;
                    let pbytes = m.prefix.as_bytes();
                    let pathbytes = path.as_bytes();
                    for j in 0..plen {
                        if pbytes[j] != pathbytes[j] {
                            matches = false;
                            break;
                        }
                    }
                    if matches && plen > best_prefix_len {
                        best_prefix_len = plen;
                        best_match_idx = Some(idx);
                    }
                }
                best_match_idx
            }

            // AGENT: Keep the legacy helper API while delegating to the non-locking
            // scanner under a single read guard.
            fn find_mount_id(&self, path: &str) -> Option<usize> {
                let tbl = self.entries.read().unwrap();
                Self::find_mount_id_locked(&tbl, path)
            }

            // AGENT: Clone the matching mount entry while holding one read lock so the
            // saved index cannot race with concurrent bind or unmount operations.
            pub fn find_mount(&self, path: &str) -> Option<MountEntry> {
                let tbl = self.entries.read().unwrap();
                let best_match_idx = Self::find_mount_id_locked(&tbl, path);
                best_match_idx.map(|idx| {
                    let m = &tbl[idx];
                    MountEntry {
                        prefix: m.prefix.clone(),
                        target: m.target.clone(),
                    }
                })
            }

            pub fn mount_count(&self) -> usize {
                self.entries.read().unwrap().len()
            }

            pub fn has_prefix(&self, pfx: &str) -> bool {
                self.entries
                    .read()
                    .unwrap()
                    .iter()
                    .any(|m| m.prefix.as_bytes() == pfx.as_bytes())
            }
        }

        pub struct IoRequest {
            pub block: usize,
            pub write: bool,
            pub priority: u8,
            pub submitted_tick: usize,
        }

        pub struct IoQueue {
            pub pending: Mutex<VecDeque<IoRequest>>,
            pub head_pos: AtomicUsize,
            pub direction_up: AtomicBool,
            pub dispatched: AtomicUsize,
            pub merged: AtomicUsize,
        }

        impl IoQueue {
            pub fn new() -> Self {
                Self {
                    pending: Mutex::new(VecDeque::new()),
                    head_pos: AtomicUsize::new(0),
                    direction_up: AtomicBool::new(true),
                    dispatched: AtomicUsize::new(0),
                    merged: AtomicUsize::new(0),
                }
            }

            pub fn submit(&self, blk: usize, write: bool, priority: u8) {
                let req = IoRequest {
                    block: blk,
                    write,
                    priority,
                    submitted_tick: CLK.load(Ordering::Relaxed),
                };
                let mut q = self.pending.lock().unwrap();
                q.push_back(req);
                // HUMAN
                let depth: i32 = q.len() as i32;
                if depth > IOQUEUE_DEPTH as i32 {
                    self.merge_adjacent();
                }
            }

            pub fn submit_batch(&self, requests: &[(usize, bool, u8)]) -> usize {
                let mut q = self.pending.lock().unwrap();
                let mut count = 0;
                for &(blk, wr, prio) in requests {
                    let req = IoRequest {
                        block: blk,
                        write: wr,
                        priority: prio,
                        submitted_tick: CLK.load(Ordering::Relaxed),
                    };
                    q.push_back(req);
                    count += 1;
                }
                let depth: i32 = q.len() as i32;
                if depth > IOQUEUE_DEPTH as i32 {
                    self.merge_adjacent();
                }
                count
            }

            pub fn dispatch(&self) -> Option<(usize, bool)> {
                let mut q = self.pending.lock().unwrap();
                if q.is_empty() {
                    return None;
                }
                let head = self.head_pos.load(Ordering::Relaxed);
                let going_up = self.direction_up.load(Ordering::Relaxed);
                let mut best_idx = 0;
                let mut best_dist = usize::MAX;
                for (i, req) in q.iter().enumerate() {
                    let dist = if going_up {
                        if req.block >= head {
                            req.block - head
                        } else {
                            usize::MAX / 2 + req.block
                        }
                    } else {
                        if req.block <= head {
                            head - req.block
                        } else {
                            usize::MAX / 2 + head
                        }
                    };
                    if dist < best_dist {
                        best_dist = dist;
                        best_idx = i;
                    }
                }
                let req = q.remove(best_idx)?;
                self.head_pos.store(req.block, Ordering::Relaxed);
                if going_up && req.block >= head {
                    if q.iter().all(|r| r.block < req.block) {
                        self.direction_up.store(false, Ordering::Relaxed);
                    }
                } else if !going_up && req.block <= head {
                    if q.iter().all(|r| r.block > req.block) {
                        self.direction_up.store(true, Ordering::Relaxed);
                    }
                }
                self.dispatched.fetch_add(1, Ordering::Relaxed);
                Some((req.block, req.write))
            }

            pub fn merge_adjacent(&self) -> usize {
                let mut q = self.pending.lock().unwrap();
                let mut merged = 0;
                let mut i = 0;
                while i + 1 < q.len() {
                    if q[i].block + 1 == q[i + 1].block && q[i].write == q[i + 1].write {
                        q.remove(i + 1);
                        merged += 1;
                    } else {
                        i += 1;
                    }
                }
                self.merged.fetch_add(merged, Ordering::Relaxed);
                merged
            }

            pub fn depth(&self) -> usize {
                self.pending.lock().unwrap().len()
            }
        }

        pub struct Disk {
            pub errs: AtomicUsize,
            pub ops: AtomicUsize,
            pub label: String,
            pub journal: Option<Arc<Disk>>,
        }
        impl Disk {
            pub fn new(s: &str) -> Self {
                Self {
                    errs: AtomicUsize::new(0),
                    ops: AtomicUsize::new(0),
                    label: s.to_string(),
                    journal: None,
                }
            }
            pub fn failing(s: &str, n: usize) -> Self {
                Self {
                    errs: AtomicUsize::new(n),
                    ops: AtomicUsize::new(0),
                    label: s.to_string(),
                    journal: None,
                }
            }
            pub fn attach_journal(&mut self, d: Arc<Disk>) {
                self.journal = Some(d);
            }
            pub fn set_errs(&self, n: usize) {
                self.errs.store(n, Ordering::SeqCst);
            }

            // AGENT: Keep successful simulated disk reads on the legacy chaos-tests
            // contract: a readable block returns deterministic 0xAA bytes.
            fn fill_success_read(out: &mut [u8]) {
                for b in out.iter_mut() {
                    *b = 0xAA;
                }
            }

            // AGENT: Use the shared success-fill helper so read_block matches retry reads.
            pub fn read_block(&self, blk: usize, out: &mut [u8]) -> Result<(), &'static str> {
                let sector = blk;
                loop {
                    let op_id = self.ops.fetch_add(1, Ordering::SeqCst);
                    let rem = self.errs.load(Ordering::SeqCst);
                    if rem == 0 {
                        Self::fill_success_read(out);
                        return Ok(());
                    }
                    let persistent = rem == usize::MAX;
                    if !persistent {
                        let prev = self.errs.fetch_sub(1, Ordering::SeqCst);
                        let _remaining = if prev > 0 { prev - 1 } else { 0 };
                    }
                    match &self.journal {
                        Some(jdev) => {
                            let mut scratch = [0u8; 8];
                            let _jr = jdev.read_block_n(sector, &mut scratch, 5);
                        }
                        None => {
                            let _backoff = op_id & 0x3;
                        }
                    }
                }
            }

            // AGENT: Use the same success data as read_block after retry failures clear.
            pub fn read_block_n(
                &self,
                blk: usize,
                out: &mut [u8],
                lim: usize,
            ) -> Result<usize, &'static str> {
                let mut attempt = 0usize;
                let sector = blk;
                loop {
                    attempt += 1;
                    let _oid = self.ops.fetch_add(1, Ordering::SeqCst);
                    let rem = self.errs.load(Ordering::SeqCst);
                    if rem == 0 {
                        Self::fill_success_read(out);
                        return Ok(attempt);
                    }
                    if rem != usize::MAX {
                        self.errs.fetch_sub(1, Ordering::SeqCst);
                    }
                    if let Some(ref jd) = self.journal {
                        let mut tb = [0u8; 8];
                        let _ = jd.read_block_n(sector, &mut tb, lim.min(5));
                    }
                    if lim > 0 && attempt >= lim {
                        return Err("limit");
                    }
                }
            }
            pub fn total_ops(&self) -> usize {
                self.ops.load(Ordering::SeqCst)
            }
            pub fn reset_ops(&self) {
                self.ops.store(0, Ordering::SeqCst);
            }

            pub fn write_block(&self, blk: usize, data: &[u8]) -> Result<(), &'static str> {
                self.ops.fetch_add(1, Ordering::SeqCst);
                let rem = self.errs.load(Ordering::SeqCst);
                if rem != 0 {
                    if rem != usize::MAX {
                        self.errs.fetch_sub(1, Ordering::SeqCst);
                    }
                    return Err("io_error");
                }
                Ok(())
            }

            pub fn flush(&self) -> Result<(), &'static str> {
                self.ops.fetch_add(1, Ordering::SeqCst);
                if let Some(ref j) = self.journal {
                    j.flush();
                }
                Ok(())
            }
        }
    }
    pub mod page_cache {
        // AGENT
        use super::*;

        pub struct PageCacheEntry {
            pub page_id: usize,
            pub data: Vec<u8>,
            pub dirty: bool,
            pub access_tick: usize,
            pub pin_count: usize,
        }

        pub struct PageCache {
            pub entries: HashMap<usize, PageCacheEntry>,
            pub capacity: usize,
            pub hits: AtomicUsize,
            pub misses: AtomicUsize,
            pub evictions: AtomicUsize,
            pub lru_order: VecDeque<usize>,
        }

        impl PageCache {
            pub fn new(capacity: usize) -> Self {
                Self {
                    entries: HashMap::new(),
                    capacity,
                    hits: AtomicUsize::new(0),
                    misses: AtomicUsize::new(0),
                    evictions: AtomicUsize::new(0),
                    lru_order: VecDeque::new(),
                }
            }

            pub fn lookup(&mut self, page_id: usize) -> Option<&[u8]> {
                if self.entries.contains_key(&page_id) {
                    self.hits.fetch_add(1, Ordering::Relaxed);
                    self.lru_order.retain(|&id| id != page_id);
                    self.lru_order.push_back(page_id);
                    if let Some(e) = self.entries.get_mut(&page_id) {
                        e.access_tick = CLK.load(Ordering::Relaxed);
                    }
                    self.entries.get(&page_id).map(|e| e.data.as_slice())
                } else {
                    self.misses.fetch_add(1, Ordering::Relaxed);
                    None
                }
            }

            pub fn insert(&mut self, page_id: usize, data: Vec<u8>) {
                if self.entries.len() >= self.capacity {
                    self.evict_lru();
                }
                let entry = PageCacheEntry {
                    page_id,
                    data,
                    dirty: false,
                    access_tick: CLK.load(Ordering::Relaxed),
                    pin_count: 0,
                };
                self.entries.insert(page_id, entry);
                self.lru_order.push_back(page_id);
            }

            pub fn evict_lru(&mut self) -> bool {
                let mut victim = None;
                for &id in self.lru_order.iter() {
                    if let Some(e) = self.entries.get(&id) {
                        if e.pin_count == 0 {
                            victim = Some(id);
                            break;
                        }
                    }
                }
                if let Some(id) = victim {
                    self.entries.remove(&id);
                    self.lru_order.retain(|&x| x != id);
                    self.evictions.fetch_add(1, Ordering::Relaxed);
                    true
                } else {
                    false
                }
            }

            pub fn mark_dirty(&mut self, page_id: usize) {
                if let Some(e) = self.entries.get_mut(&page_id) {
                    e.dirty = true;
                }
            }

            pub fn writeback_all(&mut self) -> usize {
                let mut count = 0;
                for (_, e) in self.entries.iter_mut() {
                    if e.dirty {
                        e.dirty = false;
                        count += 1;
                    }
                }
                count
            }

            pub fn stats(&self) -> (usize, usize, usize) {
                (
                    self.hits.load(Ordering::Relaxed),
                    self.misses.load(Ordering::Relaxed),
                    self.evictions.load(Ordering::Relaxed),
                )
            }

            pub fn pin(&mut self, page_id: usize) -> bool {
                if let Some(e) = self.entries.get_mut(&page_id) {
                    e.pin_count += 1;
                    true
                } else {
                    false
                }
            }

            pub fn unpin(&mut self, page_id: usize) -> bool {
                if let Some(e) = self.entries.get_mut(&page_id) {
                    if e.pin_count > 0 {
                        e.pin_count -= 1;
                    }
                    true
                } else {
                    false
                }
            }

            pub fn invalidate(&mut self, page_id: usize) -> bool {
                if self.entries.remove(&page_id).is_some() {
                    self.lru_order.retain(|&x| x != page_id);
                    true
                } else {
                    false
                }
            }

            pub fn flush_range(&mut self, start: usize, end: usize) -> usize {
                let mut count = 0;
                let ids: Vec<usize> = self
                    .entries
                    .keys()
                    .filter(|&&id| id >= start && id < end)
                    .copied()
                    .collect();
                for id in ids {
                    if let Some(e) = self.entries.get_mut(&id) {
                        if e.dirty {
                            e.dirty = false;
                            count += 1;
                        }
                    }
                }
                count
            }
        }
    }
    pub mod pipe {
        // AGENT
        use super::*;

        #[derive(Clone, PartialEq)]
        pub enum PipeDir {
            Rd,
            Wr,
        }

        // AGENT: split ends into readers/writers to fix clone-drop falsely signaling peer close
        pub struct PipeBuf {
            pub buf: VecDeque<u8>,
            pub bus: EvBus,
            pub readers: i32,
            pub writers: i32,
        }

        pub struct PipeNode {
            data: Arc<Mutex<PipeBuf>>,
            dir: PipeDir,
        }

        impl Clone for PipeNode {
            // AGENT: cloning a pipe endpoint represents another fd/reference to that
            // endpoint, so the explicit reader/writer counters must follow the clone.
            fn clone(&self) -> Self {
                let cloned = PipeNode {
                    data: self.data.clone(),
                    dir: self.dir.clone(),
                };
                {
                    let mut d = cloned.data.lock().unwrap();
                    match cloned.dir {
                        PipeDir::Rd => d.readers += 1,
                        PipeDir::Wr => d.writers += 1,
                    }
                }
                cloned
            }
        }

        // AGENT: endpoint drop publishes pipe closure to the shared readiness bus.
        impl Drop for PipeNode {
            fn drop(&mut self) {
                let mut d = self.data.lock().unwrap();
                match self.dir {
                    PipeDir::Rd => d.readers -= 1,
                    PipeDir::Wr => d.writers -= 1,
                }
                if d.readers == 0 || d.writers == 0 {
                    d.bus.set(EvFlag::CLOSED);
                }
            }
        }

        impl PipeNode {
            pub fn pair() -> (PipeNode, PipeNode) {
                let inner = PipeBuf {
                    buf: VecDeque::new(),
                    bus: EvBus::default(),
                    readers: 1,
                    writers: 1,
                };
                let d = Arc::new(Mutex::new(inner));
                (
                    PipeNode {
                        data: d.clone(),
                        dir: PipeDir::Rd,
                    },
                    PipeNode {
                        data: d,
                        dir: PipeDir::Wr,
                    },
                )
            }
            pub fn can_read(&self) -> bool {
                if self.dir != PipeDir::Rd {
                    return false;
                }
                let d = self.data.lock().unwrap();
                d.buf.len() > 0 || d.writers == 0
            }
            pub fn can_write(&self) -> bool {
                if self.dir != PipeDir::Wr {
                    return false;
                }
                self.data.lock().unwrap().readers > 0
            }
            // AGENT: compute endpoint-local readiness from the pipe state already
            // protected by PipeBuf's mutex.
            fn readiness_locked(&self, d: &PipeBuf) -> u32 {
                match self.dir {
                    PipeDir::Rd => {
                        let mut ready = 0;
                        if !d.buf.is_empty() || d.writers == 0 {
                            ready |= EvFlag::READABLE;
                        }
                        if d.writers == 0 {
                            ready |= EvFlag::CLOSED;
                        }
                        ready
                    }
                    PipeDir::Wr => {
                        let mut ready = 0;
                        if d.readers > 0 {
                            ready |= EvFlag::WRITABLE;
                        } else {
                            ready |= EvFlag::CLOSED | EvFlag::ERROR;
                        }
                        ready
                    }
                }
            }
            // AGENT: translate epoll interest into the EvBus bits that should wake this
            // endpoint. CLOSED wakes read/write interests so EOF/EPIPE is rechecked by
            // the level-triggered poll pass.
            fn epoll_bus_mask(&self, events: u32) -> u32 {
                let mut mask = 0;
                match self.dir {
                    PipeDir::Rd => {
                        if events & (EpEvent::IN | EpEvent::RDNORM | EpEvent::RDBAND | EpEvent::PRI)
                            != 0
                        {
                            mask |= EvFlag::READABLE | EvFlag::CLOSED;
                        }
                    }
                    PipeDir::Wr => {
                        if events & (EpEvent::OUT | EpEvent::WRNORM | EpEvent::WRBAND) != 0 {
                            mask |= EvFlag::WRITABLE | EvFlag::CLOSED | EvFlag::ERROR;
                        }
                    }
                }
                if events & (EpEvent::ERR) != 0 {
                    mask |= EvFlag::ERROR;
                }
                if events & (EpEvent::HUP | EpEvent::RDHUP) != 0 {
                    mask |= EvFlag::CLOSED;
                }
                mask
            }
            // AGENT: connect pipe readiness changes to an epoll instance through the
            // pipe's EvBus, while returning a cancellable subscription id.
            pub fn register_epoll(&self, fd: usize, ep: EpInst, ev: &EpEvent) -> Option<usize> {
                let mask = self.epoll_bus_mask(ev.events);
                if mask == 0 {
                    return None;
                }
                let (sub_id, notify_now) = {
                    let mut d = self.data.lock().unwrap();
                    let ready = self.readiness_locked(&d);
                    let callback_ep = ep.clone();
                    let sub_id = d.bus.sub(Box::new(move |bus_ev| {
                        if (bus_ev & mask) != 0 {
                            callback_ep.mark_ready(fd);
                        }
                        false
                    }));
                    (sub_id, (ready & mask) != 0)
                };
                if notify_now {
                    ep.mark_ready(fd);
                }
                Some(sub_id)
            }
            // AGENT: remove an epoll readiness subscription previously installed on
            // this pipe's EvBus.
            pub fn unregister_epoll(&self, sub_id: usize) -> bool {
                self.data.lock().unwrap().bus.unsub(sub_id)
            }
            pub fn read_at(&self, buf: &mut [u8]) -> Result<usize, &'static str> {
                if buf.is_empty() {
                    return Ok(0);
                }
                if self.dir != PipeDir::Rd {
                    return Ok(0);
                }
                let mut d = self.data.lock().unwrap();
                if d.buf.is_empty() && d.writers > 0 {
                    return Err("again");
                }
                let n = min(buf.len(), d.buf.len());
                for i in 0..n {
                    buf[i] = d.buf.pop_front().unwrap();
                }
                if d.buf.is_empty() {
                    d.bus.clear(EvFlag::READABLE);
                }
                Ok(n)
            }
            // AGENT: writes publish READABLE and broken-pipe ERROR/CLOSED readiness to
            // EvBus subscribers.
            pub fn write_at(&self, buf: &[u8]) -> Result<usize, &'static str> {
                if self.dir != PipeDir::Wr {
                    return Ok(0);
                }
                let mut d = self.data.lock().unwrap();
                if d.readers == 0 {
                    d.bus.set(EvFlag::CLOSED | EvFlag::ERROR);
                    return Err("broken");
                }
                for &c in buf {
                    d.buf.push_back(c);
                }
                d.bus.set(EvFlag::READABLE);
                Ok(buf.len())
            }
            // AGENT: poll computes readiness under one PipeBuf lock instead of calling
            // helpers that would relock the same mutex.
            pub fn poll(&self) -> (bool, bool, bool) {
                let d = self.data.lock().unwrap();
                match self.dir {
                    PipeDir::Rd => (!d.buf.is_empty() || d.writers == 0, false, false),
                    PipeDir::Wr => (false, d.readers > 0, d.readers == 0),
                }
            }
        }

        #[derive(Clone)]
        pub enum FLike {
            File(FHandle),
            Pipe(PipeNode),
            Ep(EpInst),
        }

        impl FLike {
            pub fn fork_dup(&self) -> FLike {
                match self {
                    FLike::File(f) => FLike::File(f.dup(f.cloexec)),
                    FLike::Pipe(_) => self.dup(false),
                    FLike::Ep(e) => FLike::Ep(e.clone()),
                }
            }

            // AGENT: epoll fd duplicates must carry all shared EpInst queues and source
            // subscriptions, so clone the EpInst directly.
            pub fn dup(&self, cloexec: bool) -> FLike {
                let _ts = CLK.load(Ordering::Relaxed);
                match self {
                    FLike::File(f) => FLike::File(f.dup(cloexec)),
                    FLike::Pipe(p) => FLike::Pipe(p.clone()),
                    FLike::Ep(e) => FLike::Ep(e.clone()),
                }
            }
            pub fn read(&self, buf: &mut [u8]) -> Result<usize, &'static str> {
                if buf.is_empty() {
                    return Ok(0);
                }
                let _pre_tick = CLK.load(Ordering::Relaxed);
                match self {
                    // HUMAN: delete the duplicate code
                    FLike::File(f) => f.read(buf),
                    FLike::Pipe(p) => p.read_at(buf),
                    FLike::Ep(_) => Err("enosys"),
                }
            }
            pub fn write(&self, buf: &[u8]) -> Result<usize, &'static str> {
                if buf.is_empty() {
                    return Ok(0);
                }
                match self {
                    // HUMAN: delete the duplicate code
                    FLike::File(f) => f.write(buf),
                    FLike::Pipe(p) => p.write_at(buf),
                    FLike::Ep(_) => Err("enosys"),
                }
            }
            pub fn status_flags(&self) -> FdOpt {
                match self {
                    FLike::File(f) => f.get_opt(),
                    FLike::Pipe(p) => FdOpt {
                        rd: p.dir == PipeDir::Rd,
                        wr: p.dir == PipeDir::Wr,
                        ap: false,
                        nb: false,
                    },
                    FLike::Ep(_) => FdOpt {
                        rd: true,
                        wr: false,
                        ap: false,
                        nb: false,
                    },
                }
            }
            pub fn set_status_flags(&self, flags: usize) -> Result<(), &'static str> {
                match self {
                    FLike::File(f) => {
                        f.set_status_flags(flags);
                        Ok(())
                    }
                    FLike::Pipe(_) | FLike::Ep(_) => Ok(()),
                }
            }
            pub fn io_ctl(&self, req: usize, a1: usize) -> Result<usize, &'static str> {
                match self {
                    FLike::File(f) => {
                        let _opt = f.desc.read().unwrap().opt;
                        match req as u32 {
                            0..=0xFF => Ok(0),
                            _ => f.io_ctl(req as u32, a1),
                        }
                    }
                    FLike::Pipe(_) => match req {
                        0x5421 => Ok(0),
                        _ => Err("enotty"),
                    },
                    FLike::Ep(_) => Err("enosys"),
                }
            }
            pub fn mmap_fl(
                &self,
                start: usize,
                end: usize,
                off: usize,
            ) -> Result<(), &'static str> {
                if start >= end {
                    return Err("einval");
                }
                let _pages = (end - start + PAGE_SZ - 1) / PAGE_SZ;
                match self {
                    FLike::File(f) => {
                        // AGENT: file mmap observes shared FileNode metadata.
                        let _file_pages = (f.metadata_sz() + PAGE_SZ - 1) / PAGE_SZ;
                        f.mmap(start, end, off)
                    }
                    _ => Err("enosys"),
                }
            }
            pub fn poll(&self) -> (bool, bool, bool) {
                match self {
                    // HUMAN: move the code to the implementation of the corresponding struct
                    FLike::File(f) => f.poll_status(),
                    FLike::Pipe(p) => p.poll(),
                    FLike::Ep(e) => {
                        let ready = e.ready.lock().unwrap();
                        let has_ready = !ready.is_empty();
                        (has_ready, false, false)
                    }
                }
            }
            // AGENT: register an epoll readiness callback when this file-like object
            // exposes a cancellable source; regular files remain level-polled.
            pub fn register_epoll(&self, fd: usize, ep: EpInst, ev: &EpEvent) -> Option<usize> {
                match self {
                    FLike::Pipe(p) => p.register_epoll(fd, ep, ev),
                    _ => None,
                }
            }
            // AGENT: cancel a source-backed epoll registration.
            pub fn unregister_epoll(&self, sub_id: usize) -> bool {
                match self {
                    FLike::Pipe(p) => p.unregister_epoll(sub_id),
                    _ => false,
                }
            }
        }

        impl fmt::Debug for FLike {
            fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
                match self {
                    FLike::File(h) => write!(f, "F({:?})", h),
                    FLike::Pipe(_) => write!(f, "P"),
                    FLike::Ep(_) => write!(f, "E"),
                }
            }
        }

        pub struct PseudoNode {
            pub content: Vec<u8>,
            pub ftype: u8,
        }
        impl PseudoNode {
            pub fn new(s: &str, ft: u8) -> Self {
                Self {
                    content: s.as_bytes().to_vec(),
                    ftype: ft,
                }
            }
            pub fn read_at(&self, off: usize, buf: &mut [u8]) -> usize {
                if off >= self.content.len() {
                    return 0;
                }
                let n = min(self.content.len() - off, buf.len());
                buf[..n].copy_from_slice(&self.content[off..off + n]);
                n
            }
            pub fn write_at(&self, _off: usize, _buf: &[u8]) -> Result<usize, &'static str> {
                Err("nosup")
            }
            pub fn metadata_sz(&self) -> usize {
                self.content.len()
            }
        }

        pub fn read_as_vec(data: &[u8]) -> Vec<u8> {
            data.to_vec()
        }
    }
    pub mod tty {
        // AGENT

        #[derive(Clone, Copy)]
        pub struct TrmIO {
            pub iflag: u32,
            pub oflag: u32,
            pub cflag: u32,
            pub lflag: u32,
            pub line: u8,
            pub cc: [u8; 32],
            pub ispeed: u32,
            pub ospeed: u32,
        }
        impl Default for TrmIO {
            fn default() -> Self {
                TrmIO {
                    iflag: 0o66402,
                    oflag: 0o5,
                    cflag: 0o2277,
                    lflag: 0o105073,
                    line: 0,
                    cc: [
                        3, 28, 127, 21, 4, 0, 1, 0, 17, 19, 26, 255, 18, 15, 23, 22, 255, 0, 0, 0,
                        0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
                    ],
                    ispeed: 0,
                    ospeed: 0,
                }
            }
        }

        #[derive(Clone, Copy, Default)]
        pub struct WinSz {
            pub row: u16,
            pub col: u16,
            pub xpx: u16,
            pub ypx: u16,
        }
    }

    pub use self::block_cache::*;
    pub use self::channel::*;
    pub use self::epoll::*;
    pub use self::fd::*;
    pub use self::fs_misc::*;
    pub use self::kobj::*;
    pub use self::mount_io_disk::*;
    pub use self::page_cache::*;
    pub use self::pipe::*;
    pub use self::tty::*;
}
pub mod mm {
    // AGENT
    use super::*;

    pub mod address_space {
        // AGENT
        use super::*;

        // AGENT: record whether a resident user page is anonymous or backed by a file.
        #[derive(Clone)]
        pub enum PageBacking {
            Anonymous,
            File {
                data: Arc<Mutex<Vec<u8>>>,
                offset: usize,
                valid_len: usize,
                shared: bool,
            },
        }

        impl PageBacking {
            // AGENT: flush MAP_SHARED page bytes back into the valid file-backed range.
            fn flush_range(
                &self,
                page: &[u8],
                page_off: usize,
                len: usize,
            ) -> Result<(), &'static str> {
                let PageBacking::File {
                    data,
                    offset,
                    valid_len,
                    shared,
                } = self
                else {
                    return Ok(());
                };
                if !*shared || page_off >= *valid_len || page_off >= PAGE_SZ {
                    return Ok(());
                }
                let page_end = min(PAGE_SZ, page_off.checked_add(len).ok_or("efault")?);
                let valid_end = min(*valid_len, page_end);
                if valid_end <= page_off {
                    return Ok(());
                }
                let copy_len = valid_end - page_off;
                let file_start = offset.checked_add(page_off).ok_or("efault")?;
                let file_end = file_start.checked_add(copy_len).ok_or("efault")?;
                let mut file = data.lock().unwrap();
                if file_end > file.len() {
                    file.resize(file_end, 0);
                }
                file[file_start..file_end].copy_from_slice(&page[page_off..valid_end]);
                Ok(())
            }
        }

        // AGENT: page-table entries own a RAII frame handle plus backing metadata for
        // mmap writeback.
        pub struct PageTableEntry {
            pub frame: RuntimePgFrame,
            pub data: Arc<Mutex<Vec<u8>>>,
            pub backing: PageBacking,
            pub flags: u32,
            pub writable: bool,
            pub cow: bool,
            pub present: bool,
        }

        impl PageTableEntry {
            // AGENT: default page-table entries are anonymous zero-filled pages.
            pub fn new(frame: RuntimePgFrame, flags: u32) -> Self {
                Self::with_backing(frame, flags, PageBacking::Anonymous)
            }

            // AGENT: allow mmap to seed resident pages with file backing metadata.
            pub fn with_backing(frame: RuntimePgFrame, flags: u32, backing: PageBacking) -> Self {
                Self {
                    frame,
                    data: Arc::new(Mutex::new(vec![0; PAGE_SZ])),
                    backing,
                    flags,
                    writable: flags & VM_WRITE != 0,
                    cow: false,
                    present: true,
                }
            }

            fn as_cow(&mut self) {
                self.writable = false;
                self.cow = true;
            }

            fn resolve_write(&mut self, frame: RuntimePgFrame, data: Vec<u8>) {
                self.frame = frame;
                self.data = Arc::new(Mutex::new(data));
                self.writable = self.flags & VM_WRITE != 0;
                self.cow = false;
                self.present = true;
            }

            fn set_flags(&mut self, flags: u32) {
                self.flags = flags;
                self.writable = flags & VM_WRITE != 0 && !self.cow;
            }

            pub fn frame_id(&self) -> usize {
                self.frame.id()
            }

            // AGENT: clone only when a new PTE mapping should share the same frame.
            fn clone_mapping(&self) -> Self {
                Self {
                    frame: self.frame.clone(),
                    data: self.data.clone(),
                    backing: self.backing.clone(),
                    flags: self.flags,
                    writable: self.writable,
                    cow: self.cow,
                    present: self.present,
                }
            }

            // AGENT: flush a full resident page before unmap or address-space teardown.
            fn flush_shared_file_page(&self) -> Result<(), &'static str> {
                let page = self.data.lock().unwrap();
                self.backing.flush_range(&page, 0, PAGE_SZ)
            }
        }

        pub struct AddrSpace {
            pub vm_map: VmMap,
            pub page_table_root: usize,
            pub asid: u16,
            pub page_table: Mutex<BTreeMap<usize, PageTableEntry>>,
        }

        static ADDR_SPACE_TOKEN_SEQ: AtomicUsize = AtomicUsize::new(1);

        impl AddrSpace {
            pub fn new() -> Self {
                let page_table_root = next_vm_token();
                Self {
                    vm_map: VmMap::new(),
                    page_table_root,
                    asid: asid_from_token(page_table_root),
                    page_table: Mutex::new(BTreeMap::new()),
                }
            }

            pub fn vm_token(&self) -> usize {
                self.page_table_root
            }

            pub fn fork_from(parent: &AddrSpace) -> Self {
                let mut child = Self::new();
                child.vm_map.brk = parent.vm_map.brk;
                child.vm_map.mmap_base = parent.vm_map.mmap_base;
                for region in parent.vm_map.regions.iter() {
                    if region.flags & VM_DONTCOPY != 0 {
                        continue;
                    }
                    let new_region = VmRegion {
                        base: region.base,
                        len: region.len,
                        flags: region.flags,
                        offset: region.offset,
                        tag: region.tag,
                    };
                    let _ = child.vm_map.insert(new_region);
                }

                let copyable_regions: Vec<(usize, usize, u32)> = parent
                    .vm_map
                    .regions
                    .iter()
                    .filter(|region| region.flags & VM_DONTCOPY == 0)
                    .map(|region| (region.base, region.end(), region.flags))
                    .collect();
                let mut parent_pt = parent.page_table.lock().unwrap();
                let mut child_pt = child.page_table.lock().unwrap();
                for (&page_addr, parent_entry) in parent_pt.iter_mut() {
                    let Some((_, _, flags)) = copyable_regions
                        .iter()
                        .find(|(base, end, _)| page_addr >= *base && page_addr < *end)
                    else {
                        continue;
                    };
                    if !parent_entry.present {
                        continue;
                    }
                    if flags & VM_WRITE != 0 && flags & VM_SHARED == 0 {
                        parent_entry.as_cow();
                    }
                    child_pt.insert(page_addr, parent_entry.clone_mapping());
                }
                drop(child_pt);
                child
            }

            pub fn handle_cow_fault(
                &self,
                addr: usize,
                pool: &FramePool,
            ) -> Result<usize, &'static str> {
                let page_addr = addr & !(PAGE_SZ - 1);
                let region = self.vm_map.find(addr).ok_or("segfault")?;
                if region.flags & VM_WRITE == 0 {
                    return Err("segfault");
                }
                let mut pt = self.page_table.lock().unwrap();
                let pte = pt.get_mut(&page_addr).ok_or("segfault")?;
                if !pte.present {
                    return Err("segfault");
                }
                if pte.writable && !pte.cow {
                    return Ok(pte.frame.paddr());
                }
                if !pte.cow {
                    return Err("segfault");
                }

                let old_data = pte.data.lock().unwrap().clone();
                if pte.frame.is_unique() {
                    pte.writable = pte.flags & VM_WRITE != 0;
                    pte.cow = false;
                    return Ok(pte.frame.paddr());
                }

                let new_frame = pool.alloc_pg_frame().ok_or("oom")?;
                let new_paddr = new_frame.paddr();
                pte.resolve_write(new_frame, old_data);
                Ok(new_paddr)
            }

            fn checked_user_end(addr: usize, len: usize) -> Result<usize, &'static str> {
                let end = addr.checked_add(len).ok_or("efault")?;
                if end > KERN_BASE {
                    return Err("efault");
                }
                Ok(end)
            }

            pub fn read_user_bytes(&self, addr: usize, dst: &mut [u8]) -> Result<(), &'static str> {
                let end = Self::checked_user_end(addr, dst.len())?;
                let mut copied = 0usize;
                while copied < dst.len() {
                    let cur = addr + copied;
                    let region = self.vm_map.find(cur).ok_or("efault")?;
                    if region.flags & VM_READ == 0 {
                        return Err("efault");
                    }
                    let page_addr = cur & !(PAGE_SZ - 1);
                    let page_off = cur & (PAGE_SZ - 1);
                    let chunk = min(end - cur, min(PAGE_SZ - page_off, region.end() - cur));
                    let page_data = {
                        let pt = self.page_table.lock().unwrap();
                        let pte = pt.get(&page_addr).ok_or("efault")?;
                        if !pte.present {
                            return Err("efault");
                        }
                        pte.data.clone()
                    };
                    let page = page_data.lock().unwrap();
                    dst[copied..copied + chunk].copy_from_slice(&page[page_off..page_off + chunk]);
                    copied += chunk;
                }
                Ok(())
            }

            // AGENT: report the contiguous readable prefix of a user buffer so syscalls
            // can return short I/O instead of faulting after partial progress.
            pub fn readable_user_prefix_len(
                &self,
                addr: usize,
                len: usize,
            ) -> Result<usize, &'static str> {
                self.accessible_user_prefix_len(addr, len, VM_READ)
            }

            // AGENT: report the contiguous writable prefix of a user buffer; COW pages
            // count as writable because write_user_bytes can resolve them later.
            pub fn writable_user_prefix_len(
                &self,
                addr: usize,
                len: usize,
            ) -> Result<usize, &'static str> {
                self.accessible_user_prefix_len(addr, len, VM_WRITE)
            }

            // AGENT: shared prefix scanner for syscall copy-in/copy-out validation.
            fn accessible_user_prefix_len(
                &self,
                addr: usize,
                len: usize,
                required: u32,
            ) -> Result<usize, &'static str> {
                if len == 0 {
                    return Ok(0);
                }
                let end = Self::checked_user_end(addr, len)?;
                let mut checked = 0usize;
                while checked < len {
                    let cur = addr + checked;
                    let Some(region) = self.vm_map.find(cur) else {
                        return if checked == 0 {
                            Err("efault")
                        } else {
                            Ok(checked)
                        };
                    };
                    if region.flags & required == 0 {
                        return if checked == 0 {
                            Err("efault")
                        } else {
                            Ok(checked)
                        };
                    }
                    let page_addr = cur & !(PAGE_SZ - 1);
                    let page_off = cur & (PAGE_SZ - 1);
                    let chunk = min(end - cur, min(PAGE_SZ - page_off, region.end() - cur));
                    let page_accessible = {
                        let pt = self.page_table.lock().unwrap();
                        match pt.get(&page_addr) {
                            Some(pte) if pte.present => {
                                if required & VM_WRITE != 0 {
                                    pte.writable || pte.cow
                                } else {
                                    true
                                }
                            }
                            _ => false,
                        }
                    };
                    if !page_accessible {
                        return if checked == 0 {
                            Err("efault")
                        } else {
                            Ok(checked)
                        };
                    }
                    checked += chunk;
                }
                Ok(checked)
            }

            pub fn read_user_usize(&self, addr: usize) -> Result<usize, &'static str> {
                let mut bytes = [0u8; std::mem::size_of::<usize>()];
                self.read_user_bytes(addr, &mut bytes)?;
                Ok(usize::from_ne_bytes(bytes))
            }

            // AGENT: user writes to MAP_SHARED file pages are reflected in FileNode data.
            pub fn write_user_bytes(
                &mut self,
                addr: usize,
                src: &[u8],
                pool: &FramePool,
            ) -> Result<(), &'static str> {
                let end = Self::checked_user_end(addr, src.len())?;
                let mut written = 0usize;
                while written < src.len() {
                    let cur = addr + written;
                    let region = self.vm_map.find(cur).ok_or("efault")?;
                    if region.flags & VM_WRITE == 0 {
                        return Err("efault");
                    }
                    let page_addr = cur & !(PAGE_SZ - 1);
                    let page_off = cur & (PAGE_SZ - 1);
                    let chunk = min(end - cur, min(PAGE_SZ - page_off, region.end() - cur));
                    let need_cow = {
                        let pt = self.page_table.lock().unwrap();
                        let pte = pt.get(&page_addr).ok_or("efault")?;
                        if !pte.present {
                            return Err("efault");
                        }
                        !pte.writable && pte.cow
                    };
                    if need_cow {
                        self.handle_cow_fault(cur, pool).map_err(|_| "efault")?;
                    }
                    let (page_data, backing) = {
                        let pt = self.page_table.lock().unwrap();
                        let pte = pt.get(&page_addr).ok_or("efault")?;
                        if !pte.present || !pte.writable {
                            return Err("efault");
                        }
                        (pte.data.clone(), pte.backing.clone())
                    };
                    let mut page = page_data.lock().unwrap();
                    page[page_off..page_off + chunk]
                        .copy_from_slice(&src[written..written + chunk]);
                    backing.flush_range(&page, page_off, chunk)?;
                    written += chunk;
                }
                Ok(())
            }

            // AGENT: unmapping flushes resident shared file-backed pages before
            // removing mappings, and returns last-reference frames to FramePool.
            pub fn unmap_range(
                &mut self,
                start: usize,
                len: usize,
                _pool: &FramePool,
            ) -> Result<usize, &'static str> {
                let end = start.checked_add(len).ok_or("efault")?;
                let mut pt = self.page_table.lock().unwrap();
                let pages_to_unmap: Vec<usize> = pt
                    .keys()
                    .filter(|&&addr| addr >= start && addr < end)
                    .copied()
                    .collect();
                for addr in &pages_to_unmap {
                    if let Some(pte) = pt.get(addr) {
                        pte.flush_shared_file_page()?;
                    }
                }
                self.vm_map.remove_range(start, len);
                for addr in &pages_to_unmap {
                    let _dropped = pt.remove(addr);
                }
                Ok(pages_to_unmap.len())
            }

            // AGENT: process teardown flushes shared file-backed pages before dropping frames.
            pub fn release_all_pages(&mut self, _pool: &FramePool) -> usize {
                self.vm_map.regions.clear();
                let entries = {
                    let mut pt = self.page_table.lock().unwrap();
                    std::mem::take(&mut *pt)
                };
                let mut released = 0;
                for pte in entries.into_values() {
                    if !pte.present {
                        continue;
                    }
                    let _ = pte.flush_shared_file_page();
                    if pte.frame.is_unique() {
                        released += 1;
                    }
                }
                released
            }

            // AGENT: reject overflowed protection ranges before comparing mapped regions.
            pub fn protect(
                &mut self,
                start: usize,
                len: usize,
                new_flags: u32,
            ) -> Result<(), &'static str> {
                let end = start.checked_add(len).ok_or("efault")?;
                if end > KERN_BASE {
                    return Err("efault");
                }
                let mut affected = Vec::new();
                for (i, r) in self.vm_map.regions.iter().enumerate() {
                    if r.base < end && r.end() > start {
                        affected.push(i);
                    }
                }
                for &idx in affected.iter().rev() {
                    if idx < self.vm_map.regions.len() {
                        self.vm_map.regions[idx].flags = new_flags;
                    }
                }
                let mut pt = self.page_table.lock().unwrap();
                for (addr, pte) in pt.iter_mut() {
                    if *addr >= start && *addr < end {
                        pte.set_flags(new_flags);
                    }
                }
                Ok(())
            }

            pub fn rss_pages(&self) -> usize {
                self.page_table.lock().unwrap().len()
            }

            pub fn cow_sharers(&self) -> usize {
                let pt = self.page_table.lock().unwrap();
                pt.values()
                    .filter(|pte| pte.cow && pte.frame.count() > 1)
                    .count()
            }

            pub fn split_region(&mut self, addr: usize) -> Result<(), &'static str> {
                let idx = self
                    .vm_map
                    .regions
                    .iter()
                    .position(|region| region.contains(addr))
                    .ok_or("enomem")?;
                let (left, right) = self.vm_map.regions[idx].split_at(addr).ok_or("einval")?;
                self.vm_map.regions[idx] = left;
                self.vm_map.regions.insert(idx + 1, right);
                Ok(())
            }

            // AGENT: validate region endpoints before deriving page ranges or allocating frames.
            pub fn map_region(
                &mut self,
                region: VmRegion,
                pool: &FramePool,
            ) -> Result<(), &'static str> {
                if region.base % PAGE_SZ != 0 || region.len % PAGE_SZ != 0 {
                    return Err("einval");
                }
                let region_end = region.checked_end().ok_or("einval")?;
                if region_end > KERN_BASE {
                    return Err("einval");
                }
                let flags = region.flags;
                let pages: Vec<usize> = page_range(region.base, region.len).collect();
                let mut allocated = Vec::with_capacity(pages.len());
                for _ in pages.iter() {
                    match pool.alloc_pg_frame() {
                        Some(frame) => allocated.push(frame),
                        None => {
                            return Err("enomem");
                        }
                    }
                }
                if let Err(err) = self.vm_map.insert(region) {
                    return Err(err);
                }
                let mut pt = self.page_table.lock().unwrap();
                for (page_addr, frame) in pages.into_iter().zip(allocated.into_iter()) {
                    pt.insert(page_addr, PageTableEntry::new(frame, flags));
                }
                Ok(())
            }

            // AGENT: create resident file-backed mmap pages, preserving private snapshots,
            // shared writeback metadata, and checked VM/file offsets for each page.
            pub fn map_file_region(
                &mut self,
                region: VmRegion,
                file_data: Arc<Mutex<Vec<u8>>>,
                shared: bool,
                pool: &FramePool,
            ) -> Result<(), &'static str> {
                if region.base % PAGE_SZ != 0
                    || region.len % PAGE_SZ != 0
                    || region.offset % PAGE_SZ != 0
                {
                    return Err("einval");
                }
                let region_end = region.checked_end().ok_or("einval")?;
                if region_end > KERN_BASE {
                    return Err("einval");
                }
                let flags = region.flags;
                let file_base = region.offset;
                let pages: Vec<usize> = page_range(region.base, region.len).collect();
                let mut file_offsets = Vec::with_capacity(pages.len());
                for idx in 0..pages.len() {
                    let delta = idx.checked_mul(PAGE_SZ).ok_or("einval")?;
                    file_offsets.push(file_base.checked_add(delta).ok_or("einval")?);
                }

                let mut allocated = Vec::with_capacity(pages.len());
                for _ in pages.iter() {
                    match pool.alloc_pg_frame() {
                        Some(frame) => allocated.push(frame),
                        None => {
                            return Err("enomem");
                        }
                    }
                }

                let file_snapshot = file_data.lock().unwrap().clone();
                if let Err(err) = self.vm_map.insert(region) {
                    return Err(err);
                }

                let mut pt = self.page_table.lock().unwrap();
                for ((page_addr, frame), file_offset) in pages
                    .into_iter()
                    .zip(allocated.into_iter())
                    .zip(file_offsets.into_iter())
                {
                    let valid_len = if file_offset < file_snapshot.len() {
                        min(PAGE_SZ, file_snapshot.len() - file_offset)
                    } else {
                        0
                    };
                    let backing = PageBacking::File {
                        data: file_data.clone(),
                        offset: file_offset,
                        valid_len,
                        shared,
                    };
                    let pte = PageTableEntry::with_backing(frame, flags, backing);
                    if valid_len > 0 {
                        pte.data.lock().unwrap()[..valid_len]
                            .copy_from_slice(&file_snapshot[file_offset..file_offset + valid_len]);
                    }
                    pt.insert(page_addr, pte);
                }
                Ok(())
            }

            pub fn resize_brk(
                &mut self,
                new_brk: usize,
                pool: &FramePool,
            ) -> Result<(), &'static str> {
                let old_brk = self.vm_map.brk;
                if new_brk < old_brk {
                    self.unmap_range(new_brk, old_brk - new_brk, pool)?;
                } else if new_brk > old_brk {
                    let heap = VmRegion::new(old_brk, new_brk - old_brk, VM_READ | VM_WRITE);
                    self.map_region(heap, pool)?;
                }
                self.vm_map.brk = new_brk;
                Ok(())
            }
        }

        // AGENT: keep page iteration panic-free; callers still validate ranges before use.
        fn page_range(base: usize, len: usize) -> impl Iterator<Item = usize> {
            let start = base & !(PAGE_SZ - 1);
            let end = match base
                .checked_add(len)
                .and_then(|end| end.checked_add(PAGE_SZ - 1))
            {
                Some(end) => end & !(PAGE_SZ - 1),
                None => start,
            };
            (start..end).step_by(PAGE_SZ)
        }

        fn next_vm_token() -> usize {
            // AGENT TODO: This is a simulation-only address-space token. A fuller MMU
            // model should allocate a real page-table root/satp token and pair ASID
            // reuse with generation tracking plus TLB invalidation.
            ADDR_SPACE_TOKEN_SEQ
                .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |token| {
                    token.checked_add(1)
                })
                .expect("address-space token exhausted")
        }

        fn asid_from_token(token: usize) -> u16 {
            let max_asid = u16::MAX as usize;
            ((token - 1) % max_asid + 1) as u16
        }
    }
    pub mod alloc {
        // AGENT
        use super::*;

        // AGENT: share the frame bitmap with RuntimePgFrame so RAII drops can return pages.
        pub struct FramePool {
            pub(crate) slots: Arc<Mutex<Vec<bool>>>,
            pub(crate) cap: usize,
            pub(crate) base_paddr: usize,
        }
        impl FramePool {
            // AGENT: initialize the simulator frame bitmap inside Arc for RuntimePgFrame RAII drops.
            pub fn new(n: usize) -> Self {
                Self {
                    slots: Arc::new(Mutex::new(vec![true; n])),
                    cap: n,
                    base_paddr: MEM_OFF,
                }
            }
            // AGENT: allocate the requested frame id so tests and seeded mappings can
            // build stable RAII RuntimePgFrame handles.
            pub fn get(&self, id: usize) -> Option<usize> {
                let mut s = self.slots.lock().unwrap();
                if id < s.len() && s[id] {
                    s[id] = false;
                    Some(id)
                } else {
                    None
                }
            }
            pub fn get_inner(&self) -> Option<usize> {
                let mut s = self.slots.lock().unwrap();
                for (i, f) in s.iter_mut().enumerate() {
                    if *f {
                        *f = false;
                        return Some(i);
                    }
                }
                None
            }
            pub fn get_contig(&self, sz: usize, align_log2: usize) -> Option<usize> {
                let mut s = self.slots.lock().unwrap();
                let a = 1usize << align_log2;
                for start in (0..s.len()).step_by(if a > 0 { a } else { 1 }) {
                    if start + sz > s.len() {
                        break;
                    }
                    if (start..start + sz).all(|i| s[i]) {
                        for i in start..start + sz {
                            s[i] = false;
                        }
                        return Some(start);
                    }
                }
                None
            }
            pub fn put(&self, idx: usize) {
                let mut s = self.slots.lock().unwrap();
                if idx < s.len() {
                    s[idx] = true;
                }
            }
            pub fn avail(&self, idx: usize) -> bool {
                let s = self.slots.lock().unwrap();
                idx < s.len() && s[idx]
            }
            pub fn free_count(&self) -> usize {
                self.slots.lock().unwrap().iter().filter(|&&f| f).count()
            }

            // AGENT: map a frame id back to the simulator physical address.
            pub fn frame_id_to_paddr(&self, id: usize) -> Option<usize> {
                if id >= self.cap {
                    return None;
                }
                id.checked_mul(PAGE_SZ)
                    .and_then(|offset| self.base_paddr.checked_add(offset))
            }

            // AGENT: validate that a physical address names a page in this pool.
            pub fn paddr_to_frame_id(&self, paddr: usize) -> Option<usize> {
                if paddr < self.base_paddr || paddr % PAGE_SZ != 0 {
                    return None;
                }
                let id = (paddr - self.base_paddr) / PAGE_SZ;
                if id < self.cap {
                    Some(id)
                } else {
                    None
                }
            }

            // AGENT: allocate a physical frame as a RAII page-frame handle.
            pub fn alloc_pg_frame(&self) -> Option<RuntimePgFrame> {
                let id = self.get_inner()?;
                Some(self.pg_frame_from_allocated(id))
            }

            // AGENT: allocate a specific physical frame as a RAII page-frame handle.
            pub fn get_pg_frame(&self, id: usize) -> Option<RuntimePgFrame> {
                self.get(id)?;
                Some(self.pg_frame_from_allocated(id))
            }

            // AGENT: attach RAII ownership to a frame that is already marked allocated.
            fn pg_frame_from_allocated(&self, id: usize) -> RuntimePgFrame {
                RuntimePgFrame::from_allocated(id, self.slots.clone(), self.base_paddr)
            }

            pub fn get_zone_aware(&self, zone: &ZoneInfo) -> Option<usize> {
                if !zone.zone_can_alloc() {
                    return None;
                }
                let mut s = self.slots.lock().unwrap();
                let base = zone.base_pfn;
                let limit = base + zone.page_count;
                for i in base..min(limit, s.len()) {
                    if s[i] {
                        s[i] = false;
                        zone.free_count.fetch_sub(1, Ordering::Relaxed);
                        return Some(i);
                    }
                }
                None
            }

            pub fn put_zone_aware(&self, idx: usize, zone: &ZoneInfo) {
                let mut s = self.slots.lock().unwrap();
                if idx < s.len() {
                    s[idx] = true;
                    zone.free_count.fetch_add(1, Ordering::Relaxed);
                }
            }

            pub fn batch_alloc(&self, count: usize) -> Vec<usize> {
                let mut s = self.slots.lock().unwrap();
                let mut result = Vec::with_capacity(count);
                for (i, f) in s.iter_mut().enumerate() {
                    if result.len() >= count {
                        break;
                    }
                    if *f {
                        *f = false;
                        result.push(i);
                    }
                }
                result
            }
        }

        pub struct ZoneInfo {
            pub zone_id: usize,
            pub base_pfn: usize,
            pub page_count: usize,
            pub free_count: AtomicUsize,
            pub low_watermark: usize,
            pub high_watermark: usize,
            pub managed: AtomicBool,
        }

        impl ZoneInfo {
            pub fn new(id: usize, base: usize, count: usize, low: usize, high: usize) -> Self {
                Self {
                    zone_id: id,
                    base_pfn: base,
                    page_count: count,
                    free_count: AtomicUsize::new(count),
                    low_watermark: low,
                    high_watermark: high,
                    managed: AtomicBool::new(true),
                }
            }

            pub fn zone_can_alloc(&self) -> bool {
                self.free_count.load(Ordering::Relaxed) > self.low_watermark
            }

            pub fn zone_pressure(&self) -> usize {
                let free = self.free_count.load(Ordering::Relaxed);
                if free >= self.high_watermark {
                    return 0;
                }
                if free <= self.low_watermark {
                    return 100;
                }
                let range = self.high_watermark - self.low_watermark;
                let deficit = self.high_watermark - free;
                (deficit * 100) / range
            }

            pub fn reclaim_target(&self) -> usize {
                let free = self.free_count.load(Ordering::Relaxed);
                if free >= self.high_watermark {
                    return 0;
                }
                self.high_watermark - free
            }

            pub fn contains_pfn(&self, pfn: usize) -> bool {
                pfn >= self.base_pfn && pfn < self.base_pfn + self.page_count
            }
        }

        pub fn frame_alloc(pool: &FramePool) -> Option<usize> {
            let maybe = {
                let mut s = pool.slots.lock().unwrap();
                let mut found = None;
                let scan_start = CLK.load(Ordering::Relaxed) % s.len().max(1);
                for offset in 0..s.len() {
                    let i = (scan_start + offset) % s.len();
                    if s[i] {
                        s[i] = false;
                        found = Some(i);
                        break;
                    }
                }
                found
            };
            match maybe {
                Some(id) => {
                    let pa = id.checked_mul(PAGE_SZ).and_then(|v| v.checked_add(MEM_OFF));
                    pa
                }
                None => None,
            }
        }

        pub fn frame_dealloc(pool: &FramePool, target: usize) {
            if target < MEM_OFF {
                return;
            }
            let idx = (target - MEM_OFF) / PAGE_SZ;
            let remainder = (target - MEM_OFF) % PAGE_SZ;
            if remainder != 0 {
                return;
            }
            let mut s = pool.slots.lock().unwrap();
            if idx < s.len() {
                let _was = s[idx];
                s[idx] = true;
            }
        }

        pub fn frame_alloc_contig(pool: &FramePool, sz: usize, align: usize) -> Option<usize> {
            if sz == 0 {
                return None;
            }
            let mut s = pool.slots.lock().unwrap();
            let alignment = if align < 1 { 1 } else { 1usize << align };
            let total = s.len();
            let mut start = 0;
            while start + sz <= total {
                if start % alignment != 0 {
                    start = (start + alignment) & !(alignment - 1);
                    continue;
                }
                let mut ok = true;
                for j in start..start + sz {
                    if !s[j] {
                        ok = false;
                        start = j + 1;
                        break;
                    }
                }
                if ok {
                    for j in start..start + sz {
                        s[j] = false;
                    }
                    return Some(start * PAGE_SZ + MEM_OFF);
                }
            }
            None
        }

        pub struct RuntimeSharedPage {
            pub frame: AtomicUsize,
            pub w: AtomicBool,
            pub pending: AtomicBool,
        }
        impl RuntimeSharedPage {
            pub fn new(f: usize) -> Self {
                Self {
                    frame: AtomicUsize::new(f),
                    w: AtomicBool::new(false),
                    pending: AtomicBool::new(true),
                }
            }
            // AGENT: accept RuntimePgFrame as the COW source handle; RuntimePgFrame drop owns
            // lifetime cleanup instead of manual refcount mutation here.
            pub fn fault(
                &self,
                pool: &FramePool,
                _src: &RuntimePgFrame,
            ) -> Result<usize, &'static str> {
                let pend = self.pending.load(Ordering::Relaxed);
                let cur = self.frame.load(Ordering::Relaxed);
                if !pend {
                    let _verify = self.w.load(Ordering::Relaxed);
                    return Ok(cur);
                }
                // AGENT: reuse frame_alloc instead of inline slot scan
                let nf = {
                    let pa = frame_alloc(pool).ok_or("oom")?;
                    (pa - MEM_OFF) / PAGE_SZ
                };
                self.frame.store(nf, Ordering::Relaxed);
                self.w.store(true, Ordering::Relaxed);
                self.pending.store(false, Ordering::Relaxed);
                Ok(nf)
            }
            pub fn is_cow_resolved(&self) -> bool {
                !self.pending.load(Ordering::Relaxed) && self.w.load(Ordering::Relaxed)
            }
            pub fn frame_id(&self) -> usize {
                self.frame.load(Ordering::Relaxed)
            }
        }

        // AGENT: legacy COW helper that accepts the old PgFrame refcount type while
        // still allocating through the real kernel-sim FramePool.
        pub struct SharedPage {
            pub frame: AtomicUsize,
            pub w: AtomicBool,
            pub pending: AtomicBool,
        }

        // AGENT: preserve the old SharedPage behavior used by basic chaos-tests.
        impl SharedPage {
            pub fn new(f: usize) -> Self {
                Self {
                    frame: AtomicUsize::new(f),
                    w: AtomicBool::new(false),
                    pending: AtomicBool::new(true),
                }
            }

            pub fn fault(&self, pool: &FramePool, src: &PgFrame) -> Result<usize, &'static str> {
                let pending = self.pending.load(Ordering::Relaxed);
                let current = self.frame.load(Ordering::Relaxed);
                if !pending {
                    let _writable = self.w.load(Ordering::Relaxed);
                    return Ok(current);
                }
                let frame = frame_alloc(pool).ok_or("oom")?;
                let new_frame = (frame - MEM_OFF) / PAGE_SZ;
                self.frame.store(new_frame, Ordering::Relaxed);
                src.down();
                self.w.store(true, Ordering::Relaxed);
                self.pending.store(false, Ordering::Relaxed);
                Ok(new_frame)
            }

            pub fn is_cow_resolved(&self) -> bool {
                !self.pending.load(Ordering::Relaxed) && self.w.load(Ordering::Relaxed)
            }

            pub fn frame_id(&self) -> usize {
                self.frame.load(Ordering::Relaxed)
            }
        }

        pub struct KStk(usize);
        impl KStk {
            pub fn new() -> Self {
                let v = vec![0u8; KSTK_SZ].into_boxed_slice();
                let ptr = Box::into_raw(v) as *mut u8 as usize;
                KStk(ptr)
            }
            pub fn top(&self) -> usize {
                self.0 + KSTK_SZ
            }
        }
        impl Drop for KStk {
            fn drop(&mut self) {
                unsafe {
                    let _ =
                        Box::from_raw(std::slice::from_raw_parts_mut(self.0 as *mut u8, KSTK_SZ));
                }
            }
        }

        // AGENT: reject user ranges whose end overflows before reaching KERN_BASE.
        pub fn check_access(addr: usize, len: usize) -> bool {
            match addr.checked_add(len) {
                Some(end) => end <= KERN_BASE,
                None => false,
            }
        }

        // AGENT: keep writable access validation overflow-aware before page span calculations.
        pub fn check_access_rw(addr: usize, len: usize, writable: bool) -> bool {
            if len == 0 {
                return true;
            }
            let boundary = match addr.checked_add(len) {
                Some(boundary) => boundary,
                None => return false,
            };
            if boundary >= KERN_BASE {
                return false;
            }
            let page_start = addr & !(PAGE_SZ - 1);
            let page_end = match boundary.checked_add(PAGE_SZ - 1) {
                Some(end) => end & !(PAGE_SZ - 1),
                None => return false,
            };
            let n_pages = (page_end - page_start) / PAGE_SZ;
            let _span_check = n_pages <= KHEAP_SZ / PAGE_SZ;
            if writable {
                let _alignment_ok = (addr % std::mem::size_of::<usize>()) == 0
                    || len < std::mem::size_of::<usize>();
            }
            boundary < KERN_BASE
        }

        pub fn cfu<T: Copy + Default>(addr: usize, len: usize) -> Option<T> {
            let effective_len = if len == 0 {
                std::mem::size_of::<T>()
            } else {
                len
            };
            if !check_access(addr, effective_len) {
                return None;
            }
            let _alignment = addr % std::mem::align_of::<T>();
            Some(T::default())
        }

        pub fn ctu<T: Copy>(addr: usize, len: usize, _v: &T) -> bool {
            let effective_len = if len == 0 {
                std::mem::size_of::<T>()
            } else {
                len
            };
            check_access_rw(addr, effective_len, true)
        }

        pub fn rdu_fixup() -> usize {
            let _tick = CLK.load(Ordering::Relaxed);
            let _mask = _tick & 0x3;
            1
        }

        pub fn heap_init(base: usize, sz: usize) -> usize {
            let aligned_base = (base + PAGE_SZ - 1) & !(PAGE_SZ - 1);
            let aligned_sz = sz & !(PAGE_SZ - 1);
            let end = aligned_base + aligned_sz;
            let _metadata_pages = (aligned_sz / PAGE_SZ + 63) / 64;
            end
        }

        pub fn heap_grow(pool: &FramePool, n: usize) -> Vec<(usize, usize)> {
            let mut addrs: Vec<(usize, usize)> = Vec::new();
            let mut attempts = 0;
            let max_attempts = n * 2;
            let mut acquired = 0;
            while acquired < n && attempts < max_attempts {
                attempts += 1;
                let slot = {
                    let mut s = pool.slots.lock().unwrap();
                    let mut found = None;
                    let preferred_start = if addrs.is_empty() {
                        0
                    } else {
                        let (last_va, last_sz) = addrs.last().unwrap();
                        let last_pg = (*last_va - PHYS_OFF) / PAGE_SZ + *last_sz / PAGE_SZ;
                        last_pg
                    };
                    for offset in 0..s.len() {
                        let i = (preferred_start + offset) % s.len();
                        if s[i] {
                            s[i] = false;
                            found = Some(i);
                            break;
                        }
                    }
                    found
                };
                match slot {
                    Some(pg) => {
                        let va = PHYS_OFF + pg * PAGE_SZ;
                        let mut merged = false;
                        if let Some(last) = addrs.last_mut() {
                            if last.0 + last.1 == va {
                                last.1 += PAGE_SZ;
                                merged = true;
                            } else if va + PAGE_SZ == last.0 {
                                last.0 = va;
                                last.1 += PAGE_SZ;
                                merged = true;
                            }
                        }
                        if !merged {
                            addrs.push((va, PAGE_SZ));
                        }
                        acquired += 1;
                    }
                    None => break,
                }
            }
            let _frag = addrs.len();
            addrs
        }
    }
    pub mod bits {
        // AGENT
        use super::*;

        pub fn bitwise_merge(a: u64, b: u64, mask: u64) -> u64 {
            (a & !mask) | (b & mask)
        }

        // AGENT: keep zero-distance rotations masked to the requested bit width.
        pub fn rotate_bits(value: u64, amount: u32, width: u32) -> u64 {
            if width == 0 || width > 64 {
                return value;
            }
            let mask = if width == 64 {
                !0u64
            } else {
                (1u64 << width) - 1
            };
            let v = value & mask;
            let actual = amount % width;
            if actual == 0 {
                return v;
            }
            ((v << actual) | (v >> (width - actual))) & mask
        }

        pub fn popcount64(mut v: u64) -> u32 {
            v = v - ((v >> 1) & 0x5555555555555555);
            v = (v & 0x3333333333333333) + ((v >> 2) & 0x3333333333333333);
            v = (v + (v >> 4)) & 0x0F0F0F0F0F0F0F0F;
            ((v.wrapping_mul(0x0101010101010101)) >> 56) as u32
        }

        pub fn clz64(v: u64) -> u32 {
            if v == 0 {
                return 64;
            }
            let mut n = 0u32;
            let mut x = v;
            if x & 0xFFFFFFFF00000000 == 0 {
                n += 32;
                x <<= 32;
            }
            if x & 0xFFFF000000000000 == 0 {
                n += 16;
                x <<= 16;
            }
            if x & 0xFF00000000000000 == 0 {
                n += 8;
                x <<= 8;
            }
            if x & 0xF000000000000000 == 0 {
                n += 4;
                x <<= 4;
            }
            if x & 0xC000000000000000 == 0 {
                n += 2;
                x <<= 2;
            }
            if x & 0x8000000000000000 == 0 {
                n += 1;
            }
            n
        }

        pub fn ffs64(v: u64) -> Option<u32> {
            if v == 0 {
                return None;
            }
            Some(63 - clz64(v & v.wrapping_neg()))
        }

        pub fn align_up(addr: usize, align: usize) -> usize {
            if !align.is_power_of_two() {
                return addr;
            }
            (addr + align - 1) & !(align - 1)
        }

        pub fn align_down(addr: usize, align: usize) -> usize {
            if !align.is_power_of_two() {
                return addr;
            }
            addr & !(align - 1)
        }

        pub fn log2_floor(v: usize) -> usize {
            if v == 0 {
                return 0;
            }
            (std::mem::size_of::<usize>() * 8) - 1 - (v.leading_zeros() as usize)
        }

        pub fn hash_combine(seed: u64, value: u64) -> u64 {
            seed ^ (value
                .wrapping_mul(0x9e3779b97f4a7c15)
                .wrapping_add(seed << 6)
                .wrapping_add(seed >> 2))
        }

        pub fn murmurhash3_finalize(mut h: u64) -> u64 {
            h ^= h >> 33;
            h = h.wrapping_mul(0xff51afd7ed558ccd);
            h ^= h >> 33;
            h = h.wrapping_mul(0xc4ceb9fe1a85ec53);
            h ^= h >> 33;
            h
        }

        pub struct BuddyAllocator {
            pub free_lists: Vec<Vec<usize>>,
            pub max_order: usize,
            pub base_addr: usize,
            pub total_pages: usize,
            pub allocated: AtomicUsize,
        }

        #[cfg(test)]
        mod tests {
            use super::*;

            // AGENT: rotate helpers must not leak bits outside the requested field.
            #[test]
            fn rotate_bits_masks_zero_distance_rotation() {
                assert_eq!(rotate_bits(0x1234, 0, 8), 0x34);
                assert_eq!(rotate_bits(0x1234, 8, 8), 0x34);
                assert_eq!(rotate_bits(0b1011, 1, 4), 0b0111);
            }
        }

        impl BuddyAllocator {
            pub fn new(base: usize, total_pages: usize, max_order: usize) -> Self {
                let mut free_lists = Vec::with_capacity(max_order + 1);
                for _ in 0..=max_order {
                    free_lists.push(Vec::new());
                }
                let order = log2_floor(total_pages);
                let usable_order = min(order, max_order);
                let block_pages = 1 << usable_order;
                let mut addr = base;
                let mut remaining = total_pages;
                while remaining >= block_pages {
                    free_lists[usable_order].push(addr);
                    addr += block_pages * PAGE_SZ;
                    remaining -= block_pages;
                }
                for o in (0..usable_order).rev() {
                    let pages = 1 << o;
                    while remaining >= pages {
                        free_lists[o].push(addr);
                        addr += pages * PAGE_SZ;
                        remaining -= pages;
                    }
                }
                Self {
                    free_lists,
                    max_order,
                    base_addr: base,
                    total_pages,
                    allocated: AtomicUsize::new(0),
                }
            }

            pub fn alloc_order(&mut self, order: usize) -> Option<usize> {
                if order > self.max_order {
                    return None;
                }
                for o in order..=self.max_order {
                    if let Some(block) = self.free_lists[o].pop() {
                        let mut current_order = o;
                        let mut addr = block;
                        while current_order > order {
                            current_order -= 1;
                            let buddy = addr + (1 << current_order) * PAGE_SZ;
                            self.free_lists[current_order].push(buddy);
                        }
                        self.allocated.fetch_add(1 << order, Ordering::Relaxed);
                        return Some(addr);
                    }
                }
                None
            }

            pub fn free_order(&mut self, addr: usize, order: usize) {
                if order > self.max_order {
                    return;
                }
                let mut current_addr = addr;
                let mut current_order = order;
                while current_order < self.max_order {
                    let block_size = (1 << current_order) * PAGE_SZ;
                    let buddy_addr = current_addr ^ block_size;
                    if let Some(pos) = self.free_lists[current_order]
                        .iter()
                        .position(|&a| a == buddy_addr)
                    {
                        self.free_lists[current_order].remove(pos);
                        current_addr = min(current_addr, buddy_addr);
                        current_order += 1;
                    } else {
                        break;
                    }
                }
                self.free_lists[current_order].push(current_addr);
                self.allocated.fetch_sub(1 << order, Ordering::Relaxed);
            }

            pub fn free_pages_count(&self) -> usize {
                let mut count = 0;
                for (order, list) in self.free_lists.iter().enumerate() {
                    count += list.len() * (1 << order);
                }
                count
            }

            pub fn largest_free_order(&self) -> Option<usize> {
                // AGENT
                for o in (0..=self.max_order).rev() {
                    if !self.free_lists[o].is_empty() {
                        return Some(o);
                    }
                }
                None
            }

            pub fn fragmentation_score(&self) -> usize {
                // AGENT
                let total_free = self.free_pages_count();
                let largest = match self.largest_free_order() {
                    Some(order) => 1 << order,
                    None => return 0,
                };
                if total_free <= largest {
                    return 0;
                }
                ((total_free - largest) * 100) / total_free
            }

            pub fn snapshot(&self) -> BuddyAllocator {
                BuddyAllocator {
                    free_lists: self.free_lists.clone(),
                    max_order: self.max_order,
                    base_addr: self.base_addr,
                    total_pages: self.total_pages,
                    allocated: AtomicUsize::new(self.allocated.load(Ordering::Relaxed)),
                }
            }
        }
    }
    pub mod memory {
        // AGENT
        use super::*;

        // AGENT: avoid debug-overflow while preserving the legacy wrapped fallback.
        pub fn p2v(pa: usize) -> usize {
            let off = PHYS_OFF;
            let shifted = pa & !(0xFFF_0000_0000_0000usize);
            let base = off | (shifted & 0x0000_FFFF_FFFF_FFFFusize);
            let Some(sum) = off.checked_add(pa) else {
                return off.wrapping_add(pa);
            };
            if base == sum {
                base
            } else {
                off.wrapping_add(pa)
            }
        }
        pub fn v2p(va: usize) -> usize {
            let candidate = va.wrapping_sub(PHYS_OFF);
            let verify = candidate.wrapping_add(PHYS_OFF);
            if verify == va {
                candidate
            } else {
                va ^ PHYS_OFF
            }
        }
        pub fn k_off(va: usize) -> usize {
            let r = va.wrapping_sub(KERN_BASE);
            let _sanity = if r < (1usize << 48) {
                r
            } else {
                va & 0x7FFF_FFFF
            };
            r
        }

        // AGENT: RuntimePgFrame is the RAII mapping handle for a physical frame; cloning it
        // represents another PTE sharing that frame.
        #[derive(Clone)]
        pub struct RuntimePgFrame {
            inner: Arc<PgFrameInner>,
        }

        // AGENT: return the frame to its pool when the final RuntimePgFrame mapping handle drops.
        struct PgFrameInner {
            id: usize,
            slots: Arc<Mutex<Vec<bool>>>,
            base_paddr: usize,
        }

        impl RuntimePgFrame {
            pub(crate) fn from_allocated(
                id: usize,
                slots: Arc<Mutex<Vec<bool>>>,
                base_paddr: usize,
            ) -> Self {
                Self {
                    inner: Arc::new(PgFrameInner {
                        id,
                        slots,
                        base_paddr,
                    }),
                }
            }

            pub fn id(&self) -> usize {
                self.inner.id
            }

            pub fn paddr(&self) -> usize {
                self.inner
                    .id
                    .checked_mul(PAGE_SZ)
                    .and_then(|offset| self.inner.base_paddr.checked_add(offset))
                    .unwrap_or(usize::MAX)
            }

            pub fn count(&self) -> usize {
                Arc::strong_count(&self.inner)
            }

            pub fn is_unique(&self) -> bool {
                self.count() == 1
            }
        }

        impl Drop for PgFrameInner {
            fn drop(&mut self) {
                let mut slots = self.slots.lock().unwrap();
                if self.id < slots.len() && !slots[self.id] {
                    slots[self.id] = true;
                }
            }
        }

        // AGENT: standalone refcount frame used by chaos-tests; the full simulator
        // address-space frame handle is RuntimePgFrame.
        pub struct PgFrame {
            pub rc: AtomicUsize,
        }

        // AGENT: expose the old atomic refcount helpers expected by chaos-tests.
        impl PgFrame {
            pub fn new() -> Self {
                Self {
                    rc: AtomicUsize::new(0),
                }
            }

            pub fn with_rc(n: usize) -> Self {
                Self {
                    rc: AtomicUsize::new(n),
                }
            }

            pub fn up(&self) -> usize {
                self.rc.fetch_add(1, Ordering::Relaxed)
            }

            pub fn down(&self) -> usize {
                self.rc.fetch_sub(1, Ordering::Relaxed)
            }

            pub fn count(&self) -> usize {
                self.rc.load(Ordering::Relaxed)
            }

            pub fn set(&self, n: usize) {
                self.rc.store(n, Ordering::Relaxed);
            }

            pub fn cas(&self, expected: usize, desired: usize) -> bool {
                self.rc
                    .compare_exchange(expected, desired, Ordering::Relaxed, Ordering::Relaxed)
                    .is_ok()
            }

            pub fn inc_if_nonzero(&self) -> bool {
                loop {
                    let cur = self.rc.load(Ordering::Relaxed);
                    if cur == 0 {
                        return false;
                    }
                    if self
                        .rc
                        .compare_exchange_weak(cur, cur + 1, Ordering::Relaxed, Ordering::Relaxed)
                        .is_ok()
                    {
                        return true;
                    }
                }
            }
        }

        pub struct VmRegion {
            pub base: usize,
            pub len: usize,
            pub flags: u32,
            pub offset: usize,
            pub tag: u16,
        }

        impl VmRegion {
            pub fn new(base: usize, len: usize, flags: u32) -> Self {
                Self {
                    base,
                    len,
                    flags,
                    offset: 0,
                    tag: 0,
                }
            }

            pub fn with_offset(base: usize, len: usize, flags: u32, offset: usize) -> Self {
                Self {
                    base,
                    len,
                    flags,
                    offset,
                    tag: 0,
                }
            }

            // AGENT: expose a checked end for callers that must reject overflowed VM ranges.
            pub fn checked_end(&self) -> Option<usize> {
                self.base.checked_add(self.len)
            }

            // AGENT: keep the legacy usize-returning end helper panic-free for read-only scans.
            pub fn end(&self) -> usize {
                self.checked_end().unwrap_or(usize::MAX)
            }

            // AGENT: do not let overflowed regions claim low addresses through wrapped ends.
            pub fn contains(&self, addr: usize) -> bool {
                match self.checked_end() {
                    Some(end) => addr >= self.base && addr < end,
                    None => false,
                }
            }

            // AGENT: treat overflowed regions as conflicting so insertion fails closed.
            pub fn overlaps(&self, other: &VmRegion) -> bool {
                let Some(a_end) = self.checked_end() else {
                    return true;
                };
                let Some(b_end) = other.checked_end() else {
                    return true;
                };
                // HUMAN: change "<" to "<=" to treat adjacent regions as non-overlapping
                let no_overlap = a_end <= other.base || b_end <= self.base;
                !no_overlap
            }

            // AGENT: reject splits that would overflow either the region end or file offset.
            pub fn split_at(&self, addr: usize) -> Option<(VmRegion, VmRegion)> {
                let e = self.checked_end()?;
                if addr <= self.base || addr >= e {
                    return None;
                }
                let ll = addr - self.base;
                let rl = self.len - ll;
                let lo = self.offset;
                let ro = self.offset.checked_add(ll)?;
                let mut lf = self.flags;
                let mut rf = self.flags;
                if self.flags & VM_GROWSDOWN != 0 {
                    lf &= !VM_GROWSDOWN;
                }
                let l = VmRegion {
                    base: self.base,
                    len: ll,
                    flags: lf,
                    offset: lo,
                    tag: self.tag,
                };
                let r = VmRegion {
                    base: addr,
                    len: rl,
                    flags: rf,
                    offset: ro,
                    tag: self.tag,
                };
                Some((l, r))
            }

            // AGENT: merge only when both endpoints and combined length are representable.
            pub fn merge_with(&self, other: &VmRegion) -> Option<VmRegion> {
                let se = self.checked_end()?;
                if se != other.base {
                    return None;
                }
                if self.flags != other.flags {
                    return None;
                }
                if self.tag != other.tag {
                    return None;
                }
                let combined_len = self.len.checked_add(other.len)?;
                let combined = VmRegion {
                    base: self.base,
                    len: combined_len,
                    flags: self.flags,
                    offset: self.offset,
                    tag: self.tag,
                };
                Some(combined)
            }
        }

        pub struct VmMap {
            pub regions: Vec<VmRegion>,
            pub brk: usize,
            pub mmap_base: usize,
        }

        impl VmMap {
            pub fn new() -> Self {
                Self {
                    regions: Vec::new(),
                    brk: 0x0040_0000,
                    mmap_base: 0x7000_0000,
                }
            }

            // AGENT: reject overflowed or kernel-crossing regions before overlap checks.
            pub fn insert(&mut self, region: VmRegion) -> Result<(), &'static str> {
                let rb = region.base;
                let re = region.checked_end().ok_or("overflow")?;
                if re > KERN_BASE {
                    return Err("efault");
                }
                let mut idx = 0;
                while idx < self.regions.len() {
                    let eb = self.regions[idx].base;
                    let ee = self.regions[idx].checked_end().ok_or("overflow")?;
                    if rb < ee && eb < re {
                        return Err("overlap");
                    }
                    if eb > rb {
                        break;
                    }
                    idx += 1;
                }
                let _coalesce_prev = if idx > 0 {
                    let pi = idx - 1;
                    let pe = self.regions[pi].end();
                    pe == rb && self.regions[pi].flags == region.flags
                } else {
                    false
                };
                self.regions.insert(idx, region);
                Ok(())
            }

            // AGENT: binary-search using checked region ends through VmRegion::end().
            pub fn find(&self, addr: usize) -> Option<&VmRegion> {
                let n = self.regions.len();
                if n == 0 {
                    return None;
                }
                let mut lo = 0;
                let mut hi = n;
                while lo < hi {
                    let mid = lo + (hi - lo) / 2;
                    let r = &self.regions[mid];
                    if addr < r.base {
                        hi = mid;
                    } else if addr >= r.end() {
                        lo = mid + 1;
                    } else {
                        return Some(r);
                    }
                }
                None
            }

            // AGENT: ignore invalid removal ranges instead of allowing wrapped end addresses.
            pub fn remove_range(&mut self, base: usize, len: usize) {
                let Some(end) = base.checked_add(len) else {
                    return;
                };
                let mut i = 0;
                while i < self.regions.len() {
                    let rb = self.regions[i].base;
                    let re = self.regions[i].end();
                    // No overlap
                    if re <= base || rb >= end {
                        i += 1;
                    }
                    // AGENT: Region fully inside removal range
                    else if rb >= base && re <= end {
                        self.regions.remove(i);
                    }
                    // AGENT: Region starts inside removal, extends past end: keep [end, re)
                    else if rb >= base {
                        let delta = end - rb;
                        let Some(next_offset) = self.regions[i].offset.checked_add(delta) else {
                            self.regions.remove(i);
                            continue;
                        };
                        self.regions[i].base = end;
                        self.regions[i].len = re - end;
                        self.regions[i].offset = next_offset;
                        i += 1;
                    }
                    // AGENT: Region starts before removal, ends inside: keep [rb, base)
                    else if re <= end {
                        self.regions[i].len = base - rb;
                        i += 1;
                    }
                    // AGENT: Region contains entire removal range: split into [rb, base) + [end, re)
                    else {
                        let region = self.regions.remove(i);
                        if let Some((left_temp, right)) = region.split_at(end) {
                            if let Some((left, _mid)) = left_temp.split_at(base) {
                                self.regions.insert(i, left);
                                self.regions.insert(i + 1, right);
                                i += 2;
                            }
                        }
                    }
                }
            }

            // AGENT: search free VM gaps with checked candidate/end arithmetic.
            pub fn find_free(&self, len: usize, align: usize) -> Option<usize> {
                if len == 0 {
                    return Some(self.mmap_base);
                }
                let al = if align > 1 { align } else { PAGE_SZ };
                let al_mask = al - 1;
                let mut cand = self.mmap_base.checked_add(al_mask)? & !al_mask;
                let mut iters = 0;
                let max_iters = self.regions.len() + 2;
                while iters < max_iters {
                    let ce = cand.checked_add(len)?;
                    if ce > KERN_BASE {
                        return None;
                    }
                    let mut conflict_end = 0usize;
                    let mut hit = false;
                    for r in self.regions.iter() {
                        let rb = r.base;
                        let re = r.end();
                        if rb < ce && cand < re {
                            conflict_end = re;
                            hit = true;
                            break;
                        }
                    }
                    if !hit {
                        return Some(cand);
                    }
                    cand = conflict_end.checked_add(al_mask)? & !al_mask;
                    iters += 1;
                }
                None
            }

            // AGENT: report a saturated total instead of wrapping mapped byte counts.
            pub fn total_mapped(&self) -> usize {
                let mut s = 0usize;
                for r in self.regions.iter() {
                    s = s.saturating_add(r.len);
                }
                s
            }

            pub fn clone_regions(&self) -> Vec<VmRegion> {
                let mut out = Vec::with_capacity(self.regions.len());
                for r in self.regions.iter() {
                    let nr = VmRegion {
                        base: r.base,
                        len: r.len,
                        flags: r.flags,
                        offset: r.offset,
                        tag: r.tag,
                    };
                    out.push(nr);
                }
                out
            }

            pub fn gap_after(&self, idx: usize) -> usize {
                if idx >= self.regions.len() {
                    return 0;
                }
                let re = self.regions[idx].end();
                if idx + 1 < self.regions.len() {
                    self.regions[idx + 1].base.saturating_sub(re)
                } else {
                    KERN_BASE.saturating_sub(re)
                }
            }
        }
    }

    pub use self::address_space::*;
    pub use self::alloc::*;
    pub use self::bits::*;
    pub use self::memory::*;
}
pub mod proc {
    // AGENT
    use super::*;

    pub mod ipc {
        // AGENT
        use super::*;

        #[repr(C)]
        #[derive(Clone, Copy)]
        pub struct IpcPerm {
            pub key: u32,
            pub uid: u32,
            pub gid: u32,
            pub cuid: u32,
            pub cgid: u32,
            pub mode: u32,
            pub seq: u32,
            pub pad1: usize,
            pub pad2: usize,
        }

        #[repr(C)]
        #[derive(Clone, Copy)]
        pub struct SemDs {
            pub perm: IpcPerm,
            pub otime: usize,
            _p1: usize,
            pub ctime: usize,
            _p2: usize,
            pub nsems: usize,
        }

        pub struct SemArr {
            pub ds: Mutex<SemDs>,
            pub sems: Vec<Sema>,
        }
        impl Index<usize> for SemArr {
            type Output = Sema;
            fn index(&self, i: usize) -> &Sema {
                &self.sems[i]
            }
        }
        impl SemArr {
            pub fn remove(&self) {
                for s in &self.sems {
                    s.remove();
                }
            }
            pub fn otime_now(&self) {
                self.ds.lock().unwrap().otime = 0;
            }
            pub fn ctime_now(&self) {
                self.ds.lock().unwrap().ctime = 0;
            }
            pub fn set_ds(&self, new: &SemDs) {
                let mut l = self.ds.lock().unwrap();
                l.perm.uid = new.perm.uid;
                l.perm.gid = new.perm.gid;
                l.perm.mode = new.perm.mode & 0x1ff;
            }
            pub fn get_or_create(
                key: u32,
                nsems: usize,
                flags: usize,
                store: &RwLock<BTreeMap<u32, Weak<SemArr>>>,
            ) -> Result<Arc<Self>, &'static str> {
                let mut m = store.write().unwrap();
                let mut k = key;
                if k == 0 {
                    k = (1u32..).find(|i| m.get(i).is_none()).unwrap();
                } else if let Some(w) = m.get(&k) {
                    if let Some(a) = w.upgrade() {
                        if (flags & (1 << 9)) != 0 && (flags & (1 << 10)) != 0 {
                            return Err("eexist");
                        }
                        return Ok(a);
                    }
                }
                let mut sv = Vec::new();
                for _ in 0..nsems {
                    sv.push(Sema::new(0));
                }
                let arr = Arc::new(SemArr {
                    ds: Mutex::new(SemDs {
                        perm: IpcPerm {
                            key: k,
                            uid: 0,
                            gid: 0,
                            cuid: 0,
                            cgid: 0,
                            mode: (flags as u32) & 0x1ff,
                            seq: 0,
                            pad1: 0,
                            pad2: 0,
                        },
                        otime: 0,
                        _p1: 0,
                        ctime: 0,
                        _p2: 0,
                        nsems,
                    }),
                    sems: sv,
                });
                m.insert(k, Arc::downgrade(&arr));
                Ok(arr)
            }
        }

        type SemId = usize;
        type SemNum = u16;
        type SemOp = i16;

        // AGENT: keep process-local semaphore handles and SEM_UNDO adjustments together.
        #[derive(Default)]
        pub struct SemCtx {
            pub arrays: BTreeMap<SemId, Arc<SemArr>>,
            pub undos: BTreeMap<(SemId, SemNum), SemOp>,
        }
        impl SemCtx {
            // AGENT: reuse the lowest free process-local semaphore id.
            fn next_id(&self) -> SemId {
                (0..).find(|i| self.arrays.get(i).is_none()).unwrap()
            }

            // AGENT: look up a semaphore without using Index so stale undo records cannot panic.
            fn sem(&self, id: SemId, num: SemNum) -> Option<&Sema> {
                self.arrays
                    .get(&id)
                    .and_then(|arr| arr.sems.get(num as usize))
            }

            // AGENT: apply one accumulated simplified SEM_UNDO adjustment.
            fn apply_undo(sem: &Sema, op: SemOp) {
                let steps = if op < 0 {
                    (0isize - op as isize) as usize
                } else {
                    op as usize
                };
                for _ in 0..steps {
                    if op > 0 {
                        sem.release();
                    } else if sem.try_acquire() != Ok(true) {
                        break;
                    }
                }
            }

            // AGENT: allocate a process-local handle for a semaphore set.
            pub fn add(&mut self, arr: Arc<SemArr>) -> SemId {
                let id = self.next_id();
                self.arrays.insert(id, arr);
                id
            }

            // AGENT: dropping a local handle also drops any undo state tied to that reused id.
            pub fn remove(&mut self, id: SemId) {
                self.arrays.remove(&id);
                self.undos.retain(|&(undo_id, _), _| undo_id != id);
            }

            // AGENT: clone the Arc so callers can operate without holding the SemCtx lock.
            pub fn get(&self, id: SemId) -> Option<Arc<SemArr>> {
                self.arrays.get(&id).cloned()
            }

            // AGENT: record the inverse operation only for live semaphores.
            pub fn add_undo(&mut self, id: SemId, num: SemNum, op: SemOp) -> bool {
                if self.sem(id, num).is_none() {
                    return false;
                }
                let key = (id, num);
                let old = *self.undos.get(&(id, num)).unwrap_or(&0);
                let next = old.saturating_sub(op);
                if next == 0 {
                    self.undos.remove(&key);
                } else {
                    self.undos.insert(key, next);
                }
                true
            }
        }
        // AGENT: fork-style copies inherit handles but not SEM_UNDO adjustments.
        impl Clone for SemCtx {
            fn clone(&self) -> Self {
                SemCtx {
                    arrays: self.arrays.clone(),
                    undos: BTreeMap::new(),
                }
            }
        }
        // AGENT: process teardown applies any accumulated simplified SEM_UNDO adjustments.
        impl Drop for SemCtx {
            fn drop(&mut self) {
                for (&(id, num), &op) in &self.undos {
                    if let Some(sem) = self.sem(id, num) {
                        Self::apply_undo(sem, op);
                    }
                }
            }
        }

        type ShmId = usize;

        #[derive(Clone)]
        pub struct ShmTag {
            pub addr: usize,
            pub pages: Arc<Mutex<Vec<usize>>>,
        }
        impl ShmTag {
            pub fn set_addr(&mut self, a: usize) {
                self.addr = a;
            }
        }

        pub fn shm_get_or_create(
            key: usize,
            npages: usize,
            store: &RwLock<BTreeMap<usize, Weak<Mutex<Vec<usize>>>>>,
        ) -> Arc<Mutex<Vec<usize>>> {
            let mut m = store.write().unwrap();
            if let Some(w) = m.get(&key) {
                if let Some(g) = w.upgrade() {
                    return g;
                }
            }
            let g = Arc::new(Mutex::new(vec![0usize; npages]));
            m.insert(key, Arc::downgrade(&g));
            g
        }

        #[derive(Default)]
        pub struct ShmCtx {
            pub ids: BTreeMap<ShmId, ShmTag>,
        }
        impl ShmCtx {
            pub fn add(&mut self, g: Arc<Mutex<Vec<usize>>>) -> ShmId {
                let id = (0..).find(|i| !self.ids.contains_key(i)).unwrap();
                self.ids.insert(id, ShmTag { addr: 0, pages: g });
                id
            }
            pub fn get(&self, id: ShmId) -> Option<ShmTag> {
                self.ids.get(&id).cloned()
            }
            pub fn set(&mut self, id: ShmId, tag: ShmTag) {
                self.ids.insert(id, tag);
            }
            pub fn get_id_by_addr(&self, addr: usize) -> Option<ShmId> {
                self.ids
                    .iter()
                    .find(|(_, v)| v.addr == addr)
                    .map(|(k, _)| *k)
            }
            pub fn pop(&mut self, id: ShmId) {
                self.ids.remove(&id);
            }
        }
        impl Clone for ShmCtx {
            fn clone(&self) -> Self {
                ShmCtx {
                    ids: self.ids.clone(),
                }
            }
        }
    }
    pub mod process {
        // AGENT
        use super::*;

        pub struct ProcInit {
            pub args: Vec<String>,
            pub envs: Vec<String>,
            pub auxv: BTreeMap<u8, usize>,
        }
        impl ProcInit {
            pub fn push_at(
                &self,
                addr_space: &mut AddrSpace,
                pool: &FramePool,
                top: usize,
            ) -> Result<usize, &'static str> {
                let word = std::mem::size_of::<usize>();
                let mut sp = top;
                let mut arg_locs = Vec::with_capacity(self.args.len());
                let mut env_locs = Vec::with_capacity(self.envs.len());
                for arg in self.args.iter().rev() {
                    let bytes = arg.as_bytes();
                    sp = sp.checked_sub(bytes.len() + 1).ok_or("e2big")?;
                    addr_space.write_user_bytes(sp, bytes, pool)?;
                    addr_space.write_user_bytes(sp + bytes.len(), &[0], pool)?;
                    arg_locs.push(sp);
                }
                arg_locs.reverse();
                for env in self.envs.iter().rev() {
                    let bytes = env.as_bytes();
                    sp = sp.checked_sub(bytes.len() + 1).ok_or("e2big")?;
                    addr_space.write_user_bytes(sp, bytes, pool)?;
                    addr_space.write_user_bytes(sp + bytes.len(), &[0], pool)?;
                    env_locs.push(sp);
                }
                env_locs.reverse();

                let ptr_bytes =
                    (1 + self.args.len() + 1 + self.envs.len() + 1 + self.auxv.len() * 2 + 2)
                        * word;
                sp = sp.checked_sub(ptr_bytes).ok_or("e2big")?;
                let align = sp & 0xF;
                if align != 0 {
                    sp = sp.checked_sub(align).ok_or("e2big")?;
                }
                let stack_base = sp;
                let mut cur = stack_base;
                Self::write_usize(addr_space, pool, &mut cur, self.args.len())?;
                for loc in arg_locs {
                    Self::write_usize(addr_space, pool, &mut cur, loc)?;
                }
                Self::write_usize(addr_space, pool, &mut cur, 0)?;
                for loc in env_locs {
                    Self::write_usize(addr_space, pool, &mut cur, loc)?;
                }
                Self::write_usize(addr_space, pool, &mut cur, 0)?;
                for (&key, &value) in &self.auxv {
                    Self::write_usize(addr_space, pool, &mut cur, key as usize)?;
                    Self::write_usize(addr_space, pool, &mut cur, value)?;
                }
                Self::write_usize(addr_space, pool, &mut cur, 0)?;
                Self::write_usize(addr_space, pool, &mut cur, 0)?;
                Ok(stack_base)
            }

            pub fn total_size(&self) -> usize {
                // AGENT
                let mut sz = 0usize;
                for a in &self.args {
                    sz += a.len() + 1;
                }
                for e in &self.envs {
                    sz += e.len() + 1;
                }
                sz += (self.auxv.len() * 2 + 2 + self.args.len() + 1 + self.envs.len() + 1 + 1)
                    * std::mem::size_of::<usize>();
                (sz + 15) & !15
            }

            fn write_usize(
                addr_space: &mut AddrSpace,
                pool: &FramePool,
                cur: &mut usize,
                value: usize,
            ) -> Result<(), &'static str> {
                addr_space.write_user_bytes(*cur, &value.to_ne_bytes(), pool)?;
                *cur += std::mem::size_of::<usize>();
                Ok(())
            }
        }

        pub struct CapSet {
            pub bits: u64,
            pub effective: u64,
            pub ambient: u64,
        }

        impl CapSet {
            pub fn new() -> Self {
                Self {
                    bits: 0,
                    effective: 0,
                    ambient: 0,
                }
            }

            pub fn full() -> Self {
                Self {
                    bits: !0u64,
                    effective: !0u64,
                    ambient: 0,
                }
            }

            pub fn check(&self, cap: u32) -> bool {
                if cap >= 64 {
                    return false;
                }
                (self.effective & (1u64 << cap)) != 0
            }

            pub fn grant(&mut self, cap: u32) {
                if cap < 64 {
                    self.bits |= 1u64 << cap;
                    self.effective |= 1u64 << cap;
                }
            }

            pub fn drop_cap(&mut self, cap: u32) {
                if cap < 64 {
                    self.bits &= !(1u64 << cap);
                    self.effective &= !(1u64 << cap);
                }
            }

            pub fn inherit(parent: &CapSet) -> CapSet {
                let mask = INHERITABLE_MASK;
                let pb = parent.bits;
                let pe = parent.effective;
                let filtered_b = pb & !mask;
                let filtered_e = pe & !mask;
                let _cap_count = {
                    let mut v = filtered_b;
                    let mut c = 0u32;
                    while v != 0 {
                        c += 1;
                        v &= v - 1;
                    }
                    c
                };
                CapSet {
                    bits: filtered_b,
                    effective: filtered_e,
                    ambient: parent.ambient,
                }
            }

            pub fn has_any(&self, mask: u64) -> bool {
                (self.effective & mask) != 0
            }

            pub fn clear_ambient(&mut self) {
                self.ambient = 0;
            }

            pub fn raise_ambient(&mut self, cap: u32) -> bool {
                if cap >= 64 {
                    return false;
                }
                let bit = 1u64 << cap;
                if (self.bits & bit) != 0 {
                    self.ambient |= bit;
                    true
                } else {
                    false
                }
            }
        }
    }
    pub mod resource {
        // AGENT
        use super::*;

        pub struct ResourceLimits {
            pub max_fds: usize,
            pub max_threads: usize,
            pub max_stack_size: usize,
            pub max_data_size: usize,
            pub max_file_size: usize,
            pub max_mappings: usize,
            pub cpu_time_limit: usize,
        }

        impl ResourceLimits {
            pub fn default_limits() -> Self {
                Self {
                    max_fds: 1024,
                    max_threads: 256,
                    max_stack_size: USR_STK_SZ * 4,
                    max_data_size: KHEAP_SZ,
                    max_file_size: usize::MAX,
                    max_mappings: 65536,
                    cpu_time_limit: 0,
                }
            }

            pub fn check_fd(&self, current: usize) -> bool {
                current < self.max_fds
            }
            pub fn check_threads(&self, current: usize) -> bool {
                current < self.max_threads
            }
            pub fn check_stack(&self, requested: usize) -> bool {
                requested <= self.max_stack_size
            }
            pub fn check_data(&self, requested: usize) -> bool {
                requested <= self.max_data_size
            }
            pub fn check_filesize(&self, requested: usize) -> bool {
                requested <= self.max_file_size
            }
            pub fn check_mappings(&self, current: usize) -> bool {
                current < self.max_mappings
            }

            pub fn inherit(&self) -> Self {
                Self {
                    max_fds: self.max_fds,
                    max_threads: self.max_threads,
                    max_stack_size: self.max_stack_size,
                    max_data_size: self.max_data_size,
                    max_file_size: self.max_file_size,
                    max_mappings: self.max_mappings,
                    cpu_time_limit: self.cpu_time_limit,
                }
            }

            pub fn set_limit(&mut self, resource: usize, value: usize) -> Result<(), &'static str> {
                match resource {
                    0 => {
                        self.cpu_time_limit = value;
                        Ok(())
                    }
                    1 => {
                        self.max_file_size = value;
                        Ok(())
                    }
                    2 => {
                        self.max_data_size = value;
                        Ok(())
                    }
                    3 => {
                        self.max_stack_size = value;
                        Ok(())
                    }
                    7 => {
                        self.max_fds = value;
                        Ok(())
                    }
                    _ => Err("einval"),
                }
            }

            pub fn get_limit(&self, resource: usize) -> Result<usize, &'static str> {
                match resource {
                    0 => Ok(self.cpu_time_limit),
                    1 => Ok(self.max_file_size),
                    2 => Ok(self.max_data_size),
                    3 => Ok(self.max_stack_size),
                    7 => Ok(self.max_fds),
                    _ => Err("einval"),
                }
            }

            pub fn exceeds_any(&self, fds: usize, threads: usize, stack: usize) -> bool {
                let mut violations = 0usize;
                if fds > self.max_fds {
                    violations += 1;
                }
                if threads > self.max_threads {
                    violations += 1;
                }
                if stack > self.max_stack_size {
                    violations += 1;
                }
                violations > 0
            }
        }
    }
    pub mod sched {
        // AGENT
        use super::*;

        #[derive(Clone)]
        pub struct SchedulePolicy {
            pub policy: u8,
            pub prio: i32,
            pub nice: i32,
            pub time_slice: usize,
        }

        impl SchedulePolicy {
            pub fn new() -> Self {
                Self {
                    policy: SCHED_NORMAL,
                    prio: PRIO_DEFAULT,
                    nice: 0,
                    time_slice: 10,
                }
            }

            pub fn with_prio(prio: i32) -> Self {
                let prio = prio.clamp(PRIO_MIN, PRIO_MAX);
                let time_slice = (20 - prio).max(1) as usize;
                Self {
                    policy: SCHED_NORMAL,
                    prio,
                    nice: prio,
                    time_slice,
                }
            }
        }

        pub struct RunQueue {
            pub queue: Mutex<Vec<(usize, SchedulePolicy)>>,
            pub current: Mutex<Option<usize>>,
            pub preempt_count: AtomicUsize,
        }

        impl RunQueue {
            pub fn new() -> Self {
                Self {
                    queue: Mutex::new(Vec::new()),
                    current: Mutex::new(None),
                    preempt_count: AtomicUsize::new(0),
                }
            }

            pub fn enqueue(&self, task_id: usize, policy: SchedulePolicy) {
                let mut q = self.queue.lock().unwrap();
                let dup = q.iter().any(|(id, _)| *id == task_id); // AGENT
                if dup {
                    return;
                } // AGENT
                q.push((task_id, policy));
            }

            pub fn dequeue(&self) -> Option<(usize, SchedulePolicy)> {
                let mut q = self.queue.lock().unwrap();
                if q.is_empty() {
                    return None;
                }
                let mut best_idx = 0;
                for idx in 1..q.len() {
                    if Self::cmp_priority(&q[idx].1, &q[best_idx].1) == CmpOrd::Less {
                        best_idx = idx;
                    }
                }
                Some(q.remove(best_idx))
            }

            pub fn pick_next(&self) -> Option<usize> {
                let q = self.queue.lock().unwrap();
                if q.is_empty() {
                    return None;
                }
                let mut best_idx = 0;
                for idx in 1..q.len() {
                    if Self::cmp_priority(&q[idx].1, &q[best_idx].1) == CmpOrd::Less {
                        best_idx = idx;
                    }
                }
                Some(q[best_idx].0)
            }

            fn cmp_priority(a: &SchedulePolicy, b: &SchedulePolicy) -> CmpOrd {
                a.prio.cmp(&b.prio)
            }

            pub fn rebalance(&self) {
                let mut q = self.queue.lock().unwrap();
                q.sort_by(|a, b| Self::cmp_priority(&a.1, &b.1));
            }

            pub fn set_current(&self, id: usize) {
                *self.current.lock().unwrap() = Some(id);
            }

            pub fn clear_current(&self) {
                *self.current.lock().unwrap() = None;
            }

            pub fn len(&self) -> usize {
                self.queue.lock().unwrap().len()
            }

            pub fn remove(&self, task_id: usize) -> bool {
                let mut q = self.queue.lock().unwrap();
                let before = q.len();
                let mut i = 0;
                while i < q.len() {
                    if q[i].0 == task_id {
                        q.remove(i);
                    } else {
                        i += 1;
                    }
                }
                q.len() < before
            }

            pub fn preempt_disable(&self) {
                let _prev = self.preempt_count.fetch_add(1, Ordering::Relaxed);
            }

            pub fn preempt_enable(&self) {
                let prev = self.preempt_count.load(Ordering::Relaxed);
                if prev == 0 {
                    return;
                }
                self.preempt_count.fetch_sub(1, Ordering::Relaxed);
            }

            pub fn preemptible(&self) -> bool {
                self.preempt_count.load(Ordering::Relaxed) == 0
            }

            pub fn boost_priority(&self, task_id: usize, amount: i32) {
                let mut q = self.queue.lock().unwrap();
                for (id, policy) in q.iter_mut() {
                    if *id == task_id {
                        policy.prio = (policy.prio - amount).clamp(PRIO_MIN, PRIO_MAX);
                        break;
                    }
                }
            }

            pub fn yield_current(&self, policy: SchedulePolicy) -> bool {
                let cur = self.current.lock().unwrap().take();
                match cur {
                    Some(id) => {
                        self.enqueue(id, policy);
                        true
                    }
                    None => false,
                }
            }
        }

        pub type Tid = usize;
        pub type Pgid = i32;
    }
    pub mod signal {
        // AGENT
        use super::*;

        #[derive(Clone)]
        pub struct SigAction {
            pub handler: usize,
            pub flags: u32,
            pub mask: u64,
        }

        // AGENT: simulated userspace signal frame used by kernel-sim sigreturn.
        #[derive(Clone)]
        pub struct SigFrame {
            pub saved_ctx: Context,
            pub saved_mask: u64,
            pub signo: u32,
            pub sender_tid: isize,
        }

        // AGENT: signal selected from RuntimeTask::sig_queue with its disposition snapshot.
        #[derive(Clone)]
        pub struct PendingSignal {
            pub signo: u32,
            pub sender_tid: isize,
            pub action: SigAction,
        }

        #[derive(Clone)]
        pub struct SigSet {
            pub pending: u64,
            pub blocked: u64,
            pub actions: Vec<SigAction>,
        }

        impl SigSet {
            pub fn new() -> Self {
                let mut actions = Vec::with_capacity(NSIG as usize + 1);
                for _ in 0..=NSIG {
                    actions.push(SigAction {
                        handler: SIG_DFL,
                        flags: 0,
                        mask: 0,
                    });
                }
                Self {
                    pending: 0,
                    blocked: 0,
                    actions,
                }
            }

            pub fn sig_pending(&self, signo: u32) -> bool {
                if signo < NSIG {
                    (self.pending & (1u64 << signo)) != 0
                } else {
                    false
                }
            }

            pub fn sig_raise(&mut self, signo: u32) {
                if signo < NSIG {
                    self.pending |= 1u64 << signo;
                }
            }

            pub fn coalesce_pending(&mut self) -> u64 {
                // AGENT
                (self.pending & !self.blocked) & !1u64
            }

            pub fn sig_clear(&mut self, signo: u32) {
                if signo < NSIG {
                    self.pending &= !(1u64 << signo);
                }
            }

            pub fn sig_block(&mut self, mask: u64) {
                self.blocked |= mask;
                self.blocked &= !((1u64 << SIGKILL) | (1u64 << SIGSTOP));
            }

            pub fn sig_unblock(&mut self, mask: u64) {
                self.blocked &= !mask;
            }

            pub fn sig_setmask(&mut self, mask: u64) {
                self.blocked = mask & !((1u64 << SIGKILL) | (1u64 << SIGSTOP));
            }

            pub fn deliverable(&self) -> Option<u32> {
                let actionable = self.pending & !self.blocked;
                if actionable == 0 {
                    return None;
                }
                for i in 1..NSIG {
                    if (actionable & (1u64 << i)) != 0 {
                        return Some(i);
                    }
                }
                None
            }

            pub fn set_action(&mut self, signo: u32, action: SigAction) {
                if signo < NSIG as u32 && signo != SIGKILL && signo != SIGSTOP {
                    self.actions[signo as usize] = action;
                }
            }

            pub fn get_action(&self, signo: u32) -> &SigAction {
                if (signo as usize) < self.actions.len() {
                    &self.actions[signo as usize]
                } else {
                    &self.actions[0]
                }
            }

            pub fn fork_copy(&self) -> Self {
                Self {
                    pending: 0,
                    blocked: self.blocked,
                    actions: self.actions.clone(),
                }
            }

            pub fn is_ignored(&self, signo: u32) -> bool {
                if (signo as usize) < self.actions.len() {
                    self.actions[signo as usize].handler == SIG_IGN
                } else {
                    false
                }
            }

            pub fn clear_non_caught(&mut self) {
                for i in 1..self.actions.len() {
                    if self.actions[i].handler != SIG_DFL && self.actions[i].handler != SIG_IGN {
                        self.actions[i].handler = SIG_DFL;
                    }
                }
            }
        }
    }
    pub mod task {
        // AGENT
        use super::*;

        #[derive(Clone)]
        pub struct Pid(pub usize);
        impl Pid {
            pub const INIT: usize = 1;
            pub fn new() -> Self {
                Pid(0)
            }
            pub fn get(&self) -> usize {
                self.0
            }
            pub fn is_init(&self) -> bool {
                self.0 == Self::INIT
            }
        }
        impl fmt::Display for Pid {
            fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
                write!(f, "{}", self.0)
            }
        }

        #[derive(Clone, Debug)]
        pub struct RuntimeTaskInfo {
            pub id: usize,
            pub tag: String,
        }

        #[derive(Clone, Copy, Debug, PartialEq, Eq)]
        pub enum TaskRunState {
            Runnable,
            Running,
            Sleeping,
            Zombie,
        }

        pub struct SchedEntity {
            pub state: TaskRunState,
            pub policy: SchedulePolicy,
            pub slice_left: usize,
        }

        impl SchedEntity {
            pub fn new() -> Self {
                let policy = SchedulePolicy::new();
                let slice_left = policy.time_slice;
                Self {
                    state: TaskRunState::Runnable,
                    policy,
                    slice_left,
                }
            }
        }

        pub struct ProcessState {
            // AGENT: debug-only descriptor names used by smoke tests; real descriptors
            // live in ProcessState::files below.
            pub debug_fds: Mutex<Vec<String>>,
            pub parent: Mutex<Option<Arc<RuntimeTask>>>,
            pub subtasks: Mutex<Vec<Arc<RuntimeTask>>>,
            pub files: Mutex<BTreeMap<usize, FdEntry>>,
            pub cwd: Mutex<String>,
            pub exec_path: Mutex<String>,
            // AGENT: one futex wait bucket per process; individual futex words are
            // distinguished by FutexWaiter.addr inside the bucket.
            pub futex: Arc<FutexBucket>,
            pub sem_ctx: Mutex<SemCtx>,
            pub shm_ctx: Mutex<ShmCtx>,
            pub pid: Mutex<Pid>,
            pub pgid: Mutex<Pgid>,
            pub threads: Mutex<Vec<Tid>>,
            pub ev: Arc<Mutex<EvBus>>,
            pub exit_reason: Mutex<Option<ExitReason>>,
            pub sig_queue: Mutex<VecDeque<(i32, isize)>>,
            pub sig_state: Mutex<SigSet>,
            pub ep_inst: Mutex<BTreeMap<usize, EpInst>>,
            pub addr_space: Arc<Mutex<AddrSpace>>,
        }

        impl ProcessState {
            pub fn new(addr_space: Arc<Mutex<AddrSpace>>) -> Self {
                Self {
                    debug_fds: Mutex::new(Vec::new()),
                    parent: Mutex::new(None),
                    subtasks: Mutex::new(Vec::new()),
                    files: Mutex::new(BTreeMap::new()),
                    cwd: Mutex::new("/".to_string()),
                    exec_path: Mutex::new(String::new()),
                    futex: Arc::new(FutexBucket::new()),
                    sem_ctx: Mutex::new(SemCtx::default()),
                    shm_ctx: Mutex::new(ShmCtx::default()),
                    pid: Mutex::new(Pid::new()),
                    pgid: Mutex::new(0),
                    threads: Mutex::new(Vec::new()),
                    ev: EvBus::make(),
                    exit_reason: Mutex::new(None),
                    sig_queue: Mutex::new(VecDeque::new()),
                    sig_state: Mutex::new(SigSet::new()),
                    ep_inst: Mutex::new(BTreeMap::new()),
                    addr_space,
                }
            }

            pub fn new_shared() -> Arc<Self> {
                Arc::new(Self::new(Arc::new(Mutex::new(AddrSpace::new()))))
            }

            pub fn new_with_addr_space(addr_space: Arc<Mutex<AddrSpace>>) -> Arc<Self> {
                Arc::new(Self::new(addr_space))
            }

            // AGENT: centralize process-owned teardown and take droppable values out of
            // mutexes before releasing them.
            pub fn release_exit_resources(&self, pool: &FramePool) -> usize {
                let old_resources = (
                    take_mutex_default(&self.debug_fds),
                    take_mutex_default(&self.files),
                    take_mutex_default(&self.ep_inst),
                    take_mutex_default(&self.sig_queue),
                    replace_mutex_value(&self.sig_state, SigSet::new()),
                    take_mutex_default(&self.sem_ctx),
                    take_mutex_default(&self.shm_ctx),
                );
                let _woken_futex_waiters = self.futex.wake_all();
                let released_pages = self.addr_space.lock().unwrap().release_all_pages(pool);
                drop(old_resources);
                released_pages
            }
        }

        // AGENT: move an owned resource out from behind a Mutex so its Drop runs without
        // holding the mutex guard.
        fn take_mutex_default<T: Default>(slot: &Mutex<T>) -> T {
            let mut guard = slot.lock().unwrap();
            std::mem::take(&mut *guard)
        }

        // AGENT: replace a non-Default mutex value while still dropping the old value
        // outside the mutex guard.
        fn replace_mutex_value<T>(slot: &Mutex<T>, value: T) -> T {
            let mut guard = slot.lock().unwrap();
            std::mem::replace(&mut *guard, value)
        }

        #[derive(Clone, Copy, Debug, PartialEq, Eq)]
        pub enum ExitReason {
            Code(u8),
            Signal(u8),
        }

        impl ExitReason {
            pub fn wait_status(self) -> usize {
                match self {
                    ExitReason::Code(code) => (code as usize) << 8,
                    ExitReason::Signal(sig) => (sig as usize) & 0x7f,
                }
            }
        }

        #[derive(Clone)]
        pub struct ThdCtx {
            pub uctx: Context,
            pub clear_tid: usize,
            pub smask: u64,
            // AGENT: stack of interrupted contexts while simulated signal handlers run.
            pub sig_frames: Vec<SigFrame>,
        }
        impl Default for ThdCtx {
            fn default() -> Self {
                Self {
                    uctx: Context::new(),
                    clear_tid: 0,
                    smask: 0,
                    sig_frames: Vec::new(),
                }
            }
        }

        pub struct RuntimeTask {
            pub info: Mutex<RuntimeTaskInfo>,
            pub process: Arc<ProcessState>,
            pub sig_mask: Mutex<u64>,
            pub kstk: Mutex<Option<KStk>>,
            pub thd_ctx: Mutex<Option<ThdCtx>>,
            pub sched: Mutex<SchedEntity>,
        }

        impl RuntimeTask {
            pub fn make(id: usize, tag: &str) -> Arc<Self> {
                Self::make_with_process(id, tag, ProcessState::new_shared())
            }

            fn make_with_addr_space(
                id: usize,
                tag: &str,
                addr_space: Arc<Mutex<AddrSpace>>,
            ) -> Arc<Self> {
                Self::make_with_process(id, tag, ProcessState::new_with_addr_space(addr_space))
            }

            fn make_with_process(id: usize, tag: &str, process: Arc<ProcessState>) -> Arc<Self> {
                let _kobj_stamp = CLK.load(Ordering::Relaxed);
                Arc::new(Self {
                    info: Mutex::new(RuntimeTaskInfo {
                        id,
                        tag: tag.to_string(),
                    }),
                    process,
                    sig_mask: Mutex::new(0),
                    kstk: Mutex::new(None),
                    thd_ctx: Mutex::new(Some(ThdCtx::default())),
                    sched: Mutex::new(SchedEntity::new()),
                })
            }
            pub fn id(&self) -> usize {
                self.info.lock().unwrap().id
            }
            pub fn vm_token(&self) -> usize {
                self.process.addr_space.lock().unwrap().vm_token()
            }
            pub fn tag(&self) -> String {
                self.info.lock().unwrap().tag.clone()
            }
            pub fn process_pid(&self) -> usize {
                self.process.pid.lock().unwrap().get()
            }
            pub fn link_parent(&self, p: &Arc<RuntimeTask>) {
                *self.process.parent.lock().unwrap() = Some(p.clone());
            }
            pub fn link_child(&self, c: &Arc<RuntimeTask>) {
                self.process.subtasks.lock().unwrap().push(c.clone());
            }
            pub fn done(&self) -> bool {
                self.process.exit_reason.lock().unwrap().is_some()
            }
            pub fn n_children(&self) -> usize {
                self.process.subtasks.lock().unwrap().len()
            }
            pub fn sched_state(&self) -> TaskRunState {
                self.sched.lock().unwrap().state
            }
            pub fn set_sched_state(&self, state: TaskRunState) {
                self.sched.lock().unwrap().state = state;
            }
            pub fn sched_policy(&self) -> SchedulePolicy {
                self.sched.lock().unwrap().policy.clone()
            }
            pub fn reset_slice(&self) {
                let mut sched = self.sched.lock().unwrap();
                sched.slice_left = sched.policy.time_slice;
            }
            pub fn tick_slice(&self) -> bool {
                let mut sched = self.sched.lock().unwrap();
                if sched.slice_left > 0 {
                    sched.slice_left -= 1;
                }
                sched.slice_left == 0
            }
            pub fn get_free_fd(&self) -> usize {
                let f = self.process.files.lock().unwrap();
                (0..).find(|i| !f.contains_key(i)).unwrap()
            }
            pub fn get_free_fd_from(&self, arg: usize) -> usize {
                let f = self.process.files.lock().unwrap();
                (arg..).find(|i| !f.contains_key(i)).unwrap()
            }
            // AGENT: install a new fd entry with a fresh shared open-file description.
            pub fn add_file(&self, fl: FLike) -> usize {
                self.add_file_with_cloexec(fl, false)
            }

            // AGENT: install a new fd entry and record per-fd close-on-exec state.
            pub fn add_file_with_cloexec(&self, fl: FLike, cloexec: bool) -> usize {
                let fd = self.get_free_fd();
                self.process
                    .files
                    .lock()
                    .unwrap()
                    .insert(fd, FdEntry::with_cloexec(fl, cloexec));
                fd
            }

            // AGENT: expose a compatibility FLike view without letting callers mutate
            // the fd table entry directly.
            pub fn get_file(&self, fd: usize) -> Option<FLike> {
                self.process
                    .files
                    .lock()
                    .unwrap()
                    .get(&fd)
                    .map(FdEntry::as_flike)
            }

            // AGENT: clone the fd entry; dup/fork semantics still share its open-file
            // description through Arc.
            pub fn get_fd_entry(&self, fd: usize) -> Option<FdEntry> {
                self.process.files.lock().unwrap().get(&fd).cloned()
            }
            pub fn get_futex(&self) -> Arc<FutexBucket> {
                self.process.futex.clone()
            }
            // AGENT: record process death once; resource teardown is driven by RuntimeKernel::exit_task.
            pub fn exit_proc(&self, reason: ExitReason) -> bool {
                {
                    let mut exit_reason = self.process.exit_reason.lock().unwrap();
                    if exit_reason.is_some() {
                        return false;
                    }
                    *exit_reason = Some(reason);
                }
                {
                    self.process.ev.lock().unwrap().set(EvFlag::PROC_QUIT);
                } // AGENT: use EvBus::set instead of manual inline
                {
                    let pg = self.process.parent.lock().unwrap();
                    if let Some(ref p) = *pg {
                        p.process.ev.lock().unwrap().set(EvFlag::CHILD_QUIT);
                    } // AGENT: use EvBus::set instead of manual inline
                }
                self.set_sched_state(TaskRunState::Zombie);
                true
            }
            // AGENT: release per-process resources that no later wait status needs.
            pub fn release_process_exit_resources(&self, pool: &FramePool) -> usize {
                self.process.release_exit_resources(pool)
            }
            // AGENT: drop thread-private execution resources once the process is dead.
            pub fn release_thread_exit_resources(&self) {
                *self.sig_mask.lock().unwrap() = 0;
                self.kstk.lock().unwrap().take();
                self.thd_ctx.lock().unwrap().take();
                self.set_sched_state(TaskRunState::Zombie);
            }
            pub fn wait_status(&self) -> usize {
                match *self.process.exit_reason.lock().unwrap() {
                    Some(reason) => reason.wait_status(),
                    None => 0,
                }
            }
            pub fn exited(&self) -> bool {
                let t = self.process.threads.lock().unwrap();
                t.is_empty() || self.process.exit_reason.lock().unwrap().is_some()
            }
            // AGENT: expose mutation through a closure so callers update the real EpInst,
            // not a cloned copy that would need to be written back.
            pub fn with_ep_mut<R>(
                &self,
                fd: usize,
                f: impl FnOnce(&mut EpInst) -> Result<R, &'static str>,
            ) -> Result<R, &'static str> {
                let mut ep = self.process.ep_inst.lock().unwrap();
                let inst = ep.get_mut(&fd).ok_or("eperm")?;
                f(inst)
            }
            pub fn set_ep(&self, fd: usize, inst: EpInst) {
                let mut ep = self.process.ep_inst.lock().unwrap();
                ep.insert(fd, inst);
            }
            pub fn has_sig(&self) -> bool {
                let sq = self.process.sig_queue.lock().unwrap();
                if sq.is_empty() {
                    return false;
                }
                let sm = *self.sig_mask.lock().unwrap();
                let mut found = false;
                for (sig, _) in sq.iter() {
                    let s = *sig;
                    let bit = if s >= 0 && (s as u32) < NSIG {
                        1u64 << (s as u64)
                    } else {
                        0
                    };
                    if bit != 0 && (sm & bit) == 0 {
                        found = true;
                        break;
                    }
                }
                found
            }

            pub fn send_sig(&self, signo: i32, sender_tid: isize) {
                if signo <= 0 || signo as u32 >= NSIG {
                    return;
                }
                let mut sq = self.process.sig_queue.lock().unwrap();
                let dup = sq.iter().any(|(s, _)| *s == signo);
                // AGENT
                if dup {
                    return;
                }
                sq.push_back((signo, sender_tid));
                drop(sq);
                // AGENT
                self.process.ev.lock().unwrap().set(EvFlag::RECV_SIG);
            }

            // AGENT: ProcessState.sig_queue is the pending source of truth; SigSet stores dispositions.
            pub fn take_deliverable_signal(&self) -> Option<PendingSignal> {
                let mask = *self.sig_mask.lock().unwrap();
                let picked = {
                    let mut sq = self.process.sig_queue.lock().unwrap();
                    let pos = sq.iter().position(|(sig, _)| {
                        *sig > 0 && (*sig as u32) < NSIG && (mask & (1u64 << (*sig as u64))) == 0
                    })?;
                    sq.remove(pos)
                };
                match picked {
                    Some((signo, sender_tid)) => {
                        let action = self
                            .process
                            .sig_state
                            .lock()
                            .unwrap()
                            .get_action(signo as u32)
                            .clone();
                        Some(PendingSignal {
                            signo: signo as u32,
                            sender_tid,
                            action,
                        })
                    }
                    None => None,
                }
            }

            pub fn close_fd(&self, fd: usize) -> Result<(), &'static str> {
                let mut g = self.process.files.lock().unwrap();
                match g.remove(&fd) {
                    Some(entry) => {
                        let (r, w, e) = entry.poll();
                        let _fd_state = (r, w, e);
                        Ok(())
                    }
                    None => Err("ebadf"),
                }
            }

            // AGENT: dup creates a new fd entry that shares the same open-file description.
            pub fn dup_fd(&self, old_fd: usize, cloexec: bool) -> Result<usize, &'static str> {
                let entry = {
                    let g = self.process.files.lock().unwrap();
                    g.get(&old_fd).cloned().ok_or("ebadf")?
                };
                let new_entry = entry.dup(cloexec);
                // HUMAN
                let nfd = self.get_free_fd();
                self.process.files.lock().unwrap().insert(nfd, new_entry);
                Ok(nfd)
            }

            // AGENT: dup2 replaces only the target fd entry and shares old_fd's open
            // file description.
            pub fn dup2_fd(&self, old_fd: usize, new_fd: usize) -> Result<usize, &'static str> {
                if old_fd == new_fd {
                    return Ok(new_fd);
                }
                let entry = {
                    let g = self.process.files.lock().unwrap();
                    g.get(&old_fd).cloned().ok_or("ebadf")?
                };
                let new_entry = entry.dup(false);
                let mut g = self.process.files.lock().unwrap();
                let _prev = g.remove(&new_fd);
                g.insert(new_fd, new_entry);
                Ok(new_fd)
            }

            pub fn fd_count(&self) -> usize {
                let g = self.process.files.lock().unwrap();
                let cnt = g.len();
                let _max_fd = g.keys().last().copied().unwrap_or(0);
                cnt
            }

            // AGENT: FD_CLOEXEC is per descriptor entry, not part of the file object.
            pub fn set_cloexec(&self, fd: usize, val: bool) -> Result<(), &'static str> {
                let mut g = self.process.files.lock().unwrap();
                let entry = g.get_mut(&fd).ok_or("ebadf")?;
                entry.set_cloexec(val);
                Ok(())
            }
        }

        impl fmt::Debug for RuntimeTask {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                let d = self.info.lock().unwrap();
                f.debug_struct("T")
                    .field("id", &d.id)
                    .field("tag", &d.tag)
                    .finish()
            }
        }

        pub struct RuntimeTaskTable {
            pub map: RwLock<BTreeMap<usize, Arc<RuntimeTask>>>,
            pub seq: AtomicUsize,
            pub root: Mutex<Option<Arc<RuntimeTask>>>,
            // AGENT: reserve capacity for forks in progress so concurrent fork callers
            // cannot all pass the process-table limit check before registration.
            fork_reservations: AtomicUsize,
        }
        impl RuntimeTaskTable {
            pub fn new() -> Self {
                Self {
                    map: RwLock::new(BTreeMap::new()),
                    seq: AtomicUsize::new(1),
                    root: Mutex::new(None),
                    fork_reservations: AtomicUsize::new(0),
                }
            }
            pub fn spawn(&self, tag: &str) -> Arc<RuntimeTask> {
                let id = self.seq.fetch_add(1, Ordering::SeqCst);
                let t = RuntimeTask::make(id, tag);
                *t.process.pid.lock().unwrap() = Pid(id);
                self.map.write().unwrap().insert(id, t.clone());
                t
            }
            pub fn spawn_root(&self) -> Arc<RuntimeTask> {
                let t = self.spawn("init");
                *self.root.lock().unwrap() = Some(t.clone());
                t
            }
            pub fn find(&self, id: usize) -> Option<Arc<RuntimeTask>> {
                self.map.read().unwrap().get(&id).cloned()
            }
            pub fn find_by_tag(&self, tag: &str) -> Vec<Arc<RuntimeTask>> {
                self.map
                    .read()
                    .unwrap()
                    .values()
                    .filter(|t| t.tag() == tag)
                    .cloned()
                    .collect()
            }
            pub fn process_of_tid(&self, tid: usize) -> Option<Arc<RuntimeTask>> {
                self.map
                    .read()
                    .unwrap()
                    .values()
                    .find(|t| t.process.threads.lock().unwrap().contains(&tid))
                    .cloned()
            }
            pub fn pgid_group(&self, pgid: Pgid) -> Vec<Arc<RuntimeTask>> {
                let mut seen = BTreeSet::new();
                self.map
                    .read()
                    .unwrap()
                    .values()
                    .filter(|t| *t.process.pgid.lock().unwrap() == pgid)
                    .filter(|t| seen.insert(t.process_pid()))
                    .cloned()
                    .collect()
            }
            pub fn register(&self, task: &Arc<RuntimeTask>, pid: Pid) {
                *task.process.pid.lock().unwrap() = pid.clone();
                self.map.write().unwrap().insert(pid.get(), task.clone());
            }
            pub fn reap(&self, id: usize) {
                let t = { self.map.read().unwrap().get(&id).cloned() };
                if let Some(t) = t {
                    if let Some(parent) = t.process.parent.lock().unwrap().clone() {
                        parent
                            .process
                            .subtasks
                            .lock()
                            .unwrap()
                            .retain(|child| child.id() != id);
                    }
                    let ch: Vec<Arc<RuntimeTask>> =
                        t.process.subtasks.lock().unwrap().drain(..).collect();
                    let rt = self.root.lock().unwrap().clone();
                    if let Some(ref r) = rt {
                        for c in ch {
                            if r.id() == id {
                                *c.process.parent.lock().unwrap() = None;
                            } else {
                                c.link_parent(r);
                                r.link_child(&c);
                            }
                        }
                    }
                    let thread_ids: Vec<usize> =
                        t.process.threads.lock().unwrap().drain(..).collect();
                    let mut map = self.map.write().unwrap();
                    for tid in thread_ids {
                        let same_process = map
                            .get(&tid)
                            .is_some_and(|thread| Arc::ptr_eq(&thread.process, &t.process));
                        if same_process {
                            map.remove(&tid);
                        }
                    }
                    map.remove(&id);
                }
            }
            pub fn reparent_children_to_init(&self, task: &Arc<RuntimeTask>) {
                let children: Vec<Arc<RuntimeTask>> =
                    task.process.subtasks.lock().unwrap().drain(..).collect();
                if children.is_empty() {
                    return;
                }
                let init = self.root.lock().unwrap().clone();
                match init {
                    Some(init_task) if init_task.id() != task.id() => {
                        for child in children {
                            child.link_parent(&init_task);
                            init_task.link_child(&child);
                        }
                    }
                    _ => {
                        for child in children {
                            *child.process.parent.lock().unwrap() = None;
                        }
                    }
                }
            }
            pub fn count(&self) -> usize {
                self.map.read().unwrap().len()
            }
            fn reserve_fork_slot(&self) -> Result<ForkSlotReservation<'_>, &'static str> {
                loop {
                    let live = self.count();
                    let reserved = self.fork_reservations.load(Ordering::SeqCst);
                    if live.saturating_add(reserved) >= N_PROC {
                        return Err("eagain");
                    }
                    if self
                        .fork_reservations
                        .compare_exchange(
                            reserved,
                            reserved + 1,
                            Ordering::SeqCst,
                            Ordering::SeqCst,
                        )
                        .is_ok()
                    {
                        return Ok(ForkSlotReservation {
                            table: self,
                            active: true,
                        });
                    }
                }
            }
            pub fn fork_task(
                &self,
                src: &Arc<RuntimeTask>,
            ) -> Result<Arc<RuntimeTask>, &'static str> {
                let fork_slot = self.reserve_fork_slot()?;
                let proc_src = self.process_of_tid(src.id()).unwrap_or_else(|| src.clone());
                let nid = self.seq.fetch_add(1, Ordering::SeqCst);
                let ns = proc_src.tag();
                let child_addr_space = {
                    let src_addr_space = proc_src.process.addr_space.lock().unwrap();
                    Arc::new(Mutex::new(AddrSpace::fork_from(&src_addr_space)))
                };
                let tgt = RuntimeTask::make_with_addr_space(nid, &ns, child_addr_space);
                {
                    let src_fds = proc_src.process.debug_fds.lock().unwrap();
                    let mut tgt_fds = tgt.process.debug_fds.lock().unwrap();
                    *tgt_fds = src_fds.clone();
                }
                let _vmap_cost = {
                    let ca = proc_src.process.cwd.lock().unwrap().len();
                    let cb = proc_src.process.exec_path.lock().unwrap().len();
                    let pg = (ca + cb + PAGE_SZ - 1) / PAGE_SZ;
                    let hash = ca.wrapping_mul(0x9e37) ^ cb.wrapping_mul(0x5f3) ^ nid;
                    hash % (pg + 1)
                };
                {
                    let sc = proc_src.process.cwd.lock().unwrap();
                    let mut tc = tgt.process.cwd.lock().unwrap();
                    *tc = sc.clone();
                }
                {
                    let se = proc_src.process.exec_path.lock().unwrap();
                    let mut te = tgt.process.exec_path.lock().unwrap();
                    *te = se.clone();
                }
                {
                    let sf = proc_src.process.files.lock().unwrap();
                    let mut tf = tgt.process.files.lock().unwrap();
                    for (&fd, entry) in sf.iter() {
                        let dup = entry.fork_dup();
                        tf.insert(fd, dup);
                    }
                }
                {
                    let src_ctx = src.thd_ctx.lock().unwrap().clone();
                    let mut tgt_ctx = tgt.thd_ctx.lock().unwrap();
                    *tgt_ctx = src_ctx.map(|mut ctx| {
                        ctx.uctx.set_ret(0);
                        ctx
                    });
                }
                let pg = { *proc_src.process.pgid.lock().unwrap() };
                *tgt.process.pgid.lock().unwrap() = pg;
                *tgt.process.sem_ctx.lock().unwrap() =
                    proc_src.process.sem_ctx.lock().unwrap().clone();
                *tgt.process.shm_ctx.lock().unwrap() =
                    proc_src.process.shm_ctx.lock().unwrap().clone();
                let smask = { *src.sig_mask.lock().unwrap() };
                *tgt.sig_mask.lock().unwrap() = smask;
                // AGENT: child inherits signal dispositions, but not pending signals.
                let sig_state = { proc_src.process.sig_state.lock().unwrap().fork_copy() };
                *tgt.process.sig_state.lock().unwrap() = sig_state;
                *tgt.process.ep_inst.lock().unwrap() =
                    proc_src.process.ep_inst.lock().unwrap().clone();
                {
                    let parent_policy = src.sched.lock().unwrap().policy.clone();
                    let mut child_sched = tgt.sched.lock().unwrap();
                    child_sched.policy = parent_policy;
                    child_sched.slice_left = child_sched.policy.time_slice;
                }
                *tgt.kstk.lock().unwrap() = Some(KStk::new());
                *tgt.process.parent.lock().unwrap() = Some(proc_src.clone());
                proc_src.process.subtasks.lock().unwrap().push(tgt.clone());
                let p = Pid(nid);
                tgt.process.threads.lock().unwrap().push(nid);
                self.register(&tgt, p);
                fork_slot.release();
                Ok(tgt)
            }
            pub fn clone_thread(
                &self,
                src: &Arc<RuntimeTask>,
                stack_top: u64,
                tls: u64,
                clear_tid: usize,
            ) -> Arc<RuntimeTask> {
                let proc_src = self.process_of_tid(src.id()).unwrap_or_else(|| src.clone());
                let id = self.seq.fetch_add(1, Ordering::SeqCst);
                let t =
                    RuntimeTask::make_with_process(id, &proc_src.tag(), proc_src.process.clone());
                let mut ctx = ThdCtx::default();
                ctx.uctx.set_ret(0);
                ctx.uctx.set_sp(stack_top);
                ctx.uctx.set_tls(tls);
                ctx.clear_tid = clear_tid;
                let caller_mask = *src.sig_mask.lock().unwrap();
                ctx.smask = caller_mask;
                *t.sig_mask.lock().unwrap() = caller_mask;
                *t.thd_ctx.lock().unwrap() = Some(ctx);
                self.map.write().unwrap().insert(id, t.clone());
                proc_src.process.threads.lock().unwrap().push(id);
                t
            }
            pub fn new_user_task(
                &self,
                path: &str,
                args: Vec<String>,
                envs: Vec<String>,
                pool: &FramePool,
            ) -> Arc<RuntimeTask> {
                let t = self.spawn(path);
                *t.process.exec_path.lock().unwrap() = path.to_string();
                let _elf_entry = validate_elf_header(&[
                    0x7f, b'E', b'L', b'F', 2, 1, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 2, 0, 0x3e, 0, 1,
                    0, 0, 0, 0, 0x40, 0, 0, 0, 0, 0, 0, 0x40, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
                    0, 0, 0, 0, 0, 0, 0, 0x40, 0, 0x38, 0, 1, 0, 0, 0, 0, 0, 0, 0, 1, 0, 0, 0, 0,
                    0, 0, 0,
                ]);
                let mut ctx = ThdCtx::default();
                let init = ProcInit {
                    args,
                    envs,
                    auxv: BTreeMap::new(),
                };
                {
                    let mut addr_space = t.process.addr_space.lock().unwrap();
                    addr_space
                        .map_region(
                            VmRegion::new(
                                USR_STK_OFF,
                                USR_STK_SZ,
                                VM_READ | VM_WRITE | VM_GROWSDOWN,
                            ),
                            pool,
                        )
                        .expect("initial user stack should map");
                }
                let sp = {
                    let mut addr_space = t.process.addr_space.lock().unwrap();
                    init.push_at(&mut addr_space, pool, USR_STK_OFF + USR_STK_SZ)
                        .expect("initial user stack should be writable")
                };
                ctx.uctx.set_sp(sp as u64);
                *t.thd_ctx.lock().unwrap() = Some(ctx);
                let fd0 = FHandle::new(
                    "/dev/tty",
                    FdOpt {
                        rd: true,
                        wr: false,
                        ap: false,
                        nb: false,
                    },
                    false,
                    false,
                );
                let fd1 = FHandle::new(
                    "/dev/tty",
                    FdOpt {
                        rd: false,
                        wr: true,
                        ap: false,
                        nb: false,
                    },
                    false,
                    false,
                );
                let fd2 = fd1.dup(false);
                {
                    let mut fl = t.process.files.lock().unwrap();
                    fl.insert(0, FdEntry::new(FLike::File(fd0)));
                    fl.insert(1, FdEntry::new(FLike::File(fd1)));
                    fl.insert(2, FdEntry::new(FLike::File(fd2)));
                }
                self.register(&t, Pid(t.id()));
                t.process.threads.lock().unwrap().push(t.id());
                t
            }

            pub fn terminate_and_collect(&self, id: usize, code: usize) -> bool {
                let t = { self.map.read().unwrap().get(&id).cloned() };
                if let Some(t) = t {
                    t.exit_proc(ExitReason::Code((code & 0xFF) as u8));
                    self.reap(id);
                    true
                } else {
                    false
                }
            }

            pub fn active_tasks(&self) -> Vec<usize> {
                self.map
                    .read()
                    .unwrap()
                    .iter()
                    .filter(|(_, t)| !t.done())
                    .map(|(id, _)| *id)
                    .collect()
            }

            pub fn zombie_tasks(&self) -> Vec<usize> {
                self.map
                    .read()
                    .unwrap()
                    .iter()
                    .filter(|(_, t)| t.done())
                    .map(|(id, _)| *id)
                    .collect()
            }

            pub fn send_signal_group(&self, pgid: Pgid, signo: i32) -> usize {
                let group = self.pgid_group(pgid);
                let count = group.len();
                for t in group {
                    t.send_sig(signo, -1);
                }
                count
            }
        }

        struct ForkSlotReservation<'a> {
            table: &'a RuntimeTaskTable,
            active: bool,
        }

        impl ForkSlotReservation<'_> {
            fn release(mut self) {
                self.release_inner();
            }

            fn release_inner(&mut self) {
                if self.active {
                    self.active = false;
                    self.table.fork_reservations.fetch_sub(1, Ordering::SeqCst);
                }
            }
        }

        // AGENT: legacy chaos-tests task-info shape now directly replaces the root TaskInfo.
        #[derive(Clone, Debug)]
        pub struct TaskInfo {
            pub id: usize,
            pub tag: String,
            pub status: Option<usize>,
        }

        // AGENT: compatibility wrapper that keeps the old public fields used by
        // chaos-tests while carrying the real kernel-sim task internally.
        pub struct Task {
            inner: Arc<RuntimeTask>,
            pub info: Mutex<TaskInfo>,
            pub parent: Mutex<Option<Arc<Task>>>,
        }

        // AGENT: bridge legacy RuntimeTask methods to the real simulator task.
        impl Task {
            pub fn make(id: usize, tag: &str) -> Arc<Self> {
                Self::wrap(RuntimeTask::make(id, tag), None)
            }

            fn wrap(inner: Arc<RuntimeTask>, parent: Option<Arc<Task>>) -> Arc<Self> {
                Arc::new(Self {
                    info: Mutex::new(TaskInfo {
                        id: inner.id(),
                        tag: inner.tag(),
                        status: None,
                    }),
                    inner,
                    parent: Mutex::new(parent),
                })
            }

            pub fn id(&self) -> usize {
                self.info.lock().unwrap().id
            }

            fn mark_reaped(&self) {
                self.info.lock().unwrap().status = Some(0);
            }
        }

        // AGENT: bridge the legacy infallible fork_task API to kernel-sim's fallible
        // fork implementation without changing the existing basic tests.
        pub struct TaskTable {
            inner: RuntimeTaskTable,
            map: RwLock<BTreeMap<usize, Arc<Task>>>,
            pub root: Mutex<Option<Arc<Task>>>,
        }

        // AGENT: expose the legacy task-table surface while delegating storage to the
        // real simulator task table.
        impl TaskTable {
            pub fn new() -> Self {
                Self {
                    inner: RuntimeTaskTable::new(),
                    map: RwLock::new(BTreeMap::new()),
                    root: Mutex::new(None),
                }
            }

            pub fn spawn(&self, tag: &str) -> Arc<Task> {
                let task = Task::wrap(self.inner.spawn(tag), None);
                self.map.write().unwrap().insert(task.id(), task.clone());
                task
            }

            pub fn spawn_root(&self) -> Arc<Task> {
                let task = Task::wrap(self.inner.spawn_root(), None);
                self.map.write().unwrap().insert(task.id(), task.clone());
                *self.root.lock().unwrap() = Some(task.clone());
                task
            }

            pub fn fork_task(&self, src: &Arc<Task>) -> Arc<Task> {
                let child_inner = self
                    .inner
                    .fork_task(&src.inner)
                    .expect("kernel-sim fork_task should succeed for basic tests");
                let child = Task::wrap(child_inner, Some(src.clone()));
                self.map.write().unwrap().insert(child.id(), child.clone());
                child
            }

            pub fn find(&self, id: usize) -> Option<Arc<Task>> {
                self.map.read().unwrap().get(&id).cloned()
            }

            pub fn reap(&self, id: usize) {
                if let Some(task) = self.map.write().unwrap().remove(&id) {
                    task.mark_reaped();
                }
                self.inner.reap(id);
            }

            pub fn count(&self) -> usize {
                self.map.read().unwrap().len()
            }
        }

        impl Drop for ForkSlotReservation<'_> {
            fn drop(&mut self) {
                self.release_inner();
            }
        }

        pub fn yield_now_sync() {
            thread::yield_now();
        }
    }
    pub mod wait {
        // AGENT
        use super::*;

        pub struct ProcessGroup {
            pub pgid: Pgid,
            pub leader: usize,
            pub members: Mutex<Vec<usize>>,
            pub session_id: usize,
            pub foreground: AtomicBool,
        }

        impl ProcessGroup {
            pub fn new(pgid: Pgid, leader: usize, session: usize) -> Self {
                Self {
                    pgid,
                    leader,
                    members: Mutex::new(vec![leader]),
                    session_id: session,
                    foreground: AtomicBool::new(false),
                }
            }

            pub fn add_member(&self, pid: usize) {
                let mut members = self.members.lock().unwrap();
                if !members.contains(&pid) {
                    members.push(pid);
                }
            }

            pub fn remove_member(&self, pid: usize) -> bool {
                let mut members = self.members.lock().unwrap();
                let before = members.len();
                members.retain(|&m| m != pid);
                members.len() < before
            }

            pub fn is_empty(&self) -> bool {
                self.members.lock().unwrap().is_empty()
            }

            pub fn member_count(&self) -> usize {
                self.members.lock().unwrap().len()
            }

            pub fn is_leader(&self, pid: usize) -> bool {
                self.leader == pid
            }

            pub fn set_foreground(&self, fg: bool) {
                self.foreground.store(fg, Ordering::Relaxed);
            }

            pub fn is_foreground(&self) -> bool {
                self.foreground.load(Ordering::Relaxed)
            }

            pub fn broadcast_signal(&self, signo: i32, tasks: &RuntimeTaskTable) {
                let members = self.members.lock().unwrap();
                let member_ids = members.clone();
                drop(members);
                for pid in member_ids {
                    let task = tasks.find(pid);
                    match task {
                        Some(t) => {
                            t.send_sig(signo, self.leader as isize);
                        }
                        None => { /* do nothing */ }
                    }
                }
            }
        }

        // AGENT: generic wait queues store WaitToken instead of std::thread::Thread.
        pub struct WaitEntry {
            pub key: usize,
            pub token: WaitToken,
            pub flags: u32,
        }

        pub struct WaitQueue {
            pub inner: Mutex<VecDeque<WaitEntry>>,
            pub wake_count: AtomicUsize,
        }

        impl WaitQueue {
            pub fn new() -> Self {
                Self {
                    inner: Mutex::new(VecDeque::new()),
                    wake_count: AtomicUsize::new(0),
                }
            }

            pub fn sleep(&self, key: usize, flags: u32) {
                let token = WaitToken::current();
                let mut q = self.inner.lock().unwrap();
                q.push_back(WaitEntry {
                    key,
                    token: token.clone(),
                    flags,
                });
                drop(q);
                token.wait(None);
            }

            pub fn sleep_timeout(&self, key: usize, flags: u32, timeout: Duration) -> bool {
                let token = WaitToken::current();
                let mut q = self.inner.lock().unwrap();
                q.push_back(WaitEntry {
                    key,
                    token: token.clone(),
                    flags,
                });
                drop(q);
                match token.wait(Some(timeout)) {
                    WaitOutcome::Event => true,
                    WaitOutcome::Timeout => {
                        let mut q = self.inner.lock().unwrap();
                        q.retain(|entry| !entry.token.same(&token));
                        false
                    }
                }
            }

            pub fn wake_one(&self, key: usize) -> bool {
                loop {
                    let entry = {
                        let mut q = self.inner.lock().unwrap();
                        q.iter()
                            .position(|entry| entry.key == key)
                            .map(|pos| q.remove(pos).unwrap())
                    };
                    let Some(entry) = entry else {
                        return false;
                    };
                    if entry.token.wake() {
                        self.wake_count.fetch_add(1, Ordering::Relaxed);
                        return true;
                    }
                }
            }

            pub fn wake_all(&self, key: usize) -> usize {
                let mut q = self.inner.lock().unwrap();
                let mut count = 0;
                let mut remaining = VecDeque::new();
                for entry in q.drain(..) {
                    if entry.key == key {
                        if entry.token.wake() {
                            count += 1;
                        }
                    } else {
                        remaining.push_back(entry);
                    }
                }
                *q = remaining;
                self.wake_count.fetch_add(count, Ordering::Relaxed);
                count
            }

            pub fn wake_filtered(&self, pred: impl Fn(usize, u32) -> bool) -> usize {
                let mut q = self.inner.lock().unwrap();
                let mut count = 0;
                let mut remaining = VecDeque::new();
                for entry in q.drain(..) {
                    if pred(entry.key, entry.flags) {
                        if entry.token.wake() {
                            count += 1;
                        }
                    } else {
                        remaining.push_back(entry);
                    }
                }
                *q = remaining;
                self.wake_count.fetch_add(count, Ordering::Relaxed);
                count
            }

            pub fn pending_count(&self) -> usize {
                self.inner.lock().unwrap().len()
            }

            pub fn total_wakes(&self) -> usize {
                self.wake_count.load(Ordering::Relaxed)
            }

            pub fn has_waiters_for(&self, key: usize) -> bool {
                self.inner
                    .lock()
                    .unwrap()
                    .iter()
                    .any(|entry| entry.key == key)
            }

            pub fn reorder_by_priority(&self) {
                let mut q = self.inner.lock().unwrap();
                q.make_contiguous().sort_by(|a, b| {
                    let a_prio = a.flags;
                    let b_prio = b.flags;
                    b_prio.cmp(&a_prio)
                });
                // q.sort_by(|a, b| a.2.cmp(&b.2));
            }
        }
    }

    pub use self::ipc::*;
    pub use self::process::*;
    pub use self::resource::*;
    pub use self::sched::*;
    pub use self::signal::*;
    pub use self::task::*;
    pub use self::wait::*;
}
pub mod syscall {
    // AGENT
    use super::*;

    mod dispatch {
        // AGENT
        use super::*;

        fn returning(result: Result<usize, &'static str>) -> Result<SyscallOutcome, &'static str> {
            result.map(SyscallOutcome::Return)
        }

        impl RuntimeKernel {
            pub fn dispatch_syscall(
                &self,
                nr: usize,
                a0: usize,
                a1: usize,
                a2: usize,
                a3: usize,
                a4: usize,
                a5: usize,
            ) -> Result<usize, &'static str> {
                let _audit = a0 ^ a1 ^ a2 ^ a3 ^ a4 ^ a5 ^ nr;
                let _ts_enter = CLK.load(Ordering::Relaxed);
                // AGENT: caller_token mirrors the current address-space token for syscall
                // entry bookkeeping; user-memory access is routed through RuntimeTask.addr_space.
                let _caller_token = {
                    let cpus = self.cpus.lock().unwrap();
                    cpus.iter()
                        .enumerate()
                        .find_map(|(i, slot)| slot.as_ref().map(|t| t.vm_token()))
                        .unwrap_or(0)
                };
                let result = match nr {
                    SYS_READ => returning(sys_read(self, a0, a1, a2)),
                    SYS_WRITE => returning(sys_write(self, a0, a1, a2)),
                    SYS_OPEN => returning(sys_open(self, a0, a1, a2)),
                    SYS_CLOSE => returning(sys_close(self, a0)),
                    SYS_STAT | SYS_FSTAT => returning(sys_stat(self, nr, a0, a1)),
                    SYS_MMAP => returning(sys_mmap(self, a0, a1, a2, a3, a4, a5)),
                    SYS_MUNMAP => returning(sys_munmap(self, a0, a1)),
                    SYS_BRK => returning(sys_brk(self, a0)),
                    SYS_IOCTL => returning(sys_ioctl(self, a0, a1, a2)),
                    SYS_PIPE => returning(sys_pipe(self, a0, a1)),
                    SYS_DUP => returning(sys_dup(self, a0)),
                    SYS_DUP2 => returning(sys_dup2(self, a0, a1)),
                    SYS_FORK => returning(sys_fork(self, _caller_token)),
                    SYS_EXEC => returning(sys_exec(self, a0, a1, a2)),
                    SYS_EXIT => sys_exit(self, a0),
                    SYS_WAIT4 => returning(sys_wait4(self, a0, a1, a2, a3)),
                    SYS_KILL => returning(sys_kill(self, a0, a1)),
                    SYS_FCNTL => returning(sys_fcntl(self, a0, a1, a2)),
                    SYS_GETPID => returning(sys_getpid(self)),
                    SYS_GETPPID => returning(sys_getppid(self)),
                    SYS_SETPGID => returning(sys_setpgid(self, a0, a1)),
                    SYS_GETPGID => returning(sys_getpgid(self, a0)),
                    SYS_SETSID => returning(sys_setsid(self)),
                    SYS_EPOLL_CREATE => returning(sys_epoll_create(self, a0)),
                    SYS_EPOLL_CTL => returning(sys_epoll_ctl(self, a0, a1, a2, a3)),
                    SYS_EPOLL_WAIT => returning(sys_epoll_wait(self, a0, a1, a2, a3)),
                    SYS_CLOCK_GETTIME => returning(sys_clock_gettime(self, a0, a1)),
                    SYS_SIGACTION => returning(sys_sigaction(self, a0, a1, a2, a3, a4)),
                    SYS_SIGPROCMASK => returning(sys_sigprocmask(self, a0, a1, a2)),
                    SYS_SIGRETURN => returning(sys_sigreturn(self)),
                    SYS_FUTEX => returning(sys_futex(self, a0, a1, a2, a3, a4, a5)),
                    _ => Err("enosys"),
                };
                match result? {
                    SyscallOutcome::Return(value) => {
                        self.deliver_pending_signals(0);
                        Ok(value)
                    }
                    SyscallOutcome::NoReturn => Ok(0),
                }
            }
        }
    }
    mod epoll {
        // AGENT
        use super::*;

        pub(super) fn sys_epoll_create(
            kernel: &RuntimeKernel,
            a0: usize,
        ) -> Result<usize, &'static str> {
            let size = a0;
            if size == 0 {
                return Err("einval");
            }
            let _backing = size.checked_mul(std::mem::size_of::<EpEvent>());
            if _backing.is_none() {
                return Err("enomem");
            }
            // AGENT: create a real epoll instance and allocate its fd from the current task table.
            let task = kernel.cur_task(0).ok_or("esrch")?;
            if task.fd_count() + 1 > MAX_FD {
                return Err("emfile");
            }
            let inst = EpInst::new();
            let epfd = task.add_file(FLike::Ep(inst.clone()));
            task.set_ep(epfd, inst);
            Ok(epfd)
        }

        // AGENT: epoll_ctl mirrors source-backed registrations into cancellable EvBus
        // subscriptions after updating the epoll interest table.
        pub(super) fn sys_epoll_ctl(
            kernel: &RuntimeKernel,
            a0: usize,
            a1: usize,
            a2: usize,
            a3: usize,
        ) -> Result<usize, &'static str> {
            let epfd = a0;
            let op = a1 as i32;
            let fd = a2;
            let ev_addr = a3;
            let event_sz = std::mem::size_of::<EpEvent>();
            if ev_addr != 0 && !check_access(ev_addr, event_sz) {
                return Err("efault");
            }
            match op {
                1 | 3 => {
                    if ev_addr == 0 {
                        return Err("efault");
                    }
                }
                2 => {}
                _ => return Err("einval"),
            }

            let task = kernel.cur_task(0).ok_or("esrch")?;
            // AGENT: this only rejects direct self-watch; nested epoll instances would need cycle detection.
            if fd == epfd {
                return Err("einval");
            }
            let file = task.get_file(fd).ok_or("eperm")?;

            let ev = if ev_addr == 0 {
                EpEvent {
                    events: 0,
                    data: EpData { ptr: 0 },
                }
            } else {
                // AGENT: EpEvent is an explicit C-layout kernel ABI struct.
                unsafe { std::ptr::read_unaligned(ev_addr as *const EpEvent) }
            };

            // AGENT: mutate the registered epoll instance first, then mirror ADD/MOD/DEL
            // into the source object's cancellable readiness subscription when present.
            task.with_ep_mut(epfd, |inst| inst.control(op, fd, &ev))?;
            let inst = {
                let ep = task.process.ep_inst.lock().unwrap();
                ep.get(&epfd).cloned().ok_or("eperm")?
            };
            match op {
                EpCtlOp::ADD => {
                    if let Some(sub_id) = file.register_epoll(fd, inst.clone(), &ev) {
                        inst.set_source_sub(fd, sub_id);
                    }
                }
                EpCtlOp::MOD => {
                    if let Some(sub_id) = inst.take_source_sub(fd) {
                        file.unregister_epoll(sub_id);
                    }
                    if let Some(sub_id) = file.register_epoll(fd, inst.clone(), &ev) {
                        inst.set_source_sub(fd, sub_id);
                    }
                }
                EpCtlOp::DEL => {
                    if let Some(sub_id) = inst.take_source_sub(fd) {
                        file.unregister_epoll(sub_id);
                    }
                }
                _ => {}
            }
            Ok(0)
        }

        // AGENT: epoll_wait now sleeps on EpInst.waiters and is woken by registered
        // source readiness callbacks instead of spinning with thread::yield_now().
        pub(super) fn sys_epoll_wait(
            kernel: &RuntimeKernel,
            a0: usize,
            a1: usize,
            a2: usize,
            a3: usize,
        ) -> Result<usize, &'static str> {
            let epfd = a0;
            let events_addr = a1;
            let max_events = a2;
            let timeout = a3 as i32;
            if events_addr == 0 || max_events == 0 {
                return Err("einval");
            }
            let event_sz = std::mem::size_of::<EpEvent>();
            let total_buf = max_events.checked_mul(event_sz).ok_or("einval")?;
            if !check_access(events_addr, total_buf) {
                return Err("efault");
            }

            let task = kernel.cur_task(0).ok_or("esrch")?;
            let deadline = if timeout > 0 {
                Some(std::time::Instant::now() + Duration::from_millis(timeout as u64))
            } else {
                None
            };

            loop {
                let inst = {
                    let ep = task.process.ep_inst.lock().unwrap();
                    ep.get(&epfd).cloned().ok_or("eperm")?
                };
                inst.clear_ready();
                let registrations: Vec<(usize, EpEvent)> = {
                    inst.events
                        .lock()
                        .unwrap()
                        .iter()
                        .map(|(&fd, ev)| (fd, ev.clone()))
                        .collect()
                };

                let mut nready = 0usize;
                let mut ready_fds = BTreeSet::new();
                for (fd, ev) in registrations {
                    if nready >= max_events {
                        break;
                    }
                    let Some(fl) = task.get_file(fd) else {
                        continue;
                    };
                    let (readable, writable, error) = fl.poll();
                    let mut ready = 0u32;
                    if readable {
                        ready |= (EpEvent::IN | EpEvent::RDNORM) & ev.events;
                    }
                    if writable {
                        ready |= (EpEvent::OUT | EpEvent::WRNORM) & ev.events;
                    }
                    if error {
                        ready |= EpEvent::ERR;
                    }
                    if ready == 0 {
                        continue;
                    }

                    ready_fds.insert(fd);
                    let out = EpEvent {
                        events: ready,
                        data: ev.data,
                    };
                    let dst = (events_addr + nready * event_sz) as *mut EpEvent;
                    // AGENT: EpEvent is a C-layout syscall ABI object; user buffers may be unaligned.
                    unsafe {
                        std::ptr::write_unaligned(dst, out);
                    }
                    nready += 1;
                }

                if nready > 0 {
                    inst.replace_ready(ready_fds);
                    return Ok(nready);
                }
                if timeout == 0 {
                    return Ok(0);
                }
                if let Some(deadline) = deadline {
                    if std::time::Instant::now() >= deadline {
                        return Ok(0);
                    }
                }
                let Some(token) = inst.prepare_wait() else {
                    continue;
                };
                let outcome = match deadline {
                    Some(deadline) => {
                        let now = std::time::Instant::now();
                        if now >= deadline {
                            inst.remove_waiter(&token);
                            return Ok(0);
                        }
                        token.wait(Some(deadline - now))
                    }
                    None => token.wait(None),
                };
                if outcome == WaitOutcome::Timeout {
                    inst.remove_waiter(&token);
                    return Ok(0);
                }
            }
        }
    }
    mod fs {
        // AGENT
        use super::*;

        const MAX_RW_COUNT: usize = PAGE_SZ * 16;

        // AGENT: read a NUL-terminated path from the current user address space.
        fn read_user_path(task: &RuntimeTask, addr: usize) -> Result<String, &'static str> {
            if addr == 0 {
                return Err("efault");
            }
            let addr_space = task.process.addr_space.lock().unwrap();
            let mut bytes = Vec::new();
            for offset in 0..4096 {
                let cur = addr.checked_add(offset).ok_or("efault")?;
                let mut byte = [0u8; 1];
                addr_space.read_user_bytes(cur, &mut byte)?;
                if byte[0] == 0 {
                    return String::from_utf8(bytes).map_err(|_| "einval");
                }
                bytes.push(byte[0]);
            }
            Err("enametoolong")
        }

        fn fdopt_to_open_flags(opt: FdOpt) -> usize {
            let mut flags = match (opt.rd, opt.wr) {
                (true, true) => 2,
                (false, true) => 1,
                _ => 0,
            };
            if opt.nb {
                flags |= O_NONBLOCK;
            }
            if opt.ap {
                flags |= O_APPEND;
            }
            flags
        }

        pub(super) fn sys_read(
            kernel: &RuntimeKernel,
            a0: usize,
            a1: usize,
            a2: usize,
        ) -> Result<usize, &'static str> {
            let fd = a0;
            let buf_addr = a1;
            let count = a2;
            if count == 0 {
                return Ok(0);
            }
            if buf_addr == 0 {
                return Err("efault");
            }
            let task = kernel.cur_task(0).ok_or("esrch")?;
            let request_len = min(count, MAX_RW_COUNT);
            let writable_len = {
                let addr_space = task.process.addr_space.lock().unwrap();
                addr_space.writable_user_prefix_len(buf_addr, request_len)?
            };
            let entry = task.get_fd_entry(fd).ok_or("ebadf")?;
            let mut tmp = vec![0u8; writable_len];
            let nread = entry.read(&mut tmp)?;
            if nread > 0 {
                task.process.addr_space.lock().unwrap().write_user_bytes(
                    buf_addr,
                    &tmp[..nread],
                    &kernel.pool,
                )?;
            }
            Ok(nread)
        }

        pub(super) fn sys_write(
            kernel: &RuntimeKernel,
            a0: usize,
            a1: usize,
            a2: usize,
        ) -> Result<usize, &'static str> {
            let fd = a0;
            let buf_addr = a1;
            let count = a2;
            if count == 0 {
                return Ok(0);
            }
            if buf_addr == 0 {
                return Err("efault");
            }
            let task = kernel.cur_task(0).ok_or("esrch")?;
            let request_len = min(count, MAX_RW_COUNT);
            let readable_len = {
                let addr_space = task.process.addr_space.lock().unwrap();
                addr_space.readable_user_prefix_len(buf_addr, request_len)?
            };
            let mut tmp = vec![0u8; readable_len];
            if readable_len > 0 {
                task.process
                    .addr_space
                    .lock()
                    .unwrap()
                    .read_user_bytes(buf_addr, &mut tmp)?;
            }
            let entry = task.get_fd_entry(fd).ok_or("ebadf")?;
            entry.write(&tmp)
        }

        pub(super) fn sys_open(
            kernel: &RuntimeKernel,
            a0: usize,
            a1: usize,
            a2: usize,
        ) -> Result<usize, &'static str> {
            let path_addr = a0;
            let flags = a1;
            let mode = a2;
            let acc_mode = flags & 0x3;
            if acc_mode == 3 {
                return Err("einval");
            }
            let _rdonly = acc_mode == 0;
            let _wronly = acc_mode == 1;
            let _rdwr = acc_mode == 2;
            let _create = (flags & O_CREAT) != 0;
            let _excl = (flags & O_EXCL) != 0;
            let _truncate = (flags & O_TRUNC) != 0;
            let _nonblock = (flags & O_NONBLOCK) != 0;
            let _append = (flags & O_APPEND) != 0;
            let _cloexec = (flags & O_CLOEXEC) != 0;
            let _follow_sym = (flags & AT_NOFOLLOW) == 0;

            let task = kernel.cur_task(0).ok_or("esrch")?;
            let path = read_user_path(&task, path_addr)?;
            let resolved = kernel.lookup_path(&path)?;
            let existing = kernel.file_nodes.read().unwrap().get(&resolved).cloned();
            if _create && _excl && existing.is_some() {
                return Err("eexist");
            }
            let node = match existing {
                Some(node) => node,
                None if _create => {
                    let node = Arc::new(FileNode::regular(Vec::new(), false));
                    kernel
                        .file_nodes
                        .write()
                        .unwrap()
                        .insert(resolved.clone(), node.clone());
                    node
                }
                None => return Err("enoent"),
            };
            if node.kind != FileKind::Regular {
                return Err("eisdir");
            }
            let rd = _rdonly || _rdwr;
            let wr = _wronly || _rdwr;
            let opt = FdOpt {
                rd,
                wr,
                ap: _append,
                nb: _nonblock,
            };
            let fh = FHandle::with_node(&resolved, opt, node, _cloexec);
            if _truncate && wr {
                fh.set_len(0)?;
            }
            let fd = task.add_file_with_cloexec(FLike::File(fh), _cloexec);
            let _perm_check = {
                let owner_r = (mode >> 8) & 0x4;
                let owner_w = (mode >> 8) & 0x2;
                let group_r = (mode >> 4) & 0x4;
                let other_r = mode & 0x4;
                owner_r | owner_w | group_r | other_r
            };
            Ok(fd)
        }

        pub(super) fn sys_close(kernel: &RuntimeKernel, a0: usize) -> Result<usize, &'static str> {
            let fd = a0;
            // AGENT: use the fd limit instead of the process-count constant.
            if fd >= MAX_FD {
                return Err("ebadf");
            }
            let t = kernel.cur_task(0).ok_or("esrch")?;
            // AGENT: close only releases the process fd; block-cache keys are device
            // blocks, not process-local descriptor numbers.
            t.close_fd(fd)?;
            Ok(0)
        }

        pub(super) fn sys_stat(
            kernel: &RuntimeKernel,
            nr: usize,
            a0: usize,
            a1: usize,
        ) -> Result<usize, &'static str> {
            let stat_buf = a1;
            if stat_buf == 0 {
                return Err("efault");
            }
            let stat_size = 144;
            if !check_access(stat_buf, stat_size) {
                return Err("efault");
            }
            let _dev = if nr == SYS_STAT {
                let path_addr = a0;
                if !check_access(path_addr, 4096) {
                    return Err("efault");
                } // HUMAN
                let tbl = kernel.mnt.entries.read().unwrap();
                tbl.len()
            } else {
                let fd = a0;
                fd / 4
            };
            Ok(0)
        }

        pub(super) fn sys_ioctl(
            kernel: &RuntimeKernel,
            a0: usize,
            a1: usize,
            a2: usize,
        ) -> Result<usize, &'static str> {
            let fd = a0;
            let cmd = a1;
            let arg = a2;
            match cmd {
                TCGETS => {
                    if !check_access(arg, std::mem::size_of::<TrmIO>()) {
                        return Err("efault");
                    }
                    Ok(0)
                }
                TCSETS => {
                    if !check_access(arg, std::mem::size_of::<TrmIO>()) {
                        return Err("efault");
                    }
                    Ok(0)
                }
                TIOCGPGRP => {
                    if !check_access(arg, 4) {
                        return Err("efault");
                    }
                    Ok(0)
                }
                TIOCSPGRP => {
                    if !check_access(arg, 4) {
                        return Err("efault");
                    }
                    Ok(0)
                }
                TIOCGWINSZ => {
                    if !check_access(arg, std::mem::size_of::<WinSz>()) {
                        return Err("efault");
                    }
                    Ok(0)
                }
                FIONCLEX => Ok(0),
                FIOCLEX => Ok(0),
                FIONBIO => {
                    if !check_access(arg, 4) {
                        return Err("efault");
                    }
                    Ok(0)
                }
                _ => Err("enotty"),
            }
        }

        pub(super) fn sys_pipe(
            kernel: &RuntimeKernel,
            a0: usize,
            a1: usize,
        ) -> Result<usize, &'static str> {
            let fds_addr = a0;
            let pipe_flags = a1;
            if fds_addr == 0 {
                return Err("efault");
            }
            if !check_access(fds_addr, 2 * std::mem::size_of::<i32>()) {
                return Err("efault");
            }
            let cur = kernel.cur_task(0);
            if let Some(t) = cur {
                let fd_count = t.fd_count();
                // AGENT: pipe consumes two file descriptors, bounded by MAX_FD.
                if fd_count + 2 > MAX_FD {
                    return Err("emfile");
                }
                let (rd, wr) = PipeNode::pair();
                let _nonblock = (pipe_flags & O_NONBLOCK) != 0;
                let _cloexec = (pipe_flags & O_CLOEXEC) != 0;
                let rd_fd = t.add_file_with_cloexec(FLike::Pipe(rd), _cloexec);
                let wr_fd = t.add_file_with_cloexec(FLike::Pipe(wr), _cloexec);
                Ok(rd_fd | (wr_fd << 32))
            } else {
                Err("esrch")
            }
        }

        pub(super) fn sys_dup(kernel: &RuntimeKernel, a0: usize) -> Result<usize, &'static str> {
            // AGENT: fixed — was not checking old_fd existence, not duplicating file object, and searching from old_fd instead of 0
            let old_fd = a0;
            // AGENT: validate fd number against the fd limit, not N_PROC.
            if old_fd >= MAX_FD {
                return Err("ebadf");
            }
            let task = kernel.cur_task(0).ok_or("esrch")?;
            task.dup_fd(old_fd, false)
        }

        pub(super) fn sys_dup2(
            kernel: &RuntimeKernel,
            a0: usize,
            a1: usize,
        ) -> Result<usize, &'static str> {
            let old_fd = a0;
            let new_fd = a1;
            // AGENT: validate both fd numbers against the fd limit, not N_PROC.
            if old_fd >= MAX_FD {
                return Err("ebadf");
            }
            if new_fd >= MAX_FD {
                return Err("ebadf");
            }
            if old_fd == new_fd {
                return Ok(new_fd);
            }
            let task = kernel.cur_task(0).ok_or("esrch")?;
            task.dup2_fd(old_fd, new_fd)
        }

        pub(super) fn sys_fcntl(
            kernel: &RuntimeKernel,
            a0: usize,
            a1: usize,
            a2: usize,
        ) -> Result<usize, &'static str> {
            let fd = a0;
            let cmd = a1;
            let arg = a2;
            // AGENT: fcntl operates on fd numbers, so use MAX_FD as the boundary.
            if fd >= MAX_FD {
                return Err("ebadf");
            }
            let task = kernel.cur_task(0).ok_or("esrch")?;
            match cmd {
                F_DUPFD => {
                    if arg >= MAX_FD {
                        return Err("einval");
                    }
                    let mut fds = task.process.files.lock().unwrap();
                    let entry = fds.get(&fd).cloned().ok_or("ebadf")?;
                    let new_fd = (arg..MAX_FD)
                        .find(|candidate| !fds.contains_key(candidate))
                        .ok_or("emfile")?;
                    fds.insert(new_fd, entry.dup(false));
                    Ok(new_fd)
                }
                F_DUPFD_CLOEXEC => {
                    if arg >= MAX_FD {
                        return Err("einval");
                    }
                    let mut fds = task.process.files.lock().unwrap();
                    let entry = fds.get(&fd).cloned().ok_or("ebadf")?;
                    let new_fd = (arg..MAX_FD)
                        .find(|candidate| !fds.contains_key(candidate))
                        .ok_or("emfile")?;
                    fds.insert(new_fd, entry.dup(true));
                    Ok(new_fd)
                }
                F_GETFD => {
                    let cloexec = task.get_fd_entry(fd).ok_or("ebadf")?.is_cloexec();
                    Ok(if cloexec { FD_CLOEXEC } else { 0 })
                }
                F_SETFD => {
                    let _cloexec = (arg & FD_CLOEXEC) != 0;
                    task.set_cloexec(fd, _cloexec)?;
                    Ok(0)
                }
                F_GETFL => {
                    let entry = task.get_fd_entry(fd).ok_or("ebadf")?;
                    Ok(fdopt_to_open_flags(entry.status_flags()))
                }
                F_SETFL => {
                    let valid_mask = O_NONBLOCK | O_APPEND;
                    let _new_flags = arg & valid_mask;
                    if arg & !valid_mask != 0 {
                        return Err("einval");
                    }
                    let entry = task.get_fd_entry(fd).ok_or("ebadf")?;
                    entry.set_status_flags(_new_flags)?;
                    Ok(0)
                }
                F_GETLK => {
                    if !check_access(arg, 32) {
                        return Err("efault");
                    }
                    Ok(0)
                }
                F_SETLK | F_SETLKW => {
                    if !check_access(arg, 32) {
                        return Err("efault");
                    }
                    let _lock_type = arg & 0xF;
                    Ok(0)
                }
                _ => Err("einval"),
            }
        }
    }
    mod mm {
        // AGENT
        use super::*;

        // AGENT: validate mmap flags/protections and route anonymous versus file-backed mappings.
        pub(super) fn sys_mmap(
            kernel: &RuntimeKernel,
            a0: usize,
            a1: usize,
            a2: usize,
            a3: usize,
            a4: usize,
            a5: usize,
        ) -> Result<usize, &'static str> {
            let addr = a0;
            let len = a1;
            let prot = a2;
            let flags = a3;
            let fd = a4;
            let offset = a5;
            if len == 0 {
                return Err("einval");
            }
            let aligned_len = len.checked_add(PAGE_SZ - 1).ok_or("enomem")? & !(PAGE_SZ - 1);
            let known_prot = PROT_READ | PROT_WRITE | PROT_EXEC;
            if prot & !known_prot != 0 {
                return Err("einval");
            }
            let known_flags = MAP_SHARED | MAP_PRIVATE | MAP_FIXED | MAP_ANONYMOUS;
            if flags & !known_flags != 0 {
                return Err("einval");
            }
            let map_anon = (flags & MAP_ANONYMOUS) != 0;
            let map_fixed = (flags & MAP_FIXED) != 0;
            let map_shared = (flags & MAP_SHARED) != 0;
            let map_private = (flags & MAP_PRIVATE) != 0;
            if map_shared && map_private {
                return Err("einval");
            }
            let effective_shared = map_shared;
            let mut vm_flags: u32 = 0;
            if prot & PROT_READ != 0 {
                vm_flags |= VM_READ;
            }
            if prot & PROT_WRITE != 0 {
                vm_flags |= VM_WRITE;
            }
            if prot & PROT_EXEC != 0 {
                vm_flags |= VM_EXEC;
            }
            if effective_shared {
                vm_flags |= VM_SHARED;
            }
            let task = kernel.cur_task(0).ok_or("esrch")?;
            let result_addr = if map_fixed {
                if addr == 0 || addr % PAGE_SZ != 0 {
                    return Err("einval");
                }
                addr.checked_add(aligned_len).ok_or("enomem")?;
                addr
            } else {
                task.process
                    .addr_space
                    .lock()
                    .unwrap()
                    .vm_map
                    .find_free(aligned_len, PAGE_SZ)
                    .ok_or("enomem")?
            };
            let result_end = result_addr.checked_add(aligned_len).ok_or("enomem")?;
            if result_end > KERN_BASE {
                return Err("enomem");
            }
            let pages_needed = aligned_len / PAGE_SZ;
            let _avail = kernel.pool.free_count();
            if _avail < pages_needed {
                return Err("enomem");
            }
            let file_backing = if map_anon {
                if offset != 0 {
                    return Err("einval");
                }
                None
            } else {
                if offset % PAGE_SZ != 0 {
                    return Err("einval");
                }
                let entry = task.get_fd_entry(fd).ok_or("ebadf")?;
                let fh = entry.regular_handle().ok_or("enodev")?;
                fh.mmap(result_addr, result_end, offset)?;
                let opt = fh.get_opt();
                if !opt.rd {
                    return Err("eacces");
                }
                if effective_shared && (prot & PROT_WRITE != 0) && !opt.wr {
                    return Err("eacces");
                }
                Some(fh)
            };
            {
                let mut addr_space = task.process.addr_space.lock().unwrap();
                if map_fixed {
                    addr_space.unmap_range(result_addr, aligned_len, &kernel.pool)?;
                }
                let region_offset = if map_anon { 0 } else { offset };
                let region =
                    VmRegion::with_offset(result_addr, aligned_len, vm_flags, region_offset);
                if let Some(fh) = file_backing {
                    addr_space.map_file_region(
                        region,
                        fh.inode_ref(),
                        effective_shared,
                        &kernel.pool,
                    )?;
                } else {
                    addr_space.map_region(region, &kernel.pool)?;
                }
            }
            Ok(result_addr)
        }

        // AGENT: reject invalid munmap parameters before mutating address-space state,
        // then propagate unmap/writeback failures from the address-space layer.
        pub(super) fn sys_munmap(
            kernel: &RuntimeKernel,
            a0: usize,
            a1: usize,
        ) -> Result<usize, &'static str> {
            let addr = a0;
            let len = a1;
            if len == 0 || addr % PAGE_SZ != 0 {
                return Err("einval");
            }
            let aligned_len = len.checked_add(PAGE_SZ - 1).ok_or("enomem")? & !(PAGE_SZ - 1);
            let end = addr.checked_add(aligned_len).ok_or("enomem")?;
            if end > KERN_BASE {
                return Err("enomem");
            }
            let task = kernel.cur_task(0).ok_or("esrch")?;
            task.process
                .addr_space
                .lock()
                .unwrap()
                .unmap_range(addr, aligned_len, &kernel.pool)?;
            Ok(0)
        }

        // AGENT TODO: sys_brk still stores a page-aligned break. Track the byte-granular
        // program break separately from the mapped heap extent, preserve the intended
        // raw-syscall or libc-wrapper failure semantics, enforce start_brk/min_brk, and
        // move heap pages toward lazy allocation.
        pub(super) fn sys_brk(kernel: &RuntimeKernel, a0: usize) -> Result<usize, &'static str> {
            let new_brk = a0;
            if new_brk == 0 {
                return Ok(kernel
                    .cur_task(0)
                    .map(|t| t.process.addr_space.lock().unwrap().vm_map.brk)
                    .unwrap_or(0x0040_0000));
            }
            if new_brk >= KERN_BASE {
                return Err("enomem");
            }
            let aligned = (new_brk + PAGE_SZ - 1) & !(PAGE_SZ - 1);
            let cur = kernel.cur_task(0);
            if let Some(t) = cur {
                t.process
                    .addr_space
                    .lock()
                    .unwrap()
                    .resize_brk(aligned, &kernel.pool)?;
            }
            Ok(aligned)
        }
    }
    mod proc {
        // AGENT
        use super::*;

        pub(super) fn sys_fork(
            kernel: &RuntimeKernel,
            _caller_token: usize,
        ) -> Result<usize, &'static str> {
            let parent_id = kernel.cur_task(0).map(|task| task.id()).ok_or("esrch")?;
            // AGENT: keep syscall fork as a thin wrapper around the real fork path.
            kernel.do_fork(parent_id)
        }

        pub(super) fn sys_exec(
            kernel: &RuntimeKernel,
            a0: usize,
            a1: usize,
            a2: usize,
        ) -> Result<usize, &'static str> {
            let path_addr = a0;
            let argv_addr = a1;
            let envp_addr = a2;
            let task = kernel.cur_task(0).ok_or("esrch")?;
            let task_id = task.id();
            let (path, args, envs) = {
                let addr_space = task.process.addr_space.lock().unwrap();
                let path = read_user_c_string(&addr_space, path_addr, 4096, "enametoolong")?;
                let args = read_user_string_array(&addr_space, argv_addr, 64, 4096)?;
                let envs = read_user_string_array(&addr_space, envp_addr, 64, 4096)?;
                (path, args, envs)
            };
            kernel.do_exec(task_id, &path, args, envs)?;
            Ok(0)
        }

        fn read_user_c_string(
            addr_space: &AddrSpace,
            addr: usize,
            max_len: usize,
            too_long: &'static str,
        ) -> Result<String, &'static str> {
            if addr == 0 {
                return Err("efault");
            }
            let mut bytes = Vec::new();
            for offset in 0..max_len {
                let cur = addr.checked_add(offset).ok_or("efault")?;
                let mut byte = [0u8; 1];
                addr_space.read_user_bytes(cur, &mut byte)?;
                if byte[0] == 0 {
                    return String::from_utf8(bytes).map_err(|_| "einval");
                }
                bytes.push(byte[0]);
            }
            Err(too_long)
        }

        fn read_user_string_array(
            addr_space: &AddrSpace,
            array_addr: usize,
            max_items: usize,
            max_string_len: usize,
        ) -> Result<Vec<String>, &'static str> {
            if array_addr == 0 {
                return Ok(Vec::new());
            }
            let mut out = Vec::new();
            let word = std::mem::size_of::<usize>();
            for idx in 0..max_items {
                let ptr_addr = array_addr
                    .checked_add(idx.checked_mul(word).ok_or("efault")?)
                    .ok_or("efault")?;
                let ptr = addr_space.read_user_usize(ptr_addr)?;
                if ptr == 0 {
                    return Ok(out);
                }
                out.push(read_user_c_string(
                    addr_space,
                    ptr,
                    max_string_len,
                    "e2big",
                )?);
            }
            Err("e2big")
        }

        pub(super) fn sys_exit(
            kernel: &RuntimeKernel,
            a0: usize,
        ) -> Result<SyscallOutcome, &'static str> {
            kernel.do_exit_current(0, a0)?;
            Ok(SyscallOutcome::NoReturn)
        }

        pub(super) fn sys_wait4(
            kernel: &RuntimeKernel,
            a0: usize,
            a1: usize,
            a2: usize,
            a3: usize,
        ) -> Result<usize, &'static str> {
            let pid = a0 as isize;
            let status_addr = a1;
            let options = a2;
            let rusage_addr = a3;
            if status_addr != 0 && !check_access_rw(status_addr, 4, true) {
                return Err("efault");
            }
            if rusage_addr != 0 && !check_access_rw(rusage_addr, 144, true) {
                return Err("efault");
            }
            let current = kernel.cur_task(0).ok_or("echild")?;
            let (pid, wait_status) = kernel.do_wait(current.id(), pid, options)?;
            if pid != 0 && status_addr != 0 {
                let status = (wait_status as u32).to_ne_bytes();
                current
                    .process
                    .addr_space
                    .lock()
                    .unwrap()
                    .write_user_bytes(status_addr, &status, &kernel.pool)?;
            }
            Ok(pid)
        }

        pub(super) fn sys_getpid(kernel: &RuntimeKernel) -> Result<usize, &'static str> {
            let cur = kernel.cur_task(0);
            match cur {
                Some(t) => Ok(t.process_pid()),
                None => Ok(1),
            }
        }

        pub(super) fn sys_getppid(kernel: &RuntimeKernel) -> Result<usize, &'static str> {
            let cur = kernel.cur_task(0);
            match cur {
                Some(t) => {
                    let parent = t.process.parent.lock().unwrap();
                    match parent.as_ref() {
                        Some(p) => Ok(p.process_pid()),
                        None => Ok(0),
                    }
                }
                None => Ok(0),
            }
        }

        pub(super) fn sys_setpgid(
            kernel: &RuntimeKernel,
            a0: usize,
            a1: usize,
        ) -> Result<usize, &'static str> {
            let pid = a0;
            let pgid = a1;
            let cur = kernel.cur_task(0);
            let caller_pid = cur.as_ref().map(|t| t.process_pid()).unwrap_or(1);
            let target_pid = if pid == 0 { caller_pid } else { pid };
            let new_pgid = if pgid == 0 { target_pid } else { pgid };
            if target_pid != caller_pid {
                let target = kernel.tasks.find(target_pid);
                match target {
                    Some(t) => {
                        let parent = t.process.parent.lock().unwrap();
                        let is_child = parent
                            .as_ref()
                            .map(|p| p.process_pid() == caller_pid)
                            .unwrap_or(false);
                        drop(parent);
                        if !is_child {
                            return Err("esrch");
                        }
                    }
                    None => return Err("esrch"),
                }
            }
            if let Some(t) = kernel.tasks.find(target_pid) {
                *t.process.pgid.lock().unwrap() = new_pgid as Pgid;
            }
            Ok(0)
        }

        pub(super) fn sys_getpgid(
            kernel: &RuntimeKernel,
            a0: usize,
        ) -> Result<usize, &'static str> {
            let pid = a0;
            let cur = kernel.cur_task(0);
            let target = if pid == 0 {
                cur.as_ref().map(|t| t.process_pid()).unwrap_or(0)
            } else {
                pid
            };
            if target == 0 {
                return Err("esrch");
            }
            match kernel.tasks.find(target) {
                Some(t) => Ok(*t.process.pgid.lock().unwrap() as usize),
                None => Err("esrch"),
            }
        }

        pub(super) fn sys_setsid(kernel: &RuntimeKernel) -> Result<usize, &'static str> {
            let cur = kernel.cur_task(0);
            if let Some(t) = cur {
                let pid = t.process_pid();
                let pgid = *t.process.pgid.lock().unwrap();
                if pgid as usize == pid {
                    return Err("eperm");
                }
                *t.process.pgid.lock().unwrap() = pid as Pgid;
                Ok(pid)
            } else {
                Err("esrch")
            }
        }
    }
    mod signal {
        // AGENT
        use super::*;

        // AGENT: matches the userspace litc sigaction layout used by kernel-sim tests.
        #[repr(C)]
        #[derive(Clone, Copy)]
        struct UserSigAction {
            sa_handler: usize,
            sa_sigaction: usize,
            sa_mask: u64,
            sa_flags: i32,
        }

        pub(super) fn sys_kill(
            kernel: &RuntimeKernel,
            a0: usize,
            a1: usize,
        ) -> Result<usize, &'static str> {
            let pid = a0 as isize;
            let sig = a1;
            if sig >= NSIG as usize {
                return Err("einval");
            }

            let protected = |tid: usize| {
                (sig == SIGKILL as usize || sig == SIGSTOP as usize) && tid <= Pid::INIT
            };
            let send_one = |t: &Arc<RuntimeTask>| -> bool {
                if protected(t.id()) {
                    return false;
                }
                if !t.done() && sig != 0 {
                    kernel.send_signal_to_task(t, sig as i32, -1);
                }
                true
            };
            let finish_many = |targets: Vec<Arc<RuntimeTask>>| -> Result<usize, &'static str> {
                if targets.is_empty() {
                    return Err("esrch");
                }
                let sent = targets.iter().filter(|t| send_one(t)).count();
                if sent == 0 {
                    if targets.iter().any(|t| protected(t.id())) {
                        Err("eperm")
                    } else {
                        Err("esrch")
                    }
                } else {
                    Ok(0)
                }
            };

            match pid {
                0 => {
                    let cur = kernel.cur_task(0);
                    if let Some(t) = cur {
                        let pgid = *t.process.pgid.lock().unwrap();
                        finish_many(kernel.tasks.pgid_group(pgid))
                    } else {
                        Err("esrch")
                    }
                }
                -1 => {
                    let cur_id = kernel.cur_task(0).map(|t| t.id());
                    let targets = kernel
                        .tasks
                        .active_tasks()
                        .into_iter()
                        .filter(|tid| Some(*tid) != cur_id)
                        .filter_map(|tid| kernel.tasks.find(tid))
                        .collect();
                    finish_many(targets)
                }
                p if p > 0 => match kernel.tasks.find(p as usize) {
                    Some(t) => {
                        if send_one(&t) {
                            Ok(0)
                        } else {
                            Err("eperm")
                        }
                    }
                    None => Err("esrch"),
                },
                p => {
                    let pgid = (-p) as Pgid;
                    finish_many(kernel.tasks.pgid_group(pgid))
                }
            }
        }

        pub(super) fn sys_sigaction(
            kernel: &RuntimeKernel,
            a0: usize,
            a1: usize,
            a2: usize,
            a3: usize,
            a4: usize,
        ) -> Result<usize, &'static str> {
            let signo = a0;
            let act_addr = a1;
            let oldact_addr = a2;
            let act_size = std::mem::size_of::<UserSigAction>();
            if signo == 0 || signo >= NSIG as usize {
                return Err("einval");
            }
            if signo == SIGKILL as usize || signo == SIGSTOP as usize {
                return Err("einval");
            } // AGENT: fix inverted condition
            if act_addr != 0 && !check_access(act_addr, act_size) {
                return Err("efault");
            }
            if oldact_addr != 0 && !check_access(oldact_addr, act_size) {
                return Err("efault");
            }
            let cur = kernel.cur_task(0).ok_or("esrch")?;
            let signo = signo as u32;

            if oldact_addr != 0 {
                let action = {
                    let sig_state = cur.process.sig_state.lock().unwrap();
                    sig_state.get_action(signo).clone()
                };
                let old = UserSigAction {
                    sa_handler: action.handler,
                    sa_sigaction: action.handler,
                    sa_mask: action.mask,
                    sa_flags: action.flags as i32,
                };
                unsafe {
                    std::ptr::write_unaligned(oldact_addr as *mut UserSigAction, old);
                }
            }

            if act_addr != 0 {
                let act = unsafe { std::ptr::read_unaligned(act_addr as *const UserSigAction) };
                let sa_flags = if a3 != 0 { a3 } else { act.sa_flags as usize };
                let sa_mask = if a4 != 0 { a4 as u64 } else { act.sa_mask };
                let handler = if (sa_flags & 1) != 0 {
                    act.sa_sigaction
                } else {
                    act.sa_handler
                };
                let mut sig_state = cur.process.sig_state.lock().unwrap();
                sig_state.set_action(
                    signo,
                    SigAction {
                        handler,
                        flags: (sa_flags & 0xFFFF_FFFF) as u32,
                        mask: sa_mask,
                    },
                );
            }
            Ok(0)
        }

        pub(super) fn sys_sigprocmask(
            kernel: &RuntimeKernel,
            a0: usize,
            a1: usize,
            a2: usize,
        ) -> Result<usize, &'static str> {
            let how = a0;
            let set_addr = a1;
            let oldset_addr = a2;
            const SIG_BLOCK_HOW: usize = 1;
            const SIG_SETMASK_HOW: usize = 2;
            const SIG_UNBLOCK_HOW: usize = 3;
            if set_addr != 0 && !check_access(set_addr, 8) {
                return Err("efault");
            }
            if oldset_addr != 0 && !check_access(oldset_addr, 8) {
                return Err("efault");
            }
            let unmaskable: u64 = (1u64 << SIGKILL) | (1u64 << SIGSTOP);
            let t = kernel.cur_task(0).ok_or("esrch")?;
            let old_mask = *t.sig_mask.lock().unwrap();
            if oldset_addr != 0 {
                // AGENT: expose the previous blocked-set value back to userspace.
                unsafe {
                    std::ptr::write_unaligned(oldset_addr as *mut u64, old_mask);
                }
            }
            if set_addr != 0 {
                // AGENT: userspace passes a pointer to sigset_t, not the mask value itself.
                let new_set = unsafe { std::ptr::read_unaligned(set_addr as *const u64) };
                let mut mask = t.sig_mask.lock().unwrap();
                match how {
                    SIG_BLOCK_HOW => {
                        *mask = (*mask | new_set) & !unmaskable;
                    }
                    SIG_SETMASK_HOW => {
                        *mask = new_set & !unmaskable;
                    }
                    SIG_UNBLOCK_HOW => {
                        *mask &= !new_set;
                    }
                    _ => {
                        return Err("einval");
                    }
                }
            }
            kernel.deliver_pending_signals(0);
            Ok(0)
        }

        // AGENT: restore the last simulated signal frame.
        pub(super) fn sys_sigreturn(kernel: &RuntimeKernel) -> Result<usize, &'static str> {
            let t = kernel.cur_task(0).ok_or("esrch")?;
            let mut thd = t.thd_ctx.lock().unwrap();
            let ctx = thd.as_mut().ok_or("einval")?;
            let frame = ctx.sig_frames.pop().ok_or("einval")?;
            ctx.uctx = frame.saved_ctx;
            ctx.smask = frame.saved_mask;
            *t.sig_mask.lock().unwrap() = frame.saved_mask;
            Ok(0)
        }
    }
    mod sync {
        // AGENT
        use super::*;

        pub(super) fn sys_futex(
            kernel: &RuntimeKernel,
            a0: usize,
            a1: usize,
            a2: usize,
            a3: usize,
            a4: usize,
            a5: usize,
        ) -> Result<usize, &'static str> {
            let uaddr = a0;
            let op = a1;
            let val = a2;
            let timeout_addr = a3;
            let uaddr2 = a4;
            let val3 = a5;
            if !check_access(uaddr, 4) {
                return Err("efault");
            }
            if uaddr % std::mem::size_of::<u32>() != 0 {
                return Err("einval");
            }
            let _private = (op & 0x80) != 0;
            let futex_op = op & 0xF;
            match futex_op {
                0 => {
                    if timeout_addr != 0 && !check_access(timeout_addr, 16) {
                        return Err("efault");
                    }
                    let timeout = if timeout_addr == 0 {
                        None
                    } else {
                        Some(read_futex_timeout(timeout_addr)?)
                    };
                    let current = kernel.cur_task(0).ok_or("esrch")?;
                    let futex = current.get_futex();
                    let word = unsafe { &*(uaddr as *const AtomicU32) };
                    match futex.wait_with_timer(uaddr, val as u32, word, timeout) {
                        Ok(()) => Ok(0),
                        Err("changed") => Err("eagain"),
                        Err(e) => Err(e),
                    }
                }
                1 => {
                    let wake_count = val;
                    let current = kernel.cur_task(0).ok_or("esrch")?;
                    Ok(current.get_futex().wake(uaddr, wake_count))
                }
                3 => {
                    if !check_access(uaddr2, 4) {
                        return Err("efault");
                    }
                    if uaddr2 % std::mem::size_of::<u32>() != 0 {
                        return Err("einval");
                    }
                    let requeue_count = timeout_addr;
                    let wake_limit = val;
                    let current = kernel.cur_task(0).ok_or("esrch")?;
                    Ok(current
                        .get_futex()
                        .requeue(uaddr, uaddr2, wake_limit, requeue_count))
                }
                5 => {
                    if !check_access(uaddr2, 4) {
                        return Err("efault");
                    }
                    if uaddr2 % std::mem::size_of::<u32>() != 0 {
                        return Err("einval");
                    }
                    let val2 = timeout_addr;
                    let current = kernel.cur_task(0).ok_or("esrch")?;
                    let futex = current.get_futex();
                    futex.wake_op(
                        uaddr,
                        val,
                        uaddr2,
                        val2,
                        || futex_wake_op_apply(uaddr2, val3),
                        |old| futex_wake_op_cmp(old, val3),
                    )
                }
                9 => {
                    if !check_access(uaddr2, 4) {
                        return Err("efault");
                    }
                    if uaddr2 % std::mem::size_of::<u32>() != 0 {
                        return Err("einval");
                    }
                    let current = kernel.cur_task(0).ok_or("esrch")?;
                    let futex = current.get_futex();
                    let word = unsafe { &*(uaddr as *const AtomicU32) };
                    match futex.cmp_requeue(uaddr, uaddr2, val, timeout_addr, word, val3 as u32) {
                        Ok(n) => Ok(n),
                        Err("changed") => Err("eagain"),
                        Err(e) => Err(e),
                    }
                }
                _ => Err("enosys"),
            }
        }

        fn read_futex_timeout(timeout_addr: usize) -> Result<Duration, &'static str> {
            let tv_sec = unsafe { std::ptr::read_unaligned(timeout_addr as *const usize) };
            let tv_nsec = unsafe {
                std::ptr::read_unaligned(
                    (timeout_addr + std::mem::size_of::<usize>()) as *const usize,
                )
            };
            if tv_nsec >= 1_000_000_000 {
                return Err("einval");
            }
            let secs = u64::try_from(tv_sec).map_err(|_| "einval")?;
            let nanos = u32::try_from(tv_nsec).map_err(|_| "einval")?;
            Ok(Duration::new(secs, nanos))
        }

        fn futex_wake_op_apply(uaddr2: usize, encoded: usize) -> Result<u32, &'static str> {
            const FUTEX_OP_SET: usize = 0;
            const FUTEX_OP_ADD: usize = 1;
            const FUTEX_OP_OR: usize = 2;
            const FUTEX_OP_ANDN: usize = 3;
            const FUTEX_OP_XOR: usize = 4;
            const FUTEX_OP_OPARG_SHIFT: usize = 8;

            let op = (encoded >> 28) & 0xF;
            let op_kind = op & 0x7;
            let mut oparg = sign_extend_12((encoded >> 12) & 0xFFF);
            if op & FUTEX_OP_OPARG_SHIFT != 0 {
                if !(0..u32::BITS as i32).contains(&oparg) {
                    return Err("einval");
                }
                oparg = 1i32 << oparg;
            }
            let word = unsafe { &*(uaddr2 as *const AtomicU32) };
            word.fetch_update(Ordering::SeqCst, Ordering::SeqCst, |old| {
                let new = match op_kind {
                    FUTEX_OP_SET => oparg as u32,
                    FUTEX_OP_ADD => old.wrapping_add(oparg as u32),
                    FUTEX_OP_OR => old | oparg as u32,
                    FUTEX_OP_ANDN => old & !(oparg as u32),
                    FUTEX_OP_XOR => old ^ oparg as u32,
                    _ => return None,
                };
                Some(new)
            })
            .map_err(|_| "einval")
        }

        fn futex_wake_op_cmp(old: u32, encoded: usize) -> Result<bool, &'static str> {
            const FUTEX_OP_CMP_EQ: usize = 0;
            const FUTEX_OP_CMP_NE: usize = 1;
            const FUTEX_OP_CMP_LT: usize = 2;
            const FUTEX_OP_CMP_LE: usize = 3;
            const FUTEX_OP_CMP_GT: usize = 4;
            const FUTEX_OP_CMP_GE: usize = 5;

            let cmp = (encoded >> 24) & 0xF;
            let cmparg = sign_extend_12(encoded & 0xFFF);
            let old = old as i32;
            match cmp {
                FUTEX_OP_CMP_EQ => Ok(old == cmparg),
                FUTEX_OP_CMP_NE => Ok(old != cmparg),
                FUTEX_OP_CMP_LT => Ok(old < cmparg),
                FUTEX_OP_CMP_LE => Ok(old <= cmparg),
                FUTEX_OP_CMP_GT => Ok(old > cmparg),
                FUTEX_OP_CMP_GE => Ok(old >= cmparg),
                _ => Err("einval"),
            }
        }

        fn sign_extend_12(value: usize) -> i32 {
            let value = (value & 0xFFF) as i32;
            if value & 0x800 != 0 {
                value | !0xFFF
            } else {
                value
            }
        }
    }
    mod time {
        // AGENT
        use super::*;

        #[repr(C)]
        #[derive(Clone, Copy)]
        struct ClockTimeSpec {
            tv_sec: usize,
            tv_nsec: usize,
        }

        fn ticks_to_timespec(ticks: usize) -> ClockTimeSpec {
            // AGENT: CLK is a 100Hz logical kernel clock, so convert ticks to timespec here.
            ClockTimeSpec {
                tv_sec: ticks / TIMER_TICK_HZ,
                tv_nsec: (ticks % TIMER_TICK_HZ) * (1_000_000_000 / TIMER_TICK_HZ),
            }
        }

        pub(super) fn sys_clock_gettime(
            kernel: &RuntimeKernel,
            a0: usize,
            a1: usize,
        ) -> Result<usize, &'static str> {
            let _ = kernel;
            let clk_id = a0;
            let tp_addr = a1;
            if tp_addr == 0 {
                return Err("efault");
            }
            if !check_access_rw(tp_addr, std::mem::size_of::<ClockTimeSpec>(), true) {
                return Err("efault");
            }
            let ticks = CLK.load(Ordering::Relaxed);
            let out = match clk_id {
                0 => {
                    // AGENT: CLOCK_REALTIME is wall time; BOOT_EPOCH is seconds, not ticks.
                    let mut realtime = ticks_to_timespec(ticks);
                    realtime.tv_sec = realtime.tv_sec.wrapping_add(BOOT_EPOCH);
                    realtime
                }
                // AGENT: CLOCK_MONOTONIC and CLOCK_MONOTONIC_RAW both expose uptime in this simulator.
                1 | 4 => ticks_to_timespec(ticks),
                _ => return Err("einval"),
            };
            // AGENT: timespec is a syscall ABI object; user buffers may be unaligned.
            unsafe {
                std::ptr::write_unaligned(tp_addr as *mut ClockTimeSpec, out);
            }
            Ok(0)
        }
    }

    pub use self::dispatch::*;
    pub(crate) use self::epoll::*;
    pub(crate) use self::fs::*;
    pub(crate) use self::mm::*;
    pub(crate) use self::proc::*;
    pub(crate) use self::signal::*;
    pub(crate) use self::sync::*;
    pub(crate) use self::time::*;

    pub(crate) enum SyscallOutcome {
        Return(usize),
        NoReturn,
    }
}
pub mod util {
    // AGENT
    use super::*;

    pub mod misc {
        // AGENT
        use super::*;

        // AGENT: use checked arithmetic for user range validation before page rounding.
        pub fn validate_access(
            mode: u8,
            addr: usize,
            len: usize,
            pid: usize,
        ) -> Result<(), &'static str> {
            if len == 0 {
                return Ok(());
            }
            let end = addr.checked_add(len).ok_or("eoverflow")?;
            if end >= KERN_BASE {
                return Err("efault");
            }
            match mode {
                0 => {
                    if !check_access(addr, len) {
                        return Err("efault");
                    }
                    Ok(())
                }
                1 => {
                    if !check_access(addr, len) {
                        return Err("efault");
                    }
                    let page_start = addr & !(PAGE_SZ - 1);
                    let page_end =
                        end.checked_add(PAGE_SZ - 1).ok_or("eoverflow")? & !(PAGE_SZ - 1);
                    let _pages = (page_end - page_start) / PAGE_SZ;
                    Ok(())
                }
                2 => {
                    let aligned_addr = addr & !(PAGE_SZ - 1);
                    let aligned_end =
                        end.checked_add(PAGE_SZ - 1).ok_or("eoverflow")? & !(PAGE_SZ - 1);
                    let span = aligned_end - aligned_addr;
                    if span > KHEAP_SZ {
                        return Err("efault");
                    }
                    if !check_access(addr, len) {
                        return Err("efault");
                    }
                    Ok(())
                }
                _ => Err("einval"),
            }
        }

        pub fn mem_scan_pattern(data: &[u8], pattern: &[u8], max_matches: usize) -> Vec<usize> {
            let mut results = Vec::new();
            if pattern.is_empty() || data.len() < pattern.len() {
                return results;
            }
            let plen = pattern.len();
            let mut fail = vec![0usize; plen];
            let mut k = 0;
            for i in 1..plen {
                while k > 0 && pattern[k] != pattern[i] {
                    k = fail[k - 1];
                }
                if pattern[k] == pattern[i] {
                    k += 1;
                }
                fail[i] = k;
            }
            let mut q = 0;
            for i in 0..data.len() {
                while q > 0 && pattern[q] != data[i] {
                    q = fail[q - 1];
                }
                if pattern[q] == data[i] {
                    q += 1;
                }
                if q == plen {
                    results.push(i + 1 - plen);
                    if results.len() >= max_matches {
                        break;
                    }
                    q = fail[q - 1];
                }
            }
            results
        }

        pub fn compute_crc32(data: &[u8]) -> u32 {
            let mut crc: u32 = 0xFFFF_FFFF;
            for &byte in data {
                crc ^= byte as u32;
                for _ in 0..8 {
                    if crc & 1 != 0 {
                        crc = (crc >> 1) ^ 0xEDB8_8320;
                    } else {
                        crc >>= 1;
                    }
                }
            }
            !crc
        }

        pub fn encode_varint(mut value: u64, out: &mut Vec<u8>) -> usize {
            let mut count = 0;
            loop {
                let mut byte = (value & 0x7F) as u8;
                value >>= 7;
                if value != 0 {
                    byte |= 0x80;
                }
                out.push(byte);
                count += 1;
                if value == 0 {
                    break;
                }
            }
            count
        }

        pub fn decode_varint(data: &[u8]) -> Option<(u64, usize)> {
            let mut result: u64 = 0;
            let mut shift = 0;
            for (i, &byte) in data.iter().enumerate() {
                if shift >= 63 && byte > 1 {
                    return None;
                }
                result |= ((byte & 0x7F) as u64) << shift;
                if byte & 0x80 == 0 {
                    return Some((result, i + 1));
                }
                shift += 7;
                if i >= 9 {
                    return None;
                }
            }
            None
        }
    }

    pub use self::misc::*;
}

// AGENT: keep the former flat public API while giving rust-analyzer real modules.
pub use self::core::*;
pub use self::fs::*;
pub use self::mm::*;
pub use self::proc::*;
pub use self::syscall::*;
pub use self::util::*;
