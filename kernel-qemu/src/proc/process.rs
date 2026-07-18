// AGENT
use super::task::fd::FdTable;
use super::*;

// AGENT: represent one process independently from any schedulable thread;
// Task owns thread-local execution state and points at this shared entity.
pub struct Process {
    pid: usize,
    parent: Mutex<Option<Weak<Process>>>,
    children: Mutex<BTreeMap<usize, Arc<Process>>>,
    pub(super) fd_table: Mutex<FdTable>,
    pub exec_path: Mutex<String>,
    // AGENT: distinguish process futex words by waiter address in one bucket.
    pub futex: Arc<FutexBucket>,
    // AGENT: close the parent's setpgid window after the child execs.
    pub did_exec: AtomicBool,
    // AGENT: keep process-wide job-control stop separate from run-queue state.
    job_stopped: AtomicBool,
    threads: Mutex<BTreeSet<Tid>>,
    pub ev: Mutex<EvBus>,
    exit_reason: Mutex<Option<ExitReason>>,
    pub sig_queue: Mutex<VecDeque<(i32, isize)>>,
    pub sig_state: Mutex<SigSet>,
    pub addr_space: Mutex<AddrSpace>,
}

// AGENT: own process identity, family links, shared resources, and lifecycle
// transitions without depending on one representative Task.
impl Process {
    // AGENT: construct a registered-identity process before creating its leader
    // thread, so Process never passes through an invalid placeholder pid.
    pub(super) fn new(pid: usize, addr_space: AddrSpace) -> Self {
        Self {
            pid,
            parent: Mutex::new(None),
            children: Mutex::new(BTreeMap::new()),
            fd_table: Mutex::new(FdTable::default()),
            exec_path: Mutex::new(String::new()),
            futex: Arc::new(FutexBucket::new()),
            did_exec: AtomicBool::new(false),
            job_stopped: AtomicBool::new(false),
            threads: Mutex::new(BTreeSet::new()),
            ev: Mutex::new(EvBus::default()),
            exit_reason: Mutex::new(None),
            sig_queue: Mutex::new(VecDeque::new()),
            sig_state: Mutex::new(SigSet::new()),
            addr_space: Mutex::new(addr_space),
        }
    }

    // AGENT: expose immutable process identity without a registration-state lock.
    pub fn pid(&self) -> usize {
        self.pid
    }

    // AGENT: upgrade the non-owning parent link only while returning a snapshot.
    pub fn parent(&self) -> Option<Arc<Process>> {
        self.parent.lock().unwrap().as_ref().and_then(Weak::upgrade)
    }

    // AGENT: return a stable child-process snapshot for wait and reparent paths.
    pub fn children_snapshot(&self) -> Vec<Arc<Process>> {
        self.children.lock().unwrap().values().cloned().collect()
    }

    // AGENT: report whether no direct child process remains attached.
    pub fn has_no_children(&self) -> bool {
        self.children.lock().unwrap().is_empty()
    }

    // AGENT: install a weak parent link so process family ownership cannot cycle.
    pub(super) fn set_parent(&self, parent: Option<&Arc<Process>>) {
        *self.parent.lock().unwrap() = parent.map(Arc::downgrade);
    }

    // AGENT: retain a child process by pid until wait/reap detaches it.
    pub(super) fn insert_child(&self, child: Arc<Process>) {
        self.children.lock().unwrap().insert(child.pid(), child);
    }

    // AGENT: detach one child during reap without scanning unrelated children.
    pub(super) fn remove_child(&self, pid: usize) -> Option<Arc<Process>> {
        self.children.lock().unwrap().remove(&pid)
    }

    // AGENT: move all children out in pid order before orphan reparenting.
    pub(super) fn take_children(&self) -> Vec<Arc<Process>> {
        mem::take(&mut *self.children.lock().unwrap())
            .into_values()
            .collect()
    }

    // AGENT: add one schedulable thread id without duplicating membership.
    pub(super) fn add_thread(&self, tid: Tid) -> bool {
        self.threads.lock().unwrap().insert(tid)
    }

    // AGENT: snapshot thread membership for process-wide exit cleanup.
    pub fn thread_ids(&self) -> Vec<Tid> {
        self.threads.lock().unwrap().iter().copied().collect()
    }

    // AGENT: expose thread count to checkpoint validation without leaking the set.
    pub fn thread_count(&self) -> usize {
        self.threads.lock().unwrap().len()
    }

