use super::*;

impl Kernel {
    // AGENT: expose the per-hart Processor current task used by scheduling and
    // syscalls without exposing its idle context.
    pub fn cur_task(&self, cpu: usize) -> Option<Arc<Task>> {
        self.processors
            .get(cpu)
            .and_then(|processor| processor.lock().unwrap().current())
    }

    // AGENT: publish the one authoritative current task for this hart; callers
    // that create wait state pass the selected Task::id() down explicitly.
    pub fn set_cur(&self, cpu: usize, t: Option<Arc<Task>>) {
        let Some(processor) = self.processors.get(cpu) else {
            return;
        };
        let mut processor = processor.lock().unwrap();
        processor.set_current(t);
    }

    // AGENT: report whether CPU0 has initialized the idle-side switch context;
    // callers use this only to enforce one-time scheduler setup.
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
    pub(crate) fn switch_current_to_idle(&self, cpu: usize) {
        assert_eq!(cpu, 0, "only CPU0 owns a scheduler context");
        let restore_interrupts = crate::csr::read_sstatus() & crate::csr::SSTATUS_SIE != 0;
        crate::csr::disable_interrupts();
        let (current_context, idle_context) = {
            let mut processor = self.processors[0].lock().unwrap();
            assert!(
                processor.scheduler_active(),
                "CPU0 idle context is not initialized"
            );
            let current_context = processor
                .take_current_context()
                .expect("CPU0 scheduler has no current task to switch");
            (current_context, processor.idle_context_ptr())
        };
        unsafe {
            crate::context::switch_kernel_context(current_context, idle_context);
        }
        if restore_interrupts {
            crate::csr::enable_interrupts();
        }
    }
}
