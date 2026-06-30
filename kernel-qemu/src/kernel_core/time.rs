// AGENT
use super::*;
use crate::irq_lock::{IrqOnceCell, Mutex};

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

    pub fn active_count(&self) -> usize {
        self.slots
            .iter()
            .flat_map(|s| s.iter())
            .filter(|e| e.active)
            .count()
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
