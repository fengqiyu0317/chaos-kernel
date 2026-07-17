// AGENT: keep scheduler-visible task metadata separate from Task behavior.
use super::*;

// AGENT: store scheduler-visible task identity and its diagnostic tag together.
#[derive(Clone, Debug)]
pub struct TaskInfo {
    pub id: usize,
    pub tag: String,
}
