// AGENT
use super::*;

#[repr(C)]
#[derive(Clone, Copy)]
pub struct EpData { pub ptr: u64 }

#[repr(C)]
#[derive(Clone)]
pub struct EpEvent { pub events: u32, pub data: EpData }
impl EpEvent {
    pub const IN: u32 = 0x001;
    pub const OUT: u32 = 0x004;
    pub const ERR: u32 = 0x008;
    pub const HUP: u32 = 0x010;
    pub const PRI: u32 = 0x002;
    pub const RDNORM: u32 = 0x040;
    pub const RDBAND: u32 = 0x080;
    pub const WRNORM: u32 = 0x100;
    pub const WRBAND: u32 = 0x200;
    pub const MSG: u32 = 0x400;
    pub const RDHUP: u32 = 0x2000;
    pub const EXCL: u32 = 1 << 28;
    pub const WAKEUP: u32 = 1 << 29;
    pub const ONESHOT: u32 = 1 << 30;
    pub const ET: u32 = 1 << 31;
    pub fn has(&self, ev: u32) -> bool { (self.events & ev) != 0 }
}

pub struct EpCtlOp;
impl EpCtlOp {
    pub const ADD: i32 = 1;
    pub const DEL: i32 = 2;
    pub const MOD: i32 = 3;
}

#[derive(Clone)]
pub struct EpInst {
    pub events: BTreeMap<usize, EpEvent>,
    pub ready: Arc<Mutex<BTreeSet<usize>>>,
}
impl EpInst {
    pub fn new() -> Self {
        EpInst {
            events: BTreeMap::new(),
            ready: Arc::new(Mutex::new(BTreeSet::new())),
        }
    }
    pub fn control(&mut self, op: i32, fd: usize, ev: &EpEvent) -> Result<(), &'static str> {
        match op {
            EpCtlOp::ADD => {
                if self.events.contains_key(&fd) {
                    return Err("eexist");
                }
                self.events.insert(fd, ev.clone());
                Ok(())
            }
            EpCtlOp::MOD => {
                if !self.events.contains_key(&fd) {
                    return Err("enoent");
                }
                self.events.insert(fd, ev.clone());
                Ok(())
            }
            EpCtlOp::DEL => {
                if self.events.remove(&fd).is_none() {
                    return Err("enoent");
                }
                self.ready.lock().unwrap().remove(&fd);
                Ok(())
            }
            _ => Err("einval"),
        }
    }
}
