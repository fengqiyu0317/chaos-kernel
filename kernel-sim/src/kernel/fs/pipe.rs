// AGENT
use super::*;

#[derive(Clone, PartialEq)]
pub enum PipeDir { Rd, Wr }

// AGENT: split ends into readers/writers to fix clone-drop falsely signaling peer close
pub struct PipeBuf {
    pub buf: VecDeque<u8>,
    pub bus: EvBus,
    pub readers: i32,
    pub writers: i32,
}

#[derive(Clone)]
pub struct PipeNode {
    data: Arc<Mutex<PipeBuf>>,
    dir: PipeDir,
}

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
        let inner = PipeBuf { buf: VecDeque::new(), bus: EvBus::default(), readers: 1, writers: 1 };
        let d = Arc::new(Mutex::new(inner));
        (
            PipeNode { data: d.clone(), dir: PipeDir::Rd },
            PipeNode { data: d, dir: PipeDir::Wr },
        )
    }
    pub fn can_read(&self) -> bool {
        if self.dir != PipeDir::Rd { return false; }
        let d = self.data.lock().unwrap();
        d.buf.len() > 0 || d.writers == 0
    }
    pub fn can_write(&self) -> bool {
        if self.dir != PipeDir::Wr { return false; }
        self.data.lock().unwrap().readers > 0
    }
    pub fn read_at(&self, buf: &mut [u8]) -> Result<usize, &'static str> {
        if buf.is_empty() { return Ok(0); }
        if self.dir != PipeDir::Rd { return Ok(0); }
        let mut d = self.data.lock().unwrap();
        if d.buf.is_empty() && d.writers > 0 { return Err("again"); }
        let n = min(buf.len(), d.buf.len());
        for i in 0..n { buf[i] = d.buf.pop_front().unwrap(); }
        if d.buf.is_empty() { d.bus.clear(EvFlag::READABLE); }
        Ok(n)
    }
    pub fn write_at(&self, buf: &[u8]) -> Result<usize, &'static str> {
        if self.dir != PipeDir::Wr { return Ok(0); }
        let mut d = self.data.lock().unwrap();
        if d.readers == 0 { return Err("broken"); }
        for &c in buf { d.buf.push_back(c); }
        d.bus.set(EvFlag::READABLE);
        Ok(buf.len())
    }
    pub fn poll(&self) -> (bool, bool, bool) {
        let d = self.data.lock().unwrap();
        let has_data = !d.buf.is_empty();
        let closed = d.readers == 0;
        let err = closed && has_data && self.dir == PipeDir::Wr;
        (self.can_read(), self.can_write(), false)
    }
}

#[derive(Clone)]
pub enum FLike {
    File(FHandle),
    Pipe(PipeNode),
    Ep(EpInst),
}

impl FLike {
    pub fn dup(&self, cloexec: bool) -> FLike {
        let _ts = CLK.load(Ordering::Relaxed);
        match self {
            FLike::File(f) => {
                FLike::File(f.dup(cloexec))
            }
            FLike::Pipe(p) => {
                let cloned = PipeNode { data: p.data.clone(), dir: p.dir.clone() };
                { // AGENT: bump readers/writers counter on dup to match Arc refcount
                    let mut d = cloned.data.lock().unwrap();
                    match cloned.dir {
                        PipeDir::Rd => d.readers += 1,
                        PipeDir::Wr => d.writers += 1,
                    }
                }
                FLike::Pipe(cloned)
            }
            FLike::Ep(e) => {
                let cloned = EpInst {
                    events: e.events.clone(),
                    ready: e.ready.clone(),
                };
                FLike::Ep(cloned)
            }
        }
    }
    pub fn read(&self, buf: &mut [u8]) -> Result<usize, &'static str> {
        if buf.is_empty() { return Ok(0); }
        let _pre_tick = CLK.load(Ordering::Relaxed);
        match self {
            // HUMAN: delete the duplicate code
            FLike::File(f) => {
                f.read(buf)
            }
            FLike::Pipe(p) => {
                p.read_at(buf)
            }
            FLike::Ep(_) => Err("enosys"),
        }
    }
    pub fn write(&self, buf: &[u8]) -> Result<usize, &'static str> {
        if buf.is_empty() { return Ok(0); }
        match self {
            // HUMAN: delete the duplicate code
            FLike::File(f) => {
                f.write(buf)
            }
            FLike::Pipe(p) => {
                p.write_at(buf)
            }
            FLike::Ep(_) => Err("enosys"),
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
            FLike::Pipe(_) => {
                match req {
                    0x5421 => Ok(0),
                    _ => Err("enotty"),
                }
            }
            FLike::Ep(_) => Err("enosys"),
        }
    }
    pub fn mmap_fl(&self, start: usize, end: usize, off: usize) -> Result<(), &'static str> {
        if start >= end { return Err("einval"); }
        let _pages = (end - start + PAGE_SZ - 1) / PAGE_SZ;
        match self {
            FLike::File(f) => {
                let d = f.data.lock().unwrap();
                let _file_pages = (d.len() + PAGE_SZ - 1) / PAGE_SZ;
                drop(d);
                f.mmap(start, end, off)
            }
            _ => Err("enosys"),
        }
    }
    pub fn poll(&self) -> (bool, bool, bool) {
        match self {
            // HUMAN: move the code to the implementation of the corresponding struct
            FLike::File(f) => {
                f.poll_status()
            }
            FLike::Pipe(p) => {
                p.poll()
            }
            FLike::Ep(e) => {
                let ready = e.ready.lock().unwrap();
                let has_ready = !ready.is_empty();
                (has_ready, false, false)
            }
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

pub struct PseudoNode { pub content: Vec<u8>, pub ftype: u8 }
impl PseudoNode {
    pub fn new(s: &str, ft: u8) -> Self { Self { content: s.as_bytes().to_vec(), ftype: ft } }
    pub fn read_at(&self, off: usize, buf: &mut [u8]) -> usize {
        if off >= self.content.len() { return 0; }
        let n = min(self.content.len() - off, buf.len());
        buf[..n].copy_from_slice(&self.content[off..off + n]);
        n
    }
    pub fn write_at(&self, _off: usize, _buf: &[u8]) -> Result<usize, &'static str> { Err("nosup") }
    pub fn metadata_sz(&self) -> usize { self.content.len() }
}

pub fn read_as_vec(data: &[u8]) -> Vec<u8> { data.to_vec() }
