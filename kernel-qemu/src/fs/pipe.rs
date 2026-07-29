// AGENT
use super::*;

// AGENT: keep the atomic-write limit distinct from total pipe capacity even
// while the first QEMU implementation intentionally uses one page for both.
pub const PIPE_BUF: usize = 4 * 1024;
const PIPE_CAPACITY: usize = 4 * 1024;
const _: () = assert!(PIPE_CAPACITY >= PIPE_BUF);

// AGENT: EvBus stores the pipe-wide publication of endpoint readiness bits, so
// refreshes should only touch pipe-related bits and leave unrelated event bits alone.
const PIPE_READY_MASK: u32 = EvFlag::READABLE | EvFlag::WRITABLE | EvFlag::ERROR | EvFlag::CLOSED;

// AGENT: pipe endpoint direction is internal to this module; Copy keeps
// endpoint checks simple without exposing the enum outside pipe handling.
#[derive(Clone, Copy, PartialEq)]
enum PipeDir {
    Rd,
    Wr,
}

// AGENT: describe one lock-held read attempt without using EAGAIN as internal
// control flow; OpenFileDesc decides whether WouldBlock sleeps or reaches ABI.
enum PipeReadStep {
    Read(usize),
    Eof,
    WouldBlock,
}

// AGENT: preserve a broken-pipe notification even when a large write already
// made partial progress, so syscall glue can generate SIGPIPE and return bytes.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PipeWriteOutcome {
    Written(usize),
    Broken { written: usize },
}

// AGENT: preserve splice progress separately from broken-pipe notification so
// syscall glue can apply Linux's partial-return plus SIGPIPE rules.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SpliceOutcome {
    Moved(usize),
    BrokenPipe { moved: usize },
}

// AGENT: represent an atomically prepared pipe-to-pipe wait without retaining
// either pipe-state guard across the scheduler handoff.
enum PipeSpliceAction {
    Complete(SpliceOutcome),
    WaitInput(WaitToken),
    WaitOutput(WaitToken),
}

// AGENT: describe one lock-held write attempt; atomic_request is supplied by
// the complete write call rather than recomputed from a shrinking remainder.
enum PipeWriteStep {
    Wrote(usize),
    WouldBlock,
    Broken,
}

// AGENT: return only terminal peer-close wakeups from endpoint accounting;
// ordinary data movement wakes the opposite queue in read_at/write_at.
enum PipeCloseWake {
    None,
    Readers,
    Writers,
}

// AGENT: keep shared pipe state private while preserving explicit reader/writer
// counts for clone/drop peer-close semantics.
struct PipeBuf {
    buf: CircBuf,
    bus: EvBus,
    readers: i32,
    writers: i32,
}

impl PipeBuf {
    // AGENT: centralize pipe-buffer construction so PipeNode no longer reaches
    // into CircBuf/EvBus fields when creating endpoint pairs.
    fn new() -> Self {
        let mut pipe = Self {
            buf: CircBuf::new(PIPE_CAPACITY),
            bus: EvBus::default(),
            readers: 1,
            writers: 1,
        };
        pipe.publish_readiness();
        pipe
    }

    // AGENT: keep endpoint reference accounting with the shared buffer state it
    // protects instead of open-coding reader/writer mutations in PipeNode.
    fn add_endpoint_ref(&mut self, dir: PipeDir) {
        match dir {
            PipeDir::Rd => self.readers += 1,
            PipeDir::Wr => self.writers += 1,
        }
        self.publish_readiness();
    }

    // AGENT: publish peer-close readiness from the buffer owner whenever the
    // last reader or writer endpoint is dropped.
    fn release_endpoint_ref(&mut self, dir: PipeDir) -> PipeCloseWake {
        let wake = match dir {
            PipeDir::Rd => {
                debug_assert!(self.readers > 0);
                self.readers -= 1;
                if self.readers == 0 {
                    PipeCloseWake::Writers
                } else {
                    PipeCloseWake::None
                }
            }
            PipeDir::Wr => {
                debug_assert!(self.writers > 0);
                self.writers -= 1;
                if self.writers == 0 {
                    PipeCloseWake::Readers
                } else {
                    PipeCloseWake::None
                }
            }
        };
        self.publish_readiness();
        wake
    }

