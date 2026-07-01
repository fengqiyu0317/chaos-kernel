// AGENT
use super::*;

// AGENT: Keep only the stored scheduling priority; policy class, nice, and
// time-slice length are not separate state in the current scheduler.
#[derive(Clone)]
pub struct SchedulePolicy {
    pub prio: i32,
}

// AGENT: Derive the time-slice length from priority instead of storing a
// duplicate field in SchedulePolicy.
impl SchedulePolicy {
    pub fn new() -> Self {
        Self { prio: PRIO_DEFAULT }
    }

    pub fn with_prio(prio: i32) -> Self {
        let prio = prio.clamp(PRIO_MIN, PRIO_MAX);
        Self { prio }
    }

    pub fn time_slice(&self) -> usize {
        ((PRIO_MAX - self.prio + 1) / 2).max(1) as usize
    }
}

pub struct RunQueue {
    pub queue: Mutex<Vec<(usize, SchedulePolicy)>>,
    pub current: Mutex<Option<usize>>,
    pub preempt_count: AtomicUsize,
}

impl RunQueue {
    pub fn new() -> Self {
        Self {
            queue: Mutex::new(Vec::new()),
            current: Mutex::new(None),
            preempt_count: AtomicUsize::new(0),
        }
    }

    pub fn enqueue(&self, task_id: usize, policy: SchedulePolicy) {
        let mut q = self.queue.lock().unwrap();
        let dup = q.iter().any(|(id, _)| *id == task_id); // AGENT
        if dup {
            return;
        } // AGENT
        q.push((task_id, policy));
    }

    pub fn dequeue(&self) -> Option<(usize, SchedulePolicy)> {
        let mut q = self.queue.lock().unwrap();
        if q.is_empty() {
            return None;
        }
        let mut best_idx = 0;
        for idx in 1..q.len() {
            if Self::cmp_priority(&q[idx].1, &q[best_idx].1) == CmpOrd::Less {
                best_idx = idx;
            }
        }
        Some(q.remove(best_idx))
    }

    pub fn pick_next(&self) -> Option<usize> {
        let q = self.queue.lock().unwrap();
        if q.is_empty() {
            return None;
        }
        let mut best_idx = 0;
        for idx in 1..q.len() {
            if Self::cmp_priority(&q[idx].1, &q[best_idx].1) == CmpOrd::Less {
                best_idx = idx;
            }
        }
        Some(q[best_idx].0)
    }

    fn cmp_priority(a: &SchedulePolicy, b: &SchedulePolicy) -> CmpOrd {
        a.prio.cmp(&b.prio)
    }

    pub fn rebalance(&self) {
        let mut q = self.queue.lock().unwrap();
        q.sort_by(|a, b| Self::cmp_priority(&a.1, &b.1));
    }

    pub fn set_current(&self, id: usize) {
        *self.current.lock().unwrap() = Some(id);
    }

    pub fn clear_current(&self) {
        *self.current.lock().unwrap() = None;
    }

    pub fn len(&self) -> usize {
        self.queue.lock().unwrap().len()
    }

    pub fn remove(&self, task_id: usize) -> bool {
        let mut q = self.queue.lock().unwrap();
        let before = q.len();
        let mut i = 0;
        while i < q.len() {
            if q[i].0 == task_id {
                q.remove(i);
            } else {
                i += 1;
            }
        }
        q.len() < before
    }

    pub fn preempt_disable(&self) {
        let _prev = self.preempt_count.fetch_add(1, Ordering::Relaxed);
    }

    pub fn preempt_enable(&self) {
        let prev = self.preempt_count.load(Ordering::Relaxed);
        if prev == 0 {
            return;
        }
        self.preempt_count.fetch_sub(1, Ordering::Relaxed);
    }

    pub fn preemptible(&self) -> bool {
        self.preempt_count.load(Ordering::Relaxed) == 0
    }

    pub fn boost_priority(&self, task_id: usize, amount: i32) {
        let mut q = self.queue.lock().unwrap();
        for (id, policy) in q.iter_mut() {
            if *id == task_id {
                policy.prio = (policy.prio - amount).clamp(PRIO_MIN, PRIO_MAX);
                break;
            }
        }
    }

    pub fn yield_current(&self, policy: SchedulePolicy) -> bool {
        let cur = self.current.lock().unwrap().take();
        match cur {
            Some(id) => {
                self.enqueue(id, policy);
                true
            }
            None => false,
        }
    }
}

pub type Tid = usize;
