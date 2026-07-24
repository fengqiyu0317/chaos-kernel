// AGENT
use super::*;

// AGENT: keep the polymorphic fd object enum out of pipe.rs, and model the QEMU
// terminal as a typed object rather than a path-tagged regular file.
#[derive(Clone)]
pub enum FLike {
    File(FHandle),
    Pipe(PipeNode),
    Ep(EpInst),
    Tty(TtyDevice),
}

impl FLike {
    // AGENT: expose only initial object access mode here, including the terminal
    // device capability seed. OpenFileDesc still owns mutable status flags.
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
            FLike::Tty(_) => FdOpt {
                rd: true,
                wr: true,
                ap: false,
                nb: false,
            },
        }
    }

    // AGENT: register an epoll readiness callback when this file-like object
    // exposes a cancellable source; regular files remain level-polled.
    pub fn register_epoll(&self, key: &EpKey, ep: &EpInst, ev: &EpEvent) -> Option<usize> {
        match self {
            FLike::Pipe(p) => p.register_epoll(key, ep, ev),
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

// AGENT: expose concrete file-like kinds in diagnostics without depending on
// regular-file path names to distinguish the terminal device.
impl fmt::Debug for FLike {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            FLike::File(h) => write!(f, "F({:?})", h),
            FLike::Pipe(_) => write!(f, "P"),
            FLike::Ep(_) => write!(f, "E"),
            FLike::Tty(_) => write!(f, "T"),
        }
    }
}
