use super::*;

impl Kernel {
    // AGENT: expose the per-hart Processor current task used by scheduling and
    // syscalls without exposing its idle context.
    pub fn cur_task(&self, cpu: usize) -> Option<Arc<Task>> {
        self.processors
            .get(cpu)
            .and_then(|processor| processor.lock().unwrap().current())
    }

    // AGENT: publish one Processor current task and the CPU0-only current-id
    // bridge used by low-level sync code.
    pub fn set_cur(&self, cpu: usize, t: Option<Arc<Task>>) {
        let Some(processor) = self.processors.get(cpu) else {
            return;
        };
        let task_id = t.as_ref().map(|task| task.id());
        let mut processor = processor.lock().unwrap();
        processor.set_current(t);
        if cpu == 0 {
            set_current_task_id(task_id);
        }
    }

    // AGENT: report whether a hart has entered its real idle/task switch loop;
    // metadata-only selftests retain their old no-switch behavior until then.
    pub(crate) fn scheduler_active(&self, cpu: usize) -> bool {
        self.processors
            .get(cpu)
            .is_some_and(|processor| processor.lock().unwrap().scheduler_active())
    }

    // AGENT: activate CPU0 scheduling and return its stable idle-context slot
    // after dropping the Processor guard.
    pub(crate) fn activate_cpu0_scheduler(&self) -> *mut crate::context::KernelContext {
        let mut processor = self.processors[0].lock().unwrap();
        processor.activate_scheduler();
        processor.idle_context_ptr()
    }

    // AGENT: detach the running CPU0 task before switching back to the idle
    // context, then restore the caller's SIE state if that task is resumed. The
    // scheduler loop retains the Arc<Task> across this suspension.
    pub(crate) fn switch_current_to_idle(&self, cpu: usize) -> bool {
        if cpu != 0 {
            return false;
        }
        let restore_interrupts = crate::csr::read_sstatus() & crate::csr::SSTATUS_SIE != 0;
        crate::csr::disable_interrupts();
        let contexts = {
            let mut processor = self.processors[0].lock().unwrap();
            if processor.scheduler_active() {
                processor.take_current_context().map(|current_context| {
                    set_current_task_id(None);
                    (current_context, processor.idle_context_ptr())
                })
            } else {
                None
            }
        };
        let Some((current_context, idle_context)) = contexts else {
            if restore_interrupts {
                crate::csr::enable_interrupts();
            }
            return false;
        };
        unsafe {
            crate::context::switch_kernel_context(current_context, idle_context);
        }
        if restore_interrupts {
            crate::csr::enable_interrupts();
        }
        true
    }
}
