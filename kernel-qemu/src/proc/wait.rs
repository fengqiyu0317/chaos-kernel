// AGENT
use super::*;

// AGENT: ProcessGroup keeps only group identity, membership, and session
// ownership. The group leader is derived from pgid, and foreground state
// belongs to the future session/TTY layer.

pub type Pgid = i32;

pub struct ProcessGroup {
    pub pgid: Pgid,
    pub members: Mutex<Vec<usize>>,
    pub session_id: usize,
}

impl ProcessGroup {
    // AGENT: leader is only the initial member pid; do not store it separately
    // from pgid.
    pub fn new(pgid: Pgid, leader: usize, session: usize) -> Self {
        Self {
            pgid,
            members: Mutex::new(vec![leader]),
            session_id: session,
        }
    }

    pub fn add_member(&self, pid: usize) {
        let mut members = self.members.lock().unwrap();
        if !members.contains(&pid) {
            members.push(pid);
        }
    }

    pub fn remove_member(&self, pid: usize) -> bool {
        let mut members = self.members.lock().unwrap();
        let before = members.len();
        members.retain(|&m| m != pid);
        members.len() < before
    }

    // AGENT: snapshot membership before looking up tasks so callers do not
    // hold the group member lock while entering TaskTable.
    pub fn members_snapshot(&self) -> Vec<usize> {
        self.members.lock().unwrap().clone()
    }

    pub fn is_empty(&self) -> bool {
        self.members.lock().unwrap().is_empty()
    }

    pub fn member_count(&self) -> usize {
        self.members.lock().unwrap().len()
    }

    // AGENT: process-group leader identity is represented by pgid.
    pub fn is_leader(&self, pid: usize) -> bool {
        self.pgid as usize == pid
    }
}
