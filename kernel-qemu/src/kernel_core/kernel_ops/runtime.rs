use super::*;

impl Kernel {
    // AGENT: expose the per-CPU current-task slot used by scheduling and syscalls.
    pub fn cur_task(&self, cpu: usize) -> Option<Arc<Task>> {
        let cg = self.cpus.lock().unwrap();
        cg.get(cpu).and_then(|slot| slot.as_ref().cloned())
    }

    // AGENT: update the per-CPU current-task slot and the CPU0-only current-id
    // bridge used by low-level sync code.
    pub fn set_cur(&self, cpu: usize, t: Option<Arc<Task>>) {
        let mut cg = self.cpus.lock().unwrap();
        if cpu < cg.len() {
            if cpu == 0 {
                set_current_task_id(t.as_ref().map(|task| task.id()));
            }
            cg[cpu] = t;
        }
    }
}