    // AGENT: report exact thread membership for focused lifecycle tests.
    pub fn has_thread(&self, tid: Tid) -> bool {
        self.threads.lock().unwrap().contains(&tid)
    }

    // AGENT: drain every thread id exactly once during process reap.
    pub(super) fn take_threads(&self) -> Vec<Tid> {
        mem::take(&mut *self.threads.lock().unwrap())
            .into_iter()
            .collect()
    }

    // AGENT: expose process-wide job-control stop independently of Task state.
    pub fn is_job_stopped(&self) -> bool {
        self.job_stopped.load(Ordering::Relaxed)
    }

    // AGENT: update process-wide job-control stop independently of Task state.
    pub fn set_job_stopped(&self, stopped: bool) {
        self.job_stopped.store(stopped, Ordering::Relaxed);
    }

    // AGENT: report process death from the authoritative shared exit reason.
    pub fn is_exited(&self) -> bool {
        self.exit_reason.lock().unwrap().is_some()
    }

    // AGENT: record process death once and notify process and parent waiters.
    pub(crate) fn exit_once(&self, reason: ExitReason) -> bool {
        let mut exit_reason = self.exit_reason.lock().unwrap();
        if exit_reason.is_some() {
            return false;
        }
        *exit_reason = Some(reason);
        drop(exit_reason);

        self.ev.lock().unwrap().set(EvFlag::PROC_QUIT);
        if let Some(parent) = self.parent() {
            parent.ev.lock().unwrap().set(EvFlag::CHILD_QUIT);
        }
        true
    }

    // AGENT: expose the encoded process exit status directly to wait paths.
    pub fn wait_status(&self) -> usize {
        match *self.exit_reason.lock().unwrap() {
            Some(reason) => reason.wait_status(),
            None => 0,
        }
    }

    // AGENT: update one process-wide disposition while holding the pending
    // queue first, then discard an already-pending ignored signal.
    pub fn set_signal_action(&self, signo: u32, action: SigAction) -> bool {
        let mut sig_queue = self.sig_queue.lock().unwrap();
        let should_discard = action.resolve(signo) == SignalDeliveryAction::Ignore;
        let mut sig_state = self.sig_state.lock().unwrap();
        if !sig_state.set_action(signo, action) {
            return false;
        }
        drop(sig_state);
        if should_discard {
            sig_queue.retain(|(pending, _)| *pending != signo as i32);
        }
        true
    }

    // AGENT: move droppable resources out of locks before process teardown and
    // reclaim address-space frames without forwarding a meaningless count.
    pub fn release_exit_resources(&self) {
        let old_resources = (
            take_mutex_default(&self.fd_table),
            take_mutex_default(&self.sig_queue),
            replace_mutex_value(&self.sig_state, SigSet::new()),
        );
        let _woken_futex_waiters = self.futex.wake_all();
        self.addr_space.lock().unwrap().release_all_pages();
        drop(old_resources);
    }
}

// AGENT: move a defaultable resource out of a mutex so Drop runs unlocked.
fn take_mutex_default<T: Default>(slot: &Mutex<T>) -> T {
    let mut guard = slot.lock().unwrap();
    mem::take(&mut *guard)
}

// AGENT: replace a non-Default mutex value and drop the old value unlocked.
fn replace_mutex_value<T>(slot: &Mutex<T>, value: T) -> T {
    let mut guard = slot.lock().unwrap();
    mem::replace(&mut *guard, value)
}

pub struct ProcInit {
    pub args: Vec<String>,
    pub envs: Vec<String>,
    pub auxv: BTreeMap<u8, usize>,
}
impl ProcInit {
    // AGENT: keep the stack-construction helper available to release builds
    // that call it from migrated exec/task code across codegen units.
    #[inline]
    pub fn push_at(
        &self,
        addr_space: &mut AddrSpace,
        pool: &FramePool,
        top: usize,
    ) -> Result<usize, &'static str> {
        let word = mem::size_of::<usize>();
        if top & 0xF != 0 {
            return Err("einval");
        }
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

        let ptr_bytes = self.checked_ptr_bytes(word)?;
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

