// AGENT
use super::*;

// AGENT: keep the generic file-like enum out of pipe.rs so pipe only owns pipe
// endpoint state and readiness logic.
#[derive(Clone)]
pub enum FLike {
    File(FHandle),
    Pipe(PipeNode),
    Ep(EpInst),
}

impl FLike {
    pub fn read(&self, buf: &mut [u8]) -> Result<usize, &'static str> {
        if buf.is_empty() {
            return Ok(0);
        }
        match self {
            // AGENT: FLike only dispatches to the concrete readable object.
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
            // AGENT: FLike only dispatches to the concrete writable object.
            FLike::File(f) => f.write(buf),
            FLike::Pipe(p) => p.write_at(buf),
            FLike::Ep(_) => Err("enosys"),
        }
    }

    pub fn status_flags(&self) -> FdOpt {
        match self {
            FLike::File(f) => f.get_opt(),
            FLike::Pipe(p) => p.status_flags(),
            FLike::Ep(_) => FdOpt {
                rd: true,
                wr: false,
                ap: false,
                nb: false,
            },
        }
    }

    // AGENT: handle object-specific ioctl requests; fd-wide requests such as
    // FIONBIO are applied by sys_ioctl because they mutate descriptor status.
    pub fn io_ctl(&self, req: usize, a1: usize) -> Result<usize, &'static str> {
        match self {
            FLike::File(f) => f.io_ctl(req, a1),
            FLike::Pipe(p) => match req {
                FIONREAD => Ok(p.readable_len()),
                _ => Err("enotty"),
            },
            FLike::Ep(_) => Err("enosys"),
        }
    }

    // AGENT: expose explicit readiness fields for epoll's final event mapping.
    pub fn poll(&self) -> PollStatus {
        match self {
            FLike::File(f) => f.poll_status(),
            FLike::Pipe(p) => p.poll(),
            FLike::Ep(e) => e.poll_status(),
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