    // AGENT: compute endpoint-local readiness from the pipe buffer and peer
    // counters in one place for poll and epoll registration.
    fn readiness(&self, dir: PipeDir) -> u32 {
        match dir {
            PipeDir::Rd => {
                let eof = self.writers == 0;
                let mut ready = 0;
                if !self.buf.empty() || eof {
                    ready |= EvFlag::READABLE;
                }
                if eof {
                    ready |= EvFlag::CLOSED;
                }
                ready
            }
            PipeDir::Wr => {
                let broken = self.readers == 0;
                let mut ready = 0;
                if broken {
                    ready |= EvFlag::CLOSED | EvFlag::ERROR;
                } else if self.buf.remaining() > 0 {
                    ready |= EvFlag::WRITABLE;
                }
                ready
            }
        }
    }

    // AGENT: publish the union of readiness visible to still-live endpoints so
    // read/write mutations do not open-code partial EvBus bit updates.
    fn publish_readiness(&mut self) {
        let mut ready = 0;
        if self.readers > 0 {
            ready |= self.readiness(PipeDir::Rd);
        }
        if self.writers > 0 {
            ready |= self.readiness(PipeDir::Wr);
        }
        if self.readers == 0 || self.writers == 0 {
            ready |= EvFlag::CLOSED;
        }

        self.bus.change(PIPE_READY_MASK & !ready, ready);
    }

    // AGENT: attach epoll callbacks to the buffer's EvBus while using the same
    // readiness calculation that poll() observes.
    fn subscribe_epoll(
        &mut self,
        dir: PipeDir,
        wake_mask: u32,
        key: &EpKey,
        ep: &EpInst,
    ) -> (usize, bool) {
        let ready = self.readiness(dir);
        let target_epoll = ep.downgrade();
        let wake_key = key.downgrade();
        let sub_id = self.bus.sub(
            wake_mask,
            Box::new(move |_bus_ev| {
                let Some(target_epoll) = target_epoll.upgrade() else {
                    return true;
                };
                let Some(key) = wake_key.upgrade() else {
                    return true;
                };
                target_epoll.mark_ready(&key);
                false
            }),
        );
        (sub_id, (ready & wake_mask) != 0)
    }

    // AGENT: remove epoll source subscriptions from the buffer-owned EvBus.
    fn unsubscribe_epoll(&mut self, sub_id: usize) -> bool {
        self.bus.unsub(sub_id)
    }

    // AGENT: keep one nonblocking read attempt and readiness publication inside
    // PipeBuf; scheduler waiting remains outside this interrupt-safe mutex.
    fn read_into(&mut self, out: &mut [u8]) -> PipeReadStep {
        if out.is_empty() {
            return PipeReadStep::Read(0);
        }
        if self.buf.empty() {
            return if self.writers == 0 {
                PipeReadStep::Eof
            } else {
                PipeReadStep::WouldBlock
            };
        }

        let n = min(out.len(), self.buf.len());
        for dst in out.iter_mut().take(n) {
            *dst = self.buf.pop().unwrap();
        }
        self.publish_readiness();
        PipeReadStep::Read(n)
    }

    // AGENT: keep peer-close and atomic-capacity checks in the same critical
    // section as buffer mutation so PIPE_BUF writes are all-or-nothing.
    fn write_from(&mut self, input: &[u8], atomic_request: bool) -> PipeWriteStep {
        if input.is_empty() {
            return PipeWriteStep::Wrote(0);
        }
        if self.readers == 0 {
            return PipeWriteStep::Broken;
        }
        let remaining = self.buf.remaining();
        if remaining == 0 || (atomic_request && remaining < input.len()) {
            return PipeWriteStep::WouldBlock;
        }

        let written = self.buf.fill_from(input);
        debug_assert!(!atomic_request || written == input.len());
        self.publish_readiness();
        PipeWriteStep::Wrote(written)
    }

