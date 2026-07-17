// AGENT: keep process-identifier representation separate from task behavior.
use super::*;

// AGENT: keep process identifiers as a small typed wrapper shared by task code.
#[derive(Clone)]
pub struct Pid(pub usize);

// AGENT: centralize pid construction and init-process checks.
impl Pid {
    pub const INIT: usize = 1;

    // AGENT: construct the unregistered pid sentinel used by fresh processes.
    pub fn new() -> Self {
        Pid(0)
    }

    // AGENT: expose the numeric pid at process-table boundaries.
    pub fn get(&self) -> usize {
        self.0
    }

    // AGENT: identify the distinguished init process without duplicating pid 1.
    pub fn is_init(&self) -> bool {
        self.0 == Self::INIT
    }
}

// AGENT: format pids using their numeric userspace representation.
impl fmt::Display for Pid {
    // AGENT: delegate pid display to the wrapped integer.
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}
