use super::*;
use crate::context::KernelContext;

// AGENT: own one hart's current task and the stable idle-side switch context;
// CPU0 is the only scheduler owner until the multi-hart protocol is designed.
pub struct Processor {
    current: Option<Arc<Task>>,
    idle_context: KernelContext,
    scheduler_active: bool,
}

// AGENT: keep current-task publication and the idle context behind one per-hart
// state object while ensuring no Processor lock guard crosses __switch.
impl Processor {
    pub fn new() -> Self {
        Self {
            current: None,
            idle_context: KernelContext::idle(),
            scheduler_active: false,
        }
    }

    pub fn current(&self) -> Option<Arc<Task>> {
        self.current.clone()
    }

    pub fn set_current(&mut self, current: Option<Arc<Task>>) {
        self.current = current;
    }

    pub(crate) fn scheduler_active(&self) -> bool {
        self.scheduler_active
    }

    pub(crate) fn activate_scheduler(&mut self) {
        assert!(
            !self.scheduler_active,
            "CPU scheduler started more than once"
        );
        self.scheduler_active = true;
    }

    pub(crate) fn idle_context_ptr(&mut self) -> *mut KernelContext {
        core::ptr::addr_of_mut!(self.idle_context)
    }

    // AGENT: detach the running task without cloning an Arc onto the task stack;
    // the suspended scheduler frame already retains the selected task owner.
    pub(crate) fn take_current_context(&mut self) -> Option<*mut KernelContext> {
        let context = self.current.as_ref()?.kernel_context_ptr();
        self.current = None;
        Some(context)
    }
}
