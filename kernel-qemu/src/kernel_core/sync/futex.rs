// AGENT
use super::{WaitOutcome, WaitToken};
use crate::kernel::kernel_core::prelude::*;

// AGENT: futex wait queues keep kernel-style wait tokens instead of host
// thread handles.
#[derive(Clone)]
pub(super) struct FutexWaiter {
    pub(super) addr: usize,
    pub(super) token: WaitToken,
}

// AGENT: keep wake and move counts separate for queue bookkeeping while both
// Linux requeue operations expose their sum as the successful syscall result.
pub(super) struct FutexRequeueResult {
    pub(super) woken: usize,
    pub(super) moved: usize,
}

impl FutexRequeueResult {
    fn affected(&self) -> usize {
        self.woken + self.moved
    }
}

// AGENT TODO: this process-owned bucket currently keys waiters only by their
// user virtual address, so it supports futexes shared by threads in one process.
// Add a shared futex key derived from the backing shared-memory object and
// offset, plus a cross-process wait registry, before supporting shared futexes.
pub struct FutexBucket {
    waiters: Mutex<VecDeque<FutexWaiter>>,
}

impl FutexBucket {
    // AGENT: keep construction at the start of the public FutexBucket API.
    pub fn new() -> Self {
        Self {
            waiters: Mutex::new(VecDeque::new()),
        }
    }

    // AGENT: compare and enqueue under one queue lock so a wake cannot slip
    // between seeing the expected value and publishing this waiter.
    pub fn wait<R>(
        &self,
        task_id: usize,
        addr: usize,
        expected: u32,
        deadline: Option<usize>,
        read_word: R,
    ) -> Result<(), &'static str>
    where
        R: FnOnce() -> Result<u32, &'static str>,
    {
        let token = WaitToken::for_task(task_id);
        {
            let mut w = self.waiters.lock().unwrap();
            if read_word()? != expected {
                return Err("changed");
            }
            w.push_back(FutexWaiter {
                addr,
                token: token.clone(),
            });
        }

        let outcome = token.wait_interruptible(deadline);
        self.finish_wait(&token, outcome)
    }

    // AGENT: place the basic address wake operation first in the wake API group.
    pub fn wake(&self, addr: usize, count: usize) -> usize {
        let mut w = self.waiters.lock().unwrap();
        Self::wake_locked(&mut w, addr, count)
    }

    // AGENT: process exit detaches the complete queue under its lock, then wakes
    // waiters unlocked and releases the old VecDeque allocation with the entries.
    pub fn wake_all(&self) -> usize {
        let waiters = {
            let mut waiters = self.waiters.lock().unwrap();
            mem::take(&mut *waiters)
        };
        let count = waiters.len();
        for waiter in waiters {
            waiter.token.wake();
        }
        count
    }

    // AGENT: keep the compound wake operation with the other public wake APIs.
    pub fn wake_op(
        &self,
        addr: usize,
        count: usize,
        addr2: usize,
        count2: usize,
        op: impl FnOnce() -> Result<u32, &'static str>,
        cmp: impl FnOnce(u32) -> Result<bool, &'static str>,
    ) -> Result<usize, &'static str> {
        let mut w = self.waiters.lock().unwrap();
        let old = op()?;
        let should_wake_addr2 = cmp(old)?;
        let mut woken = Self::wake_locked(&mut w, addr, count);
        if should_wake_addr2 {
            woken += Self::wake_locked(&mut w, addr2, count2);
        }
        Ok(woken)
    }

    // AGENT: place the unconditional requeue operation before its compare variant.
    pub fn requeue(&self, src: usize, dst: usize, wake_n: usize, move_n: usize) -> usize {
        let mut w = self.waiters.lock().unwrap();
        Self::requeue_locked(&mut w, src, dst, wake_n, move_n).affected()
    }

    // AGENT: compare the source futex word through a caller-supplied reader
    // while holding the futex queue lock, matching wait's ordering.
    pub fn cmp_requeue<R>(
        &self,
        src: usize,
        dst: usize,
        wake_n: usize,
        move_n: usize,
        expected: u32,
        read_word: R,
    ) -> Result<usize, &'static str>
    where
        R: FnOnce() -> Result<u32, &'static str>,
    {
        let mut w = self.waiters.lock().unwrap();
        if read_word()? != expected {
            return Err("changed");
        }
        Ok(Self::requeue_locked(&mut w, src, dst, wake_n, move_n).affected())
    }

    // AGENT: keep the queue-inspection helper after the mutating public APIs.
    pub fn pending_at(&self, addr: usize) -> usize {
        self.waiters
            .lock()
            .unwrap()
            .iter()
            .filter(|waiter| waiter.addr == addr)
            .count()
    }

    // AGENT: let feature-gated regressions publish controlled waiters without
    // exposing queue mutation through the production FutexBucket API.
    #[cfg(any(test, feature = "qemu-sync-selftest"))]
    pub(super) fn publish_waiter_for_test(&self, addr: usize, token: WaitToken) {
        self.waiters
            .lock()
            .unwrap()
            .push_back(FutexWaiter { addr, token });
    }

    // AGENT: collect wait completion cleanup with the other private helpers.
    fn finish_wait(&self, token: &WaitToken, outcome: WaitOutcome) -> Result<(), &'static str> {
        let mut w = self.waiters.lock().unwrap();
        w.retain(|waiter| !waiter.token.same(token));
        drop(w);

        match outcome {
            WaitOutcome::Event => Ok(()),
            WaitOutcome::Timeout => Err("timeout"),
            WaitOutcome::Signal => Err("eintr"),
            WaitOutcome::GroupExit => Err("group_exit"),
        }
    }

    // AGENT: keep lock-assuming wake mechanics outside the public API group.
    fn wake_locked(waiters: &mut VecDeque<FutexWaiter>, addr: usize, count: usize) -> usize {
        let mut woken = 0;
        waiters.retain(|waiter| {
            if waiter.addr == addr && woken < count {
                if waiter.token.wake() {
                    woken += 1;
                }
                false
            } else {
                true
            }
        });
        woken
    }

    // AGENT: keep lock-assuming requeue mechanics beside wake_locked.
    pub(super) fn requeue_locked(
        waiters: &mut VecDeque<FutexWaiter>,
        src: usize,
        dst: usize,
        wake_n: usize,
        move_n: usize,
    ) -> FutexRequeueResult {
        // AGENT: drop completed waiters before counting wake/move quotas so stale
        // timeout entries cannot consume a FUTEX_REQUEUE move slot.
        waiters.retain(|waiter| !waiter.token.is_woken());

        let (mut wk, mut mv) = (0, 0);
        for waiter in waiters.iter_mut() {
            if waiter.addr == src {
                if wk < wake_n {
                    if waiter.token.wake() {
                        wk += 1;
                    }
                } else if mv < move_n {
                    waiter.addr = dst;
                    mv += 1;
                }
            }
        }
        waiters.retain(|waiter| !waiter.token.is_woken());
        FutexRequeueResult {
            woken: wk,
            moved: mv,
        }
    }
}
