// AGENT: keep process-group membership and session ownership in one authority.
use super::*;

// AGENT: store only data owned by one process group; pgid remains the map key.
struct ProcessGroup {
    members: BTreeSet<usize>,
    session_id: usize,
}

// AGENT: initialize a group with its leader as the first process member.
impl ProcessGroup {
    fn new(leader: usize, session_id: usize) -> Self {
        Self {
            members: BTreeSet::from([leader]),
            session_id,
        }
    }
}

// AGENT: index both directions of job control under one lock so Process no
// longer mirrors pgid and sid fields that can diverge from group membership.
#[derive(Default)]
pub(super) struct JobControl {
    groups: BTreeMap<i32, ProcessGroup>,
    process_groups: BTreeMap<usize, i32>,
}

// AGENT: centralize every process-group/session transition and its invariants.
impl JobControl {
    pub(super) fn membership(&self, pid: usize) -> Option<(i32, usize)> {
        let pgid = *self.process_groups.get(&pid)?;
        let session_id = self.groups.get(&pgid)?.session_id;
        Some((pgid, session_id))
    }

    pub(super) fn members(&self, pgid: i32) -> Vec<usize> {
        self.groups
            .get(&pgid)
            .map(|group| group.members.iter().copied().collect())
            .unwrap_or_default()
    }

    pub(super) fn add_process(
        &mut self,
        pid: usize,
        pgid: i32,
        session_id: usize,
    ) -> Result<(), &'static str> {
        if self.process_groups.contains_key(&pid) {
            return Err("eexist");
        }

        match self.groups.get_mut(&pgid) {
            Some(group) => {
                if group.session_id != session_id {
                    return Err("eperm");
                }
                group.members.insert(pid);
            }
            None => {
                if pgid != pid as i32 {
                    return Err("eperm");
                }
                self.groups.insert(pgid, ProcessGroup::new(pid, session_id));
            }
        }
        self.process_groups.insert(pid, pgid);
        Ok(())
    }

    // AGENT: remove both job-control indexes through the shared group cleanup path.
    pub(super) fn remove_process(&mut self, pid: usize) {
        let Some(pgid) = self.process_groups.remove(&pid) else {
            return;
        };
        self.remove_member_from_group(pid, pgid);
    }

    // AGENT: update an existing group or create a new one without inserting pid twice.
    pub(super) fn move_process(&mut self, pid: usize, new_pgid: i32) -> Result<(), &'static str> {
        let (old_pgid, session_id) = self.membership(pid).ok_or("esrch")?;
        if old_pgid == new_pgid {
            return Ok(());
        }

        match self.groups.get(&new_pgid) {
            Some(group) if group.session_id != session_id => return Err("eperm"),
            None if new_pgid != pid as i32 => return Err("eperm"),
            _ => {}
        }

        self.remove_member_from_group(pid, old_pgid);
        self.groups
            .entry(new_pgid)
            .and_modify(|group| {
                group.members.insert(pid);
            })
            .or_insert_with(|| ProcessGroup::new(pid, session_id));
        self.process_groups.insert(pid, new_pgid);
        Ok(())
    }

    pub(super) fn start_new_session(&mut self, pid: usize) -> Result<(), &'static str> {
        let (old_pgid, _) = self.membership(pid).ok_or("esrch")?;
        if old_pgid as usize == pid {
            return Err("eperm");
        }

        let new_pgid = i32::try_from(pid).map_err(|_| "einval")?;
        if self.groups.contains_key(&new_pgid) {
            return Err("eperm");
        }

        self.remove_member_from_group(pid, old_pgid);
        self.groups.insert(new_pgid, ProcessGroup::new(pid, pid));
        self.process_groups.insert(pid, new_pgid);
        Ok(())
    }

    fn remove_member_from_group(&mut self, pid: usize, pgid: i32) {
        if let Some(group) = self.groups.get_mut(&pgid) {
            group.members.remove(&pid);
        }
        if self
            .groups
            .get(&pgid)
            .is_some_and(|group| group.members.is_empty())
        {
            self.groups.remove(&pgid);
        }
    }
}
