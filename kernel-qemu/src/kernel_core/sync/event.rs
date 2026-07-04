// AGENT
use crate::kernel::kernel_core::prelude::*;

pub struct EvFlag;
impl EvFlag {
    pub const READABLE: u32 = 1 << 0;
    pub const WRITABLE: u32 = 1 << 1;
    pub const ERROR: u32 = 1 << 2;
    pub const CLOSED: u32 = 1 << 3;
    pub const PROC_QUIT: u32 = 1 << 10;
    pub const CHILD_QUIT: u32 = 1 << 11;
    pub const RECV_SIG: u32 = 1 << 12;
    pub const SEM_RM: u32 = 1 << 20;
    pub const SEM_ACQ: u32 = 1 << 21;
}

// AGENT: use alloc::boxed::Box explicitly because kernel-qemu is no_std.
pub type EvCb = Box<dyn Fn(u32) -> bool + Send>;

// AGENT: persistent event-source subscription used by pipe readiness
// notifications feeding an EpInst.
struct EventWaitEntry {
    mask: u32,
    cb: EvCb,
}

// AGENT TODO: EvBus is still a lightweight event-bit store, not a full
// kernel-style wait/readiness mechanism. It lacks event payloads/counting,
// epoll-ready propagation, and lock-free callback dispatch.
#[derive(Default)]
pub struct EvBus {
    pub ev: u32,
    entries: BTreeMap<usize, EventWaitEntry>,
    next_sub_id: usize,
}
impl EvBus {
    pub fn make() -> Arc<Mutex<Self>> {
        Arc::new(Mutex::new(Self::default()))
    }
    pub fn set(&mut self, s: u32) {
        self.change(0, s);
    }
    pub fn clear(&mut self, s: u32) {
        self.change(s, 0);
    }
    // AGENT: event changes drive persistent subscriptions; an entry stays
    // installed until its callback asks to be removed or unsub() removes it.
    pub fn change(&mut self, rst: u32, s: u32) {
        let orig = self.ev;
        self.ev = (self.ev & !rst) | s;
        if self.ev != orig {
            let ev = self.ev;
            self.entries.retain(|_, entry| {
                if (ev & entry.mask) == 0 {
                    return true;
                }

                !(entry.cb)(ev)
            });
        }
    }
    // AGENT: return a subscription id so higher-level readiness users can
    // cancel epoll registrations when epoll_ctl removes or replaces them.
    pub fn sub(&mut self, mask: u32, cb: EvCb) -> usize {
        let id = self.next_sub_id;
        self.next_sub_id = self.next_sub_id.wrapping_add(1);
        self.entries.insert(id, EventWaitEntry { mask, cb });
        id
    }
    // AGENT: remove a previously installed callback subscription.
    pub fn unsub(&mut self, id: usize) -> bool {
        self.entries.remove(&id).is_some()
    }
    // AGENT: subscription-only EvBus keeps callback count as entry count.
    pub fn cb_len(&self) -> usize {
        self.entries.len()
    }
}
