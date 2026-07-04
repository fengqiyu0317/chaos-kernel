// AGENT
use super::*;

// AGENT: keep QEMU pipe buffering bounded while reusing the existing byte ring
// buffer implementation instead of growing VecDeque without backpressure.
const PIPE_BUF_CAPACITY: usize = 4 * 1024;

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
    buf: CircBuf,
    bus: EvBus,
    readers: i32,
    writers: i32,
}

impl PipeBuf {
    // AGENT: centralize pipe-buffer construction so PipeNode no longer reaches
    // into CircBuf/EvBus fields when creating endpoint pairs.
    fn new() -> Self {
        let mut bus = EvBus::default();
        bus.set(EvFlag::WRITABLE);
        Self {
            buf: CircBuf::new(PIPE_BUF_CAPACITY),
            bus,
            readers: 1,
            writers: 1,
        }
    }

    // AGENT: keep endpoint reference accounting with the shared buffer state it
    // protects instead of open-coding reader/writer mutations in PipeNode.
    fn add_endpoint_ref(&mut self, dir: PipeDir) {
        match dir {
            PipeDir::Rd => self.readers += 1,
            PipeDir::Wr => self.writers += 1,
        }
    }

    // AGENT: publish peer-close readiness from the buffer owner whenever the
    // last reader or writer endpoint is dropped.
    fn release_endpoint_ref(&mut self, dir: PipeDir) {
        match dir {
            PipeDir::Rd => self.readers -= 1,
            PipeDir::Wr => self.writers -= 1,
        }
        if self.readers == 0 || self.writers == 0 {
            self.bus.set(EvFlag::CLOSED);
        }
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

    // AGENT: attach epoll callbacks to the buffer's EvBus while using the same
    // readiness calculation that poll() observes.
    fn subscribe_epoll(
        &mut self,
        dir: PipeDir,
        wake_mask: u32,
        fd: usize,
        ep: &EpInst,
    ) -> (usize, bool) {
        let ready = self.readiness(dir);
        let target_epoll = ep.clone();
        let sub_id = self.bus.sub(
            wake_mask,
            Box::new(move |_bus_ev| {
                target_epoll.mark_ready(fd);
                false
            }),
        );
        (sub_id, (ready & wake_mask) != 0)
    }

    // AGENT: remove epoll source subscriptions from the buffer-owned EvBus.
    fn unsubscribe_epoll(&mut self, sub_id: usize) -> bool {
        self.bus.unsub(sub_id)
    }

    // AGENT: keep pipe read-side buffer mutation and readiness publication
    // inside PipeBuf; PipeNode only decides whether the endpoint may read.
    fn read_into(&mut self, out: &mut [u8]) -> Result<usize, &'static str> {
        if out.is_empty() {
            return Ok(0);
        }
        if self.buf.empty() {
            return if self.writers == 0 {
                Ok(0)
            } else {
                Err("again")
            };
        }

        let n = min(out.len(), self.buf.len());
        for dst in out.iter_mut().take(n) {
            *dst = self.buf.pop().unwrap();
        }
        let mut clear = 0;
        let mut set = 0;
        if self.buf.empty() {
            clear |= EvFlag::READABLE;
        }
        if self.writers > 0 && self.buf.remaining() > 0 {
            set |= EvFlag::WRITABLE;
        }
        self.bus.change(clear, set);
        Ok(n)
    }

    // AGENT: keep write-side capacity checks, broken-pipe state, and EvBus
    // publication with the buffer state they mutate.
    fn write_from(&mut self, input: &[u8]) -> Result<usize, &'static str> {
        if input.is_empty() {
            return Ok(0);
        }
        if self.readers == 0 {
            self.bus.set(EvFlag::CLOSED | EvFlag::ERROR);
            return Err("broken");
        }
        if self.buf.remaining() == 0 {
            self.bus.clear(EvFlag::WRITABLE);
            return Err("again");
        }

        let written = self.buf.fill_from(input);
        let clear = if self.buf.remaining() == 0 {
            EvFlag::WRITABLE
        } else {
            0
        };
        let set = EvFlag::READABLE
            | if self.buf.remaining() > 0 {
                EvFlag::WRITABLE
            } else {
                0
            };
        self.bus.change(clear, set);
        Ok(written)
    }

    // AGENT: keep buffered-byte queries with PipeBuf so ioctl callers do not
    // require direct access to CircBuf.
    fn readable_len(&self) -> usize {
        self.buf.len()
    }
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
            d.add_endpoint_ref(cloned.dir);
        }
        cloned
    }
}

// AGENT: endpoint drop publishes pipe closure to the shared readiness bus.
impl Drop for PipeNode {
    fn drop(&mut self) {
        let mut d = self.data.lock().unwrap();
        d.release_endpoint_ref(self.dir);
    }
}

impl PipeNode {
    // AGENT: keep endpoint-pair construction at PipeNode while delegating shared
    // buffer initialization to PipeBuf.
    pub fn pair() -> (PipeNode, PipeNode) {
        let d = Arc::new(Mutex::new(PipeBuf::new()));
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
            pipe.subscribe_epoll(self.dir, wake_mask, fd, &ep)
        };
        if ready_now {
            ep.mark_ready(fd);
        }
        Some(sub_id)
    }
    // AGENT: remove an epoll readiness subscription previously installed on
    // this pipe's EvBus.
    pub fn unregister_epoll(&self, sub_id: usize) -> bool {
        self.data.lock().unwrap().unsubscribe_epoll(sub_id)
    }
    // AGENT: empty reads are a no-op; an empty pipe returns AGAIN while writers
    // exist and EOF once the last writer is gone.
    pub fn read_at(&self, buf: &mut [u8]) -> Result<usize, &'static str> {
        if self.dir != PipeDir::Rd {
            return Ok(0);
        }
        let mut d = self.data.lock().unwrap();
        d.read_into(buf)
    }
    // AGENT: writes publish READABLE/WRITABLE transitions and broken-pipe
    // ERROR/CLOSED readiness to EvBus subscribers.
    pub fn write_at(&self, buf: &[u8]) -> Result<usize, &'static str> {
        if self.dir != PipeDir::Wr {
            return Ok(0);
        }
        let mut d = self.data.lock().unwrap();
        d.write_from(buf)
    }
    // AGENT: poll reuses the same readiness bit calculation as epoll
    // registration, so pipe readiness has one local source of truth.
    pub fn poll(&self) -> PollStatus {
        let d = self.data.lock().unwrap();
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
        self.data.lock().unwrap().readable_len()
    }
}

pub fn read_as_vec(data: &[u8]) -> Vec<u8> {
    data.to_vec()
}
