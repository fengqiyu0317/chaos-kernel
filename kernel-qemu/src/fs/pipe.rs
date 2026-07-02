// AGENT
use super::*;

// AGENT: pipe endpoint direction is internal to this module; Copy keeps
// endpoint checks simple without exposing the enum outside pipe handling.
#[derive(Clone, Copy, PartialEq)]
enum PipeDir {
    Rd,
    Wr,
}

// AGENT: keep shared pipe state private while preserving explicit reader/writer
// counts for clone/drop peer-close semantics.
struct PipeBuf {
    buf: VecDeque<u8>,
    bus: EvBus,
    readers: i32,
    writers: i32,
}

pub struct PipeNode {
    data: Arc<Mutex<PipeBuf>>,
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

impl Clone for PipeNode {
    // AGENT: cloning a pipe endpoint represents another fd/reference to that
    // endpoint, so the explicit reader/writer counters must follow the clone.
    fn clone(&self) -> Self {
        let cloned = PipeNode {
            data: self.data.clone(),
            dir: self.dir,
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
    // AGENT: translate epoll interest into EvBus wake bits. ERR/HUP are
    // reported even when not requested, so every pipe subscription watches
    // closed/error transitions.
    fn epoll_bus_mask(&self, interest: u32) -> u32 {
        let readiness_mask = match self.dir {
            PipeDir::Rd if interest & (EpEvent::IN | EpEvent::RDNORM) != 0 => EvFlag::READABLE,
            PipeDir::Wr if interest & (EpEvent::OUT | EpEvent::WRNORM) != 0 => EvFlag::WRITABLE,
            _ => 0,
        };

        readiness_mask | EvFlag::CLOSED | EvFlag::ERROR
    }
    // AGENT: connect pipe readiness changes to an epoll instance through the
    // pipe's EvBus, while returning a cancellable subscription id.
    pub fn register_epoll(&self, fd: usize, ep: EpInst, ev: &EpEvent) -> Option<usize> {
        let mask = self.epoll_bus_mask(ev.events);
        let (sub_id, already_ready) = {
            let mut d = self.data.lock().unwrap();
            let ready = self.readiness_locked(&d);
            let callback_ep = ep.clone();
            let sub_id = d.bus.sub(
                mask,
                Box::new(move |_bus_ev| {
                    callback_ep.mark_ready(fd);
                    false
                }),
            );
            (sub_id, (ready & mask) != 0)
        };
        if already_ready {
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
    // AGENT: poll reuses the same readiness bit calculation as epoll
    // registration, so pipe readiness has one local source of truth.
    pub fn poll(&self) -> PollStatus {
        let d = self.data.lock().unwrap();
        let ready = self.readiness_locked(&d);
        PollStatus {
            readable: (ready & EvFlag::READABLE) != 0,
            writable: (ready & EvFlag::WRITABLE) != 0,
            error: (ready & EvFlag::ERROR) != 0,
            closed: (ready & EvFlag::CLOSED) != 0,
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
    // AGENT: expose explicit readiness fields for epoll's final event mapping.
    pub fn poll(&self) -> PollStatus {
        match self {
            // HUMAN: move the code to the implementation of the corresponding struct
            FLike::File(f) => f.poll_status(),
            FLike::Pipe(p) => p.poll(),
            FLike::Ep(e) => {
                let ready = e.ready.lock().unwrap();
                let has_ready = !ready.is_empty();
                PollStatus {
                    readable: has_ready,
                    ..PollStatus::default()
                }
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
}

pub fn read_as_vec(data: &[u8]) -> Vec<u8> {
    data.to_vec()
}
