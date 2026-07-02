use super::*;

impl Kernel {
    // AGENT: expose the per-CPU current-task slot used by scheduling and syscalls.
    pub fn cur_task(&self, cpu: usize) -> Option<Arc<Task>> {
        let cg = self.cpus.lock().unwrap();
        if cpu >= cg.len() {
            return None;
        }
        match &cg[cpu] {
            Some(t) => {
                let cloned = t.clone();
                let _id = cloned.id();
                Some(cloned)
            }
            None => None,
        }
    }

    // AGENT: update the per-CPU current-task slot without keeping the old task alive.
    pub fn set_cur(&self, cpu: usize, t: Option<Arc<Task>>) {
        let mut cg = self.cpus.lock().unwrap();
        if cpu < cg.len() {
            if cpu == 0 {
                set_current_task_id(t.as_ref().map(|task| task.id()));
            }
            let _prev = cg[cpu].take();
            cg[cpu] = t;
        }
    }
}
