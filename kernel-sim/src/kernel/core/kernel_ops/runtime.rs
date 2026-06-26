use super::*;

impl Kernel {
    // AGENT: keep simulator tick/GKL/cache maintenance out of the Kernel state definition.
    pub fn tick(&self, id: usize) {
        assert!(
            id <= MAX_THREAD_ID,
            "thread id {} exceeds MAX_THREAD_ID {}",
            id,
            MAX_THREAD_ID
        );
        // AGENT: sentinel is MAX_THREAD_ID+1, no need for id != 0 guard
        if GKL.holder.load(Ordering::Relaxed) == id {
            GKL.depth.fetch_add(1, Ordering::Relaxed);
        } else {
            while GKL
                .flag
                .compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed)
                .is_err()
            {
                ::core::hint::spin_loop();
            }
            GKL.holder.store(id, Ordering::Relaxed);
            GKL.depth.store(1, Ordering::Relaxed);
        }
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
        {
            for ci in 0..self.cache.chains.len() {
                let ch = &self.cache.chains[ci];
                while ch
                    .lk
                    .v
                    .compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed)
                    .is_err()
                {
                    ::core::hint::spin_loop();
                }
                {
                    let mut items = ch.items.lock().unwrap();
                    for s in items.iter_mut() {
                        s.modified = false;
                    }
                }
                ch.lk.v.store(false, Ordering::Release);
            }
        }
        GKL.leave(); // AGENT
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
            let _prev = cg[cpu].take();
            cg[cpu] = t;
        }
    }
}
