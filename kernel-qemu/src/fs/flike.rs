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
    // AGENT: expose only initial object access mode here. Runtime fd I/O belongs
    // to FHandle for offsets and OpenFileDesc for mutable status flags.
    pub fn status_flags(&self) -> FdOpt {
        match self {
            // AGENT: file access/status is supplied explicitly when the open-file
            // description is created; this default is only a compatibility seed.
            FLike::File(_) => FdOpt::default(),
            FLike::Pipe(p) => p.status_flags(),
            FLike::Ep(_) => FdOpt {
                rd: true,
                wr: false,
                ap: false,
                nb: false,
            },
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
