// AGENT
use super::*;
use crate::irq_lock::{IrqOnceCell, Mutex};

const CHECKPOINT_TIMER_CLOCK_LOGICAL: u32 = 0;

// AGENT: QEMU timer wheel storage; TimerWheel owns Vec slots, so it is
// explicitly initialized after heap setup and before timer interrupts are
// enabled.
pub static TIMER_WHEEL: IrqOnceCell<Mutex<TimerWheel>> = IrqOnceCell::new();

// AGENT: initialize the QEMU logical timer wheel once heap allocation is ready.
pub fn init_timer_wheel() {
    if TIMER_WHEEL.init(Mutex::new(TimerWheel::new())).is_err() {
        panic!("QEMU timer wheel initialized more than once");
    }
}

// AGENT: single access point for the QEMU logical timer wheel.
pub fn global_timer_wheel() -> &'static Mutex<TimerWheel> {
    TIMER_WHEEL
        .get()
        .expect("QEMU timer wheel must be initialized before use")
}

// AGENT: typed timer targets let expiry dispatch route through real kernel-sim
// wakeup paths instead of interpreting an untyped numeric callback id.
#[derive(Clone)]
pub enum TimerTarget {
    Noop,
    WakeToken {
        token: WaitToken,
    },
    WakeTask {
        task_id: usize,
    },
    SignalTask {
        task_id: usize,
        signo: i32,
        sender_tid: isize,
    },
}

// AGENT: timer entries keep a numeric id only for cancellation; behavior lives
// in TimerTarget.
#[derive(Clone)]
pub struct TimerEntry {
    pub id: usize,
    pub deadline: usize,
    pub interval: usize,
    pub target: TimerTarget,
    pub active: bool,
    pub repeat: bool,
}

impl TimerEntry {
    pub fn new(deadline: usize, interval: usize, id: usize) -> Self {
        Self::with_target(id, deadline, interval, TimerTarget::Noop)
    }

    pub fn with_target(id: usize, deadline: usize, interval: usize, target: TimerTarget) -> Self {
        Self {
            id,
            deadline,
            interval,
            target,
            active: true,
            repeat: interval > 0,
        }
    }

    // AGENT: a timer expires on the tick that reaches its deadline.
    pub fn expired(&self) -> bool {
        CLK.load(Ordering::Relaxed) >= self.deadline
    }

    pub fn reset(&mut self) {
        if self.repeat {
            self.deadline = CLK.load(Ordering::Relaxed) + self.interval;
        } else {
            self.active = false;
        }
    }

    pub fn remaining(&self) -> usize {
        let now = CLK.load(Ordering::Relaxed);
        if now >= self.deadline {
            0
        } else {
            self.deadline - now
        }
    }

    pub fn cancel(&mut self) {
        self.active = false;
    }

    // AGENT: convert task-bound active timers into checkpoint records while
    // rejecting wait-token timers that belong to an in-flight blocking wait.
    fn snapshot_for_checkpoint_task(
        &self,
        task_id: usize,
    ) -> Result<Option<SavedTimer>, &'static str> {
        if !self.active {
            return Ok(None);
        }

        let (target_kind, signo, sender_tid) = match &self.target {
            TimerTarget::Noop => return Ok(None),
            TimerTarget::WakeToken { token } if token.task_id() == task_id => {
                return Err("enotsup")
            }
            TimerTarget::WakeToken { .. } => return Ok(None),
            TimerTarget::WakeTask { task_id: target } if *target == task_id => {
                (SavedTimerTargetKind::WakeTask, 0, 0)
            }
            TimerTarget::WakeTask { .. } => return Ok(None),
            TimerTarget::SignalTask {
                task_id: target,
                signo,
                sender_tid,
            } if *target == task_id => (
                SavedTimerTargetKind::SignalTask,
                *signo,
                i64::try_from(*sender_tid).map_err(|_| "einval")?,
            ),
            TimerTarget::SignalTask { .. } => return Ok(None),
        };

        Ok(Some(SavedTimer {
            clock_id: CHECKPOINT_TIMER_CLOCK_LOGICAL,
            target_kind,
            signo,
            sender_tid,
            deadline_ticks: u64::try_from(self.deadline).map_err(|_| "einval")?,
            interval_ticks: u64::try_from(self.interval).map_err(|_| "einval")?,
        }))
    }
}

// AGENT: timer wheel advanced from the CPU0 schedule_tick path.
pub struct TimerWheel {
    pub slots: Vec<Vec<TimerEntry>>,
    pub current_slot: usize,
    next_id: usize,
}

impl TimerWheel {
    pub fn new() -> Self {
        let mut slots = Vec::with_capacity(TIMER_WHEEL_SIZE);
        for _ in 0..TIMER_WHEEL_SIZE {
            slots.push(Vec::new());
        }
        Self {
            slots,
            current_slot: CLK.load(Ordering::Relaxed) % TIMER_WHEEL_SIZE,
            next_id: 1,
        }
    }