    // AGENT: expose a checked size calculation so exec rejects impossible
    // argument layouts before mapping and writing the user stack.
    pub fn checked_total_size(&self) -> Result<usize, &'static str> {
        let word = mem::size_of::<usize>();
        let mut sz = 0usize;
        for a in &self.args {
            sz = sz
                .checked_add(a.len().checked_add(1).ok_or("e2big")?)
                .ok_or("e2big")?;
        }
        for e in &self.envs {
            sz = sz
                .checked_add(e.len().checked_add(1).ok_or("e2big")?)
                .ok_or("e2big")?;
        }
        sz = sz
            .checked_add(self.checked_ptr_bytes(word)?)
            .ok_or("e2big")?;
        sz.checked_add(15).map(|size| size & !15).ok_or("e2big")
    }

    // AGENT: keep the old infallible helper as a saturating compatibility view;
    // new exec paths should use checked_total_size() for error reporting.
    pub fn total_size(&self) -> usize {
        self.checked_total_size().unwrap_or(usize::MAX)
    }

    // AGENT: account for argc, argv/envp null sentinels, auxv key/value pairs,
    // and the final AT_NULL pair without relying on unchecked usize arithmetic.
    fn checked_ptr_bytes(&self, word: usize) -> Result<usize, &'static str> {
        let aux_words = self.auxv.len().checked_mul(2).ok_or("e2big")?;
        let ptr_words = 1usize
            .checked_add(self.args.len())
            .ok_or("e2big")?
            .checked_add(1)
            .ok_or("e2big")?
            .checked_add(self.envs.len())
            .ok_or("e2big")?
            .checked_add(1)
            .ok_or("e2big")?
            .checked_add(aux_words)
            .ok_or("e2big")?
            .checked_add(2)
            .ok_or("e2big")?;
        ptr_words.checked_mul(word).ok_or("e2big")
    }

    // AGENT: write one native-width stack slot through the unified user-copy path.
    fn write_usize(
        addr_space: &mut AddrSpace,
        pool: &FramePool,
        cur: &mut usize,
        value: usize,
    ) -> Result<(), &'static str> {
        addr_space.write_user_bytes(*cur, &value.to_ne_bytes(), pool)?;
        *cur += mem::size_of::<usize>();
        Ok(())
    }
}

pub struct CapSet {
    pub bits: u64,
    pub effective: u64,
    pub ambient: u64,
}

impl CapSet {
    // AGENT: keep capability-index validation in one place so callers do not
    // repeat manual shift bounds checks.
    fn cap_bit(cap: u32) -> Option<u64> {
        if cap < 64 {
            Some(1u64 << cap)
        } else {
            None
        }
    }

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
        if let Some(bit) = Self::cap_bit(cap) {
            (self.effective & bit) != 0
        } else {
            false
        }
    }

    pub fn grant(&mut self, cap: u32) {
        if let Some(bit) = Self::cap_bit(cap) {
            self.bits |= bit;
            self.effective |= bit;
        }
    }

    // AGENT: dropping a capability must also remove it from ambient so a later
    // inheritance path cannot keep a capability the process no longer owns.
    pub fn drop_cap(&mut self, cap: u32) {
        if let Some(bit) = Self::cap_bit(cap) {
            self.bits &= !bit;
            self.effective &= !bit;
            self.ambient &= !bit;
        }
    }

    // AGENT: keep inherited capabilities easy to reason about: the mask lists
    // what may cross the boundary, and effective/ambient cannot outgrow it.
    pub fn inherit(parent: &CapSet) -> CapSet {
        let inherited_bits = parent.bits & INHERITABLE_MASK;
        let inherited_effective = parent.effective & inherited_bits;
        let inherited_ambient = parent.ambient & inherited_bits;
        CapSet {
            bits: inherited_bits,
            effective: inherited_effective,
            ambient: inherited_ambient,
        }
    }

    pub fn has_any(&self, mask: u64) -> bool {
        (self.effective & mask) != 0
    }

    pub fn clear_ambient(&mut self) {
        self.ambient = 0;
    }

    // AGENT: only owned capabilities that are allowed to cross an inheritance
    // boundary may be raised into the ambient set.
    pub fn raise_ambient(&mut self, cap: u32) -> bool {
        let Some(bit) = Self::cap_bit(cap) else {
            return false;
        };
        let owns_capability = (self.bits & bit) != 0;
        let may_inherit = (INHERITABLE_MASK & bit) != 0;
        if owns_capability && may_inherit {
            self.ambient |= bit;
            true
        } else {
            false
        }
    }
}
