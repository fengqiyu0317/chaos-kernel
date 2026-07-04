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
                let eof = d.writers == 0;
                let mut ready = 0;
                if !d.buf.is_empty() || eof {
                    ready |= EvFlag::READABLE;
                }
                if eof {
                    ready |= EvFlag::CLOSED;
                }
                ready
            }
            PipeDir::Wr => {
                let broken = d.readers == 0;
                let mut ready = 0;
                if broken {
                    ready |= EvFlag::CLOSED | EvFlag::ERROR;
                } else {
                    ready |= EvFlag::WRITABLE;
                }
                ready
            }
        }
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
    pub fn register_epoll(&self, fd: usize, ep: EpInst, ev: &EpEvent) -> Option<usize> {
        let wake_mask = self.epoll_wake_mask(ev.events);
        let (sub_id, ready_now) = {
            let mut pipe = self.data.lock().unwrap();
            let ready = self.readiness_locked(&pipe);
            let target_epoll = ep.clone();
            let sub_id = pipe.bus.sub(
                wake_mask,
                Box::new(move |_bus_ev| {
                    target_epoll.mark_ready(fd);
                    false
                }),
            );
            (sub_id, (ready & wake_mask) != 0)
        };
        if ready_now {
            ep.mark_ready(fd);
        }
        Some(sub_id)
    }
    // AGENT: remove an epoll readiness subscription previously installed on
    // this pipe's EvBus.
    pub fn unregister_epoll(&self, sub_id: usize) -> bool {
        self.data.lock().unwrap().bus.unsub(sub_id)
    }
    // AGENT: empty reads are a no-op; an empty pipe returns AGAIN while writers
    // exist and EOF once the last writer is gone.
    pub fn read_at(&self, buf: &mut [u8]) -> Result<usize, &'static str> {
        if buf.is_empty() {
            return Ok(0);
        }
        if self.dir != PipeDir::Rd {
            return Ok(0);
        }
        let mut d = self.data.lock().unwrap();
        if d.buf.is_empty() {
            return if d.writers == 0 { Ok(0) } else { Err("again") };
        }

        let n = min(buf.len(), d.buf.len());
        for dst in buf.iter_mut().take(n) {
            *dst = d.buf.pop_front().unwrap();
        }
        if d.buf.is_empty() {
            d.bus.clear(EvFlag::READABLE);
        }
        Ok(n)
    }
    // AGENT: writes publish READABLE and broken-pipe ERROR/CLOSED readiness to
    // EvBus subscribers.
    pub fn write_at(&self, buf: &[u8]) -> Result<usize, &'static str> {
        if buf.is_empty() {
            return Ok(0);
        }
        if self.dir != PipeDir::Wr {
            return Ok(0);
        }
        let mut d = self.data.lock().unwrap();
        if d.readers == 0 {
            d.bus.set(EvFlag::CLOSED | EvFlag::ERROR);
            return Err("broken");
        }

        d.buf.extend(buf.iter().copied());
        d.bus.set(EvFlag::READABLE);
        Ok(buf.len())
    }
    // AGENT: poll reuses the same readiness bit calculation as epoll
    // registration, so pipe readiness has one local source of truth.
    pub fn poll(&self) -> PollStatus {
        let d = self.data.lock().unwrap();
        PollStatus::from_ready_bits(self.readiness_locked(&d))
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
        self.data.lock().unwrap().buf.len()
    }
}

pub fn read_as_vec(data: &[u8]) -> Vec<u8> {
    data.to_vec()
}