    // AGENT: keep buffered-byte queries with PipeBuf so ioctl callers do not
    // require direct access to CircBuf.
    fn readable_len(&self) -> usize {
        self.buf.len()
    }
}

// AGENT: couple buffer state with two scheduler wait queues while keeping queue
// wakeups outside the state lock and EvBus dedicated to poll/epoll readiness.
struct PipeShared {
    state: Mutex<PipeBuf>,
    read_waiters: WaitQueue,
    write_waiters: WaitQueue,
}

impl PipeShared {
    // AGENT: initialize both condition queues beside the state they protect so
    // every pipe pair receives an isolated scheduler-wait domain.
    fn new() -> Self {
        Self {
            state: Mutex::new(PipeBuf::new()),
            read_waiters: WaitQueue::new(),
            write_waiters: WaitQueue::new(),
        }
    }
}

// AGENT: each endpoint carries one counted direction reference into PipeShared;
// descriptor aliases continue to share it through OpenFileDesc.
pub struct PipeNode {
    shared: Arc<PipeShared>,
    dir: PipeDir,
}

// AGENT: make poll readiness explicit so closed peer state is not hidden inside
// a three-boolean tuple.
#[derive(Clone, Copy, Default)]
pub struct PollStatus {
    pub readable: bool,
    pub writable: bool,
    pub error: bool,
    pub closed: bool,
}

impl PollStatus {
    // AGENT: keep the EvFlag -> PollStatus mapping in one place so pipe polling
    // and epoll event translation do not duplicate bit checks.
    fn from_ready_bits(ready: u32) -> Self {
        Self {
            readable: (ready & EvFlag::READABLE) != 0,
            writable: (ready & EvFlag::WRITABLE) != 0,
            error: (ready & EvFlag::ERROR) != 0,
            closed: (ready & EvFlag::CLOSED) != 0,
        }
    }
}

impl Clone for PipeNode {
    // AGENT: cloning a pipe endpoint represents another fd/reference to that
    // endpoint, so the explicit reader/writer counters must follow the clone.
    fn clone(&self) -> Self {
        let cloned = PipeNode {
            shared: self.shared.clone(),
            dir: self.dir,
        };
        {
            let mut d = cloned.shared.state.lock().unwrap();
            d.add_endpoint_ref(cloned.dir);
        }
        cloned
    }
}

// AGENT: endpoint drop publishes pipe closure to the shared readiness bus.
impl Drop for PipeNode {
    fn drop(&mut self) {
        let wake = {
            let mut d = self.shared.state.lock().unwrap();
            d.release_endpoint_ref(self.dir)
        };
        match wake {
            PipeCloseWake::Readers => self.shared.read_waiters.broadcast(),
            PipeCloseWake::Writers => self.shared.write_waiters.broadcast(),
            PipeCloseWake::None => {}
        }
    }
}