    // AGENT: allocate a cancelable timer id and bind it to a typed expiry target.
    pub fn register_timer(
        &mut self,
        deadline: usize,
        interval: usize,
        target: TimerTarget,
    ) -> usize {
        let id = self.next_id;
        self.next_id = self.next_id.saturating_add(1).max(1);
        self.add_timer(TimerEntry::with_target(id, deadline, interval, target));
        id
    }

    pub fn add_timer(&mut self, entry: TimerEntry) {
        self.next_id = self.next_id.max(entry.id.saturating_add(1));
        // AGENT: far-future deadlines may land in a slot before they expire; the
        // advance path keeps them in that slot until a later wheel pass reaches
        // or passes the full absolute deadline.
        let slot = entry.deadline % TIMER_WHEEL_SIZE;
        self.slots[slot].push(entry);
    }

    pub fn advance(&mut self) -> Vec<TimerEntry> {
        self.current_slot = (self.current_slot + 1) % TIMER_WHEEL_SIZE;
        let mut fired = Vec::new();
        let slot = &mut self.slots[self.current_slot];
        let mut remaining = Vec::new();
        for entry in slot.drain(..) {
            if entry.active && entry.expired() {
                fired.push(entry);
            } else if entry.active {
                remaining.push(entry);
            }
        }
        *slot = remaining;
        for t in fired.iter_mut() {
            if t.repeat {
                t.reset();
                let new_slot = t.deadline % TIMER_WHEEL_SIZE;
                let clone = TimerEntry::with_target(t.id, t.deadline, t.interval, t.target.clone());
                self.slots[new_slot].push(clone);
            }
        }
        fired
    }

    pub fn cancel(&mut self, id: usize) -> bool {
        for slot in self.slots.iter_mut() {
            for entry in slot.iter_mut() {
                if entry.id == id && entry.active {
                    entry.active = false;
                    return true;
                }
            }
        }
        false
    }

    // AGENT: snapshot only timers whose observable target belongs to the saved
    // single task; unrelated global timers remain owned by their live tasks.
    pub fn snapshot_checkpoint_timers(
        &self,
        task_id: usize,
    ) -> Result<Vec<SavedTimer>, &'static str> {
        let mut saved = Vec::new();
        for entry in self.slots.iter().flat_map(|slot| slot.iter()) {
            if let Some(timer) = entry.snapshot_for_checkpoint_task(task_id)? {
                saved.push(timer);
            }
        }
        Ok(saved)
    }

    // AGENT: restore saved task-bound timers by allocating fresh wheel ids and
    // rebinding every target to the newly restored task id.
    pub fn restore_checkpoint_timers(
        &mut self,
        timers: &[SavedTimer],
        restored_task_id: usize,
    ) -> Result<(), &'static str> {
        for timer in timers {
            if timer.clock_id != CHECKPOINT_TIMER_CLOCK_LOGICAL {
                return Err("enotsup");
            }
            let deadline = usize::try_from(timer.deadline_ticks).map_err(|_| "einval")?;
            let interval = usize::try_from(timer.interval_ticks).map_err(|_| "einval")?;
            let target = restored_timer_target(timer, restored_task_id)?;
            self.register_timer(deadline, interval, target);
        }
        Ok(())
    }

    pub fn active_count(&self) -> usize {
        self.slots
            .iter()
            .flat_map(|s| s.iter())
            .filter(|e| e.active)
            .count()
    }
}

// AGENT: rebuild a timer target from image metadata while intentionally replacing
// the saved task id with the fresh restored pid.
fn restored_timer_target(
    timer: &SavedTimer,
    restored_task_id: usize,
) -> Result<TimerTarget, &'static str> {
    match timer.target_kind {
        SavedTimerTargetKind::WakeTask => Ok(TimerTarget::WakeTask {
            task_id: restored_task_id,
        }),
        SavedTimerTargetKind::SignalTask => Ok(TimerTarget::SignalTask {
            task_id: restored_task_id,
            signo: timer.signo,
            sender_tid: isize::try_from(timer.sender_tid).map_err(|_| "einval")?,
        }),
    }
}

// AGENT: convert host Duration values into simulator clock ticks, rounding up
// so any nonzero timeout gets at least one logical tick.
pub fn duration_to_ticks(timeout: Duration) -> usize {
    if timeout.is_zero() {
        return 0;
    }
    let tick_nanos = 1_000_000_000u128 / TIMER_TICK_HZ as u128;
    let nanos = timeout.as_nanos();
    let ticks = (nanos + tick_nanos - 1) / tick_nanos;
    usize::try_from(ticks).unwrap_or(usize::MAX).max(1)
}
