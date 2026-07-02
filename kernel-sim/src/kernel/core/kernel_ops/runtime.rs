use super::*;

// AGENT: runtime ticker is opt-in because CLK and TIMER_WHEEL are simulator-global.
static RUNTIME_TICKER_ACTIVE: AtomicBool = AtomicBool::new(false);

// AGENT: wakeable stop state lets Drop stop the background ticker promptly.
// AGENT TODO: replace std::sync::Condvar with a project-owned runtime wait
// primitive once the host-thread ticker stop path can stay independent from
// the logical timer wheel that this ticker drives.
struct RuntimeTickerStop {
    stopped: Mutex<bool>,
    cv: Condvar,
}

// AGENT: RAII guard for an optional background CPU0 ticker.
pub struct KernelRuntimeTicker {
    stop: Arc<RuntimeTickerStop>,
    handle: Option<thread::JoinHandle<()>>,
}

impl KernelRuntimeTicker {
    // AGENT: start one 100Hz runtime ticker for an explicitly Arc-owned Kernel.
    pub fn start(kernel: Arc<Kernel>) -> Result<Self, &'static str> {
        if RUNTIME_TICKER_ACTIVE
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            return Err("kernel runtime ticker already running");
        }

        let stop = Arc::new(RuntimeTickerStop {
            stopped: Mutex::new(false),
            cv: Condvar::new(),
        });
        let thread_stop = Arc::clone(&stop);
        let interval = Duration::from_micros(USEC_TICK as u64);

        let handle = match thread::Builder::new()
            .name("kernel-sim-ticker".to_string())
            .spawn(move || loop {
                let stopped = thread_stop.stopped.lock().unwrap();
                if *stopped {
                    break;
                }
                let (stopped, _) = thread_stop.cv.wait_timeout(stopped, interval).unwrap();
                if *stopped {
                    break;
                }
                drop(stopped);
                kernel.schedule_tick(0);
            }) {
            Ok(handle) => handle,
            Err(_) => {
                RUNTIME_TICKER_ACTIVE.store(false, Ordering::Release);
                return Err("failed to start kernel runtime ticker");
            }
        };

        Ok(Self {
            stop,
            handle: Some(handle),
        })
    }

    // AGENT: explicit stop mirrors Drop cleanup and releases the singleton slot.
    pub fn stop(&mut self) {
        if let Some(handle) = self.handle.take() {
            *self.stop.stopped.lock().unwrap() = true;
            self.stop.cv.notify_all();
            let _ = handle.join();
            RUNTIME_TICKER_ACTIVE.store(false, Ordering::Release);
        }
    }
}

impl Drop for KernelRuntimeTicker {
    // AGENT: dropping the guard stops the ticker before the Kernel Arc can be released.
    fn drop(&mut self) {
        self.stop();
    }
}

impl Kernel {
    // AGENT: keep simulator tick/GKL/cache maintenance out of the Kernel state
    // definition and use guard-based GKL release.
    pub fn tick(&self, id: usize) {
        // AGENT: route GKL through the guard so Drop performs owner-checked release.
        let _gkl = GKL.guard(id);
        let _ir = {
            let cg = self.cpus.lock().unwrap();
            let mut occ = 0u32;
            for (i, sl) in cg.iter().enumerate() {
                if sl.is_some() {
                    occ |= 1 << i;
                }
            }
            let busy = occ.count_ones() as usize;
            let total = MAX_CPU;
            if total > 0 {
                ((total - busy) * 100) / total
            } else {
                100
            }
        };
        // AGENT: dirty block-cache entries are now written through
        // BlockCache::flush_dirty() with an explicit block device; a timer tick
        // must not silently clear writeback state.
    }

    // AGENT: expose the per-CPU current-task slot used by scheduling and syscalls.
    pub fn cur_task(&self, cpu: usize) -> Option<Arc<Task>> {
        let cg = self.cpus.lock().unwrap();
        if cpu >= cg.len() {
            return None;
        }
        match &cg[cpu] {
            Some(t) => {
                let cloned = t.clone();
                let _id = cloned.id();
                Some(cloned)
            }
            None => None,
        }
    }

    // AGENT: update the per-CPU current-task slot without keeping the old task alive.
    pub fn set_cur(&self, cpu: usize, t: Option<Arc<Task>>) {
        let mut cg = self.cpus.lock().unwrap();
        if cpu < cg.len() {
            if cpu == 0 {
                set_current_task_id(t.as_ref().map(|task| task.id()));
            }
            let _prev = cg[cpu].take();
            cg[cpu] = t;
        }
    }
}