impl PipeNode {
    // AGENT: keep endpoint-pair construction at PipeNode while delegating shared
    // buffer initialization to PipeBuf.
    pub fn pair() -> (PipeNode, PipeNode) {
        let shared = Arc::new(PipeShared::new());
        (
            PipeNode {
                shared: shared.clone(),
                dir: PipeDir::Rd,
            },
            PipeNode {
                shared,
                dir: PipeDir::Wr,
            },
        )
    }
    // AGENT: translate epoll interest into EvBus wake bits. ERR/HUP are always
    // reported by epoll, so pipe subscriptions always watch closed/error too.
    fn epoll_wake_mask(&self, interest: u32) -> u32 {
        let io_mask = match self.dir {
            PipeDir::Rd if interest & (EpEvent::IN | EpEvent::RDNORM) != 0 => EvFlag::READABLE,
            PipeDir::Wr if interest & (EpEvent::OUT | EpEvent::WRNORM) != 0 => EvFlag::WRITABLE,
            _ => 0,
        };

        io_mask | EvFlag::CLOSED | EvFlag::ERROR
    }
    // AGENT: connect pipe readiness changes to an epoll instance through the
    // pipe's EvBus, while returning a cancellable subscription id.
    pub fn register_epoll(&self, key: &EpKey, ep: &EpInst, ev: &EpEvent) -> Option<usize> {
        let wake_mask = self.epoll_wake_mask(ev.events);
        let (sub_id, ready_now) = {
            let mut pipe = self.shared.state.lock().unwrap();
            pipe.subscribe_epoll(self.dir, wake_mask, key, ep)
        };
        if ready_now {
            ep.mark_ready(key);
        }
        Some(sub_id)
    }
    // AGENT: remove an epoll readiness subscription previously installed on
    // this pipe's EvBus.
    pub fn unregister_epoll(&self, sub_id: usize) -> bool {
        self.shared.state.lock().unwrap().unsubscribe_epoll(sub_id)
    }
    // AGENT: check readiness and enqueue under the same state lock, then wait
    // interruptibly without retaining any pipe or waiter-queue guard.
    pub fn read_at(
        &self,
        task_id: usize,
        nonblock: bool,
        buf: &mut [u8],
    ) -> Result<usize, &'static str> {
        if self.dir != PipeDir::Rd {
            return Err("ebadf");
        }
        loop {
            let token = {
                let mut d = self.shared.state.lock().unwrap();
                match d.read_into(buf) {
                    PipeReadStep::Read(n) => {
                        drop(d);
                        if n != 0 {
                            self.shared.write_waiters.broadcast();
                        }
                        return Ok(n);
                    }
                    PipeReadStep::Eof => return Ok(0),
                    PipeReadStep::WouldBlock if nonblock => return Err("eagain"),
                    PipeReadStep::WouldBlock => {
                        self.shared.read_waiters.enqueue_task_locked(task_id)
                    }
                }
            };

            let outcome = token.wait_interruptible(None);
            self.shared.read_waiters.remove_waiter(&token);
            if outcome == WaitOutcome::Signal {
                return Err("eintr");
            }
        }
    }
    // AGENT: preserve PIPE_BUF atomicity, large-write partial progress, and
    // interruptible full-pipe waits while surfacing broken-peer notification.
    pub fn write_at(
        &self,
        task_id: usize,
        nonblock: bool,
        buf: &[u8],
    ) -> Result<PipeWriteOutcome, &'static str> {
        if self.dir != PipeDir::Wr {
            return Err("ebadf");
        }
        if buf.is_empty() {
            return Ok(PipeWriteOutcome::Written(0));
        }

        let atomic_request = buf.len() <= PIPE_BUF;
        let mut written = 0;
        loop {
            let token = {
                let mut d = self.shared.state.lock().unwrap();
                match d.write_from(&buf[written..], atomic_request) {
                    PipeWriteStep::Wrote(n) => {
                        drop(d);
                        written += n;
                        if n != 0 {
                            self.shared.read_waiters.broadcast();
                        }
                        if written == buf.len() || nonblock {
                            return Ok(PipeWriteOutcome::Written(written));
                        }
                        continue;
                    }
                    PipeWriteStep::Broken => {
                        return Ok(PipeWriteOutcome::Broken { written });
                    }
                    PipeWriteStep::WouldBlock if nonblock => {
                        return if written == 0 {
                            Err("eagain")
                        } else {
                            Ok(PipeWriteOutcome::Written(written))
                        };
                    }
                    PipeWriteStep::WouldBlock => {
                        self.shared.write_waiters.enqueue_task_locked(task_id)
                    }
                }
            };

            let outcome = token.wait_interruptible(None);
            self.shared.write_waiters.remove_waiter(&token);
            if outcome == WaitOutcome::Signal {
                return if written == 0 {
                    Err("eintr")
                } else {
                    Ok(PipeWriteOutcome::Written(written))
                };
            }
        }
    }

    // AGENT: identify both endpoints of one pipe without exposing PipeShared or
    // confusing descriptor/OFD identity with pipe-buffer identity.
    pub fn same_pipe(&self, other: &PipeNode) -> bool {
        Arc::ptr_eq(&self.shared, &other.shared)
    }

    // AGENT: wait for one writable pipe window, then atomically commit a
    // regular-file read and pipe fill so failures never advance only one side.
    pub fn splice_from_file(
        &self,
        task_id: usize,
        nonblock: bool,
        file: &FHandle,
        file_status: FdOpt,
        file_pos: &mut SpliceFilePos,
        count: usize,
    ) -> Result<SpliceOutcome, &'static str> {
        if self.dir != PipeDir::Wr {
            return Err("ebadf");
        }
        loop {
            let token = {
                let mut state = self.shared.state.lock().unwrap();
                if state.readers == 0 {
                    return Ok(SpliceOutcome::BrokenPipe { moved: 0 });
                }
                let available = state.buf.remaining();
                if available != 0 {
                    let bytes = file.splice_read(file_status, file_pos, min(count, available))?;
                    if bytes.is_empty() {
                        return Ok(SpliceOutcome::Moved(0));
                    }
                    let moved = state.buf.fill_from(&bytes);
                    debug_assert_eq!(moved, bytes.len());
                    state.publish_readiness();
                    drop(state);
                    self.shared.read_waiters.broadcast();
                    return Ok(SpliceOutcome::Moved(moved));
                }
                if nonblock {
                    return Err("eagain");
                }
                self.shared.write_waiters.enqueue_task_locked(task_id)
            };

            let outcome = token.wait_interruptible(None);
            self.shared.write_waiters.remove_waiter(&token);
            if outcome == WaitOutcome::Signal {
                return Err("eintr");
            }
        }
    }

    // AGENT: peek one readable pipe prefix and consume it only after the
    // regular-file write has committed, preserving pipe bytes on write errors.
    pub fn splice_to_file(
        &self,
        task_id: usize,
        nonblock: bool,
        file: &FHandle,
        file_status: FdOpt,
        file_pos: &mut SpliceFilePos,
        count: usize,
    ) -> Result<SpliceOutcome, &'static str> {
        if self.dir != PipeDir::Rd {
            return Err("ebadf");
        }
        loop {
            let token = {
                let mut state = self.shared.state.lock().unwrap();
                if !state.buf.empty() {
                    let mut bytes = Vec::new();
                    state.buf.peek_to(&mut bytes, min(count, state.buf.len()));
                    let moved = file.splice_write(file_status, file_pos, &bytes)?;
                    let discarded = state.buf.discard(moved);
                    debug_assert_eq!(discarded, moved);
                    state.publish_readiness();
                    drop(state);
                    if moved != 0 {
                        self.shared.write_waiters.broadcast();
                    }
                    return Ok(SpliceOutcome::Moved(moved));
                }
                if state.writers == 0 {
                    return Ok(SpliceOutcome::Moved(0));
                }
                if nonblock {
                    return Err("eagain");
                }
                self.shared.read_waiters.enqueue_task_locked(task_id)
            };

            let outcome = token.wait_interruptible(None);
            self.shared.read_waiters.remove_waiter(&token);
            if outcome == WaitOutcome::Signal {
                return Err("eintr");
            }
        }
    }

    // AGENT: classify one lock-held pipe-to-pipe attempt and enqueue any
    // required waiter before releasing the condition's pipe-state locks.
    fn prepare_pipe_splice_locked(
        &self,
        output: &PipeNode,
        input: &mut PipeBuf,
        output_state: &mut PipeBuf,
        task_id: usize,
        nonblock: bool,
        count: usize,
    ) -> Result<PipeSpliceAction, &'static str> {
        if output_state.readers == 0 {
            return Ok(PipeSpliceAction::Complete(SpliceOutcome::BrokenPipe {
                moved: 0,
            }));
        }
        if input.buf.empty() {
            if input.writers == 0 {
                return Ok(PipeSpliceAction::Complete(SpliceOutcome::Moved(0)));
            }
            if nonblock {
                return Err("eagain");
            }
            return Ok(PipeSpliceAction::WaitInput(
                self.shared.read_waiters.enqueue_task_locked(task_id),
            ));
        }
        if output_state.buf.remaining() == 0 {
            if nonblock {
                return Err("eagain");
            }
            return Ok(PipeSpliceAction::WaitOutput(
                output.shared.write_waiters.enqueue_task_locked(task_id),
            ));
        }

        let moved = min(count, min(input.buf.len(), output_state.buf.remaining()));
        for _ in 0..moved {
            let byte = input.buf.pop().expect("checked splice input length");
            assert!(output_state.buf.push(byte));
        }
        input.publish_readiness();
        output_state.publish_readiness();
        Ok(PipeSpliceAction::Complete(SpliceOutcome::Moved(moved)))
    }

    // AGENT: move one currently available FIFO prefix between distinct pipes,
    // ordering both state locks by allocation identity to prevent ABBA deadlock.
    pub fn splice_to_pipe(
        &self,
        output: &PipeNode,
        task_id: usize,
        nonblock: bool,
        count: usize,
    ) -> Result<SpliceOutcome, &'static str> {
        if self.dir != PipeDir::Rd || output.dir != PipeDir::Wr {
            return Err("ebadf");
        }
        if self.same_pipe(output) {
            return Err("einval");
        }

        loop {
            let input_key = Arc::as_ptr(&self.shared) as usize;
            let output_key = Arc::as_ptr(&output.shared) as usize;
            let action = if input_key < output_key {
                let mut input = self.shared.state.lock().unwrap();
                let mut output_state = output.shared.state.lock().unwrap();
                self.prepare_pipe_splice_locked(
                    output,
                    &mut input,
                    &mut output_state,
                    task_id,
                    nonblock,
                    count,
                )?
            } else {
                let mut output_state = output.shared.state.lock().unwrap();
                let mut input = self.shared.state.lock().unwrap();
                self.prepare_pipe_splice_locked(
                    output,
                    &mut input,
                    &mut output_state,
                    task_id,
                    nonblock,
                    count,
                )?
            };

            match action {
                PipeSpliceAction::Complete(result) => {
                    if matches!(result, SpliceOutcome::Moved(moved) if moved != 0) {
                        self.shared.write_waiters.broadcast();
                        output.shared.read_waiters.broadcast();
                    }
                    return Ok(result);
                }
                PipeSpliceAction::WaitInput(token) => {
                    let outcome = token.wait_interruptible(None);
                    self.shared.read_waiters.remove_waiter(&token);
                    if outcome == WaitOutcome::Signal {
                        return Err("eintr");
                    }
                }
                PipeSpliceAction::WaitOutput(token) => {
                    let outcome = token.wait_interruptible(None);
                    output.shared.write_waiters.remove_waiter(&token);
                    if outcome == WaitOutcome::Signal {
                        return Err("eintr");
                    }
                }
            }
        }
    }

    // AGENT: poll reuses the same readiness bit calculation as epoll
    // registration, so pipe readiness has one local source of truth.
    pub fn poll(&self) -> PollStatus {
        let d = self.shared.state.lock().unwrap();
        PollStatus::from_ready_bits(d.readiness(self.dir))
    }
    // AGENT: expose endpoint access mode to FLike without leaking PipeDir.
    pub fn status_flags(&self) -> FdOpt {
        FdOpt {
            rd: self.dir == PipeDir::Rd,
            wr: self.dir == PipeDir::Wr,
            ap: false,
            nb: false,
        }
    }
    // AGENT: FIONREAD/TIOCINQ reports the bytes currently buffered for the pipe.
    pub fn readable_len(&self) -> usize {
        self.shared.state.lock().unwrap().readable_len()
    }
}
