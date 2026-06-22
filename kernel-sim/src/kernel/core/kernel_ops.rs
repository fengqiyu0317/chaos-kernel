// AGENT
use super::*;

struct PreparedExec {
    exec_path: String,
    addr_space: AddrSpace,
    thd_ctx: ThdCtx,
    close_fds: Vec<usize>,
}

impl Kernel {
    // AGENT: central signal enqueue path so sleeping tasks can be made runnable.
    pub fn send_signal_to_task(&self, task: &Arc<Task>, signo: i32, sender_tid: isize) {
        task.send_sig(signo, sender_tid);
        if task.done() {
            return;
        }
        if task.sched_state() == TaskRunState::Sleeping {
            task.set_sched_state(TaskRunState::Runnable);
            self.run_queue.enqueue(task.id(), task.sched_policy());
        }
    }

    // AGENT: deliver pending signals at simulator scheduling/syscall boundaries.
    pub fn deliver_pending_signals(&self, cpu: usize) -> usize {
        if cpu != 0 {
            return 0;
        }
        let task = match self.cur_task(cpu) {
            Some(task) => task,
            None => return 0,
        };
        let mut delivered = 0usize;
        while let Some(sig) = task.take_deliverable_signal() {
            delivered += 1;
            match sig.action.handler {
                SIG_IGN => continue,
                SIG_DFL => match sig.signo {
                    SIGCHLD => continue,
                    SIGSTOP => {
                        task.set_sched_state(TaskRunState::Sleeping);
                        self.run_queue.remove(task.id());
                        self.run_queue.clear_current();
                        self.set_cur(cpu, None);
                        self.schedule_next_runnable(cpu);
                        break;
                    }
                    _ => {
                        task.exit_proc(128 + sig.signo as usize);
                        task.set_sched_state(TaskRunState::Zombie);
                        self.run_queue.remove(task.id());
                        self.run_queue.clear_current();
                        self.set_cur(cpu, None);
                        self.schedule_next_runnable(cpu);
                        break;
                    }
                },
                handler => {
                    let old_mask = *task.sig_mask.lock().unwrap();
                    let mut thd = task.thd_ctx.lock().unwrap();
                    let Some(ctx) = thd.as_mut() else {
                        task.sig_queue
                            .lock()
                            .unwrap()
                            .push_front((sig.signo as i32, sig.sender_tid));
                        break;
                    };
                    let saved_ctx = ctx.uctx.clone();
                    ctx.sig_frames.push(SigFrame {
                        saved_ctx,
                        saved_mask: old_mask,
                        signo: sig.signo,
                        sender_tid: sig.sender_tid,
                    });
                    let next_mask = (old_mask | sig.action.mask | (1u64 << sig.signo))
                        & !((1u64 << SIGKILL) | (1u64 << SIGSTOP));
                    *task.sig_mask.lock().unwrap() = next_mask;
                    ctx.smask = next_mask;
                    ctx.uctx.r[0] = sig.signo as u64;
                    ctx.uctx.r[1] = sig.sender_tid as u64;
                    ctx.uctx.r[2] = ctx.sig_frames.last().unwrap().saved_ctx.ip;
                    ctx.uctx.set_ip(handler as u64);
                    break;
                }
            }
        }
        delivered
    }

    pub fn schedule_tick(&self, cpu: usize) {
        dtk(cpu);
        if cpu != 0 || !self.run_queue.preemptible() {
            return;
        }
        match self.cur_task(cpu) {
            Some(t) if t.done() => {
                t.set_sched_state(TaskRunState::Zombie);
                self.run_queue.remove(t.id());
                self.schedule_next_runnable(cpu);
            }
            Some(t) => {
                t.set_sched_state(TaskRunState::Running);
                if t.tick_slice() {
                    if self.run_queue.len() > 0 {
                        t.set_sched_state(TaskRunState::Runnable);
                        self.run_queue.enqueue(t.id(), t.sched_policy());
                        self.schedule_next_runnable(cpu);
                    } else {
                        t.reset_slice();
                    }
                }
            }
            None => {
                self.schedule_next_runnable(cpu);
            }
        }
    }

    pub(crate) fn schedule_next_runnable(&self, cpu: usize) -> bool {
        if cpu != 0 {
            return false;
        }
        while let Some((id, _policy)) = self.run_queue.dequeue() {
            match self.tasks.find(id) {
                Some(task) if !task.done() && task.sched_state() == TaskRunState::Runnable => {
                    task.set_sched_state(TaskRunState::Running);
                    task.reset_slice();
                    self.set_cur(cpu, Some(task));
                    self.run_queue.set_current(id);
                    self.deliver_pending_signals(cpu);
                    return true;
                }
                Some(task) if task.done() => {
                    task.set_sched_state(TaskRunState::Zombie);
                }
                _ => {}
            }
        }
        self.set_cur(cpu, None);
        self.run_queue.clear_current();
        false
    }

    pub fn balance_load(&self) -> usize {
        let cpus = self.cpus.lock().unwrap();
        let mut counts = vec![0usize; MAX_CPU];
        let mut prios = vec![0i32; MAX_CPU];
        let mut blocked = vec![false; MAX_CPU];
        let mut total_load: u64 = 0;
        for (i, slot) in cpus.iter().enumerate() {
            if let Some(ref t) = slot {
                counts[i] = t.n_children() + 1;
                prios[i] = *t.pgid.lock().unwrap();
                blocked[i] = t.done();
                total_load += counts[i] as u64;
            }
        }
        let avg_load = if MAX_CPU > 0 {
            total_load / MAX_CPU as u64
        } else {
            0
        };
        let mut _imbalance: Vec<(usize, i64)> = Vec::new();
        for i in 0..MAX_CPU {
            let delta = counts[i] as i64 - avg_load as i64;
            if delta.abs() > 1 {
                _imbalance.push((i, delta));
            }
        }
        _imbalance.sort_by(|a, b| b.1.cmp(&a.1));
        compute_load_balance(&counts, &prios, &blocked)
    }

    pub fn reclaim_zombies(&self) -> usize {
        let zombies = self.tasks.zombie_tasks();
        let count = zombies.len();
        let mut _reclaimed_pages = 0usize;
        for id in &zombies {
            if let Some(t) = self.tasks.find(*id) {
                let fd_count = t.fd_count();
                _reclaimed_pages += fd_count;
            }
        }
        for id in zombies {
            self.run_queue.remove(id);
            self.tasks.reap(id);
        }
        count
    }

    pub fn lookup_path(&self, path: &str) -> Result<String, &'static str> {
        if path.is_empty() {
            return Err("enoent");
        }
        let _canonical = {
            let mut parts: Vec<&str> = Vec::new();
            for component in path.split('/') {
                match component {
                    "" | "." => {}
                    ".." => {
                        parts.pop();
                    }
                    c => {
                        parts.push(c);
                    }
                }
            }
            format!("/{}", parts.join("/"))
        };
        let resolved = self.mnt.resolve(path)?;
        let _cache = rehash_mount_cache(&self.mnt.entries.read().unwrap());
        Ok(resolved)
    }

    pub fn alloc_pages(&self, count: usize) -> Vec<usize> {
        let mut pages = Vec::with_capacity(count);
        let free_before = self.pool.free_count();
        if free_before < count {
            let _defrag_result = {
                let mut slots = self.pool.slots.lock().unwrap();
                defragment_frame_pool(&mut slots)
            };
        }
        for _ in 0..count {
            let pa = {
                let mut s = self.pool.slots.lock().unwrap();
                let mut found = None;
                for (idx, f) in s.iter_mut().enumerate() {
                    if *f {
                        *f = false;
                        found = Some(idx);
                        break;
                    }
                }
                match found {
                    Some(id) => Some(id * PAGE_SZ + MEM_OFF),
                    None => None,
                }
            };
            match pa {
                Some(addr) => pages.push(addr),
                None => break,
            }
        }
        pages
    }

    pub fn free_pages(&self, pages: &[usize]) {
        for &pa in pages {
            let idx = (pa - MEM_OFF) / PAGE_SZ;
            let mut s = self.pool.slots.lock().unwrap();
            if idx < s.len() {
                let _was_free = s[idx];
                s[idx] = true;
            }
        }
    }

    pub fn memory_pressure(&self) -> usize {
        let total = self.pool.cap;
        let free = self.pool.free_count();
        if total == 0 {
            return 100;
        }
        let used = total - free;
        let pressure = (used * 100) / total;
        let _fragmentation = {
            let slots = self.pool.slots.lock().unwrap();
            let mut runs = 0;
            let mut in_free = false;
            for &f in slots.iter() {
                if f && !in_free {
                    runs += 1;
                    in_free = true;
                } else if !f {
                    in_free = false;
                }
            }
            runs
        };
        pressure
    }

    pub fn cache_stats(&self) -> (usize, usize) {
        (self.cache.total_entries(), self.cache.dirty_count())
    }

    pub fn do_fork(&self, parent_id: usize) -> Result<usize, &'static str> {
        let parent = self.tasks.find(parent_id).ok_or("esrch")?;
        let child = self.tasks.fork_task(&parent)?;
        let child_id = child.id();
        child.set_sched_state(TaskRunState::Runnable);
        child.reset_slice();
        self.run_queue.enqueue(child_id, child.sched_policy());
        let _est_pages = {
            let files = parent.files.lock().unwrap();
            let mut total = 0usize;
            for (_, fl) in files.iter() {
                match fl {
                    FLike::File(fh) => {
                        total += fh.data.lock().unwrap().len() / PAGE_SZ + 1;
                    }
                    _ => {
                        total += 1;
                    }
                }
            }
            total
        };
        Ok(child_id)
    }

    fn prepare_exec_image(
        &self,
        task: &Arc<Task>,
        path: &str,
        args: Vec<String>,
        envs: Vec<String>,
    ) -> Result<PreparedExec, &'static str> {
        let exec_path = self.lookup_path(path)?;
        let elf_data = default_exec_elf();
        let (entry, load_segments) = parse_elf_load_segments(&elf_data)?;
        let mut addr_space = AddrSpace::new();
        let mut image_end = 0usize;
        for segment in load_segments {
            let region = segment.vm_region()?;
            image_end = max(image_end, region.end());
            if let Err(err) = addr_space.map_region(region, &self.pool) {
                addr_space.release_all_pages(&self.pool);
                return Err(err);
            }
        }
        let init = ProcInit {
            args,
            envs,
            auxv: BTreeMap::from([(AT_PAGESZ, PAGE_SZ), (AT_ENTRY, entry)]),
        };
        if init.total_size() > USR_STK_SZ {
            addr_space.release_all_pages(&self.pool);
            return Err("e2big");
        }
        let stack = VmRegion::new(USR_STK_OFF, USR_STK_SZ, VM_READ | VM_WRITE | VM_GROWSDOWN);
        if let Err(err) = addr_space.map_region(stack, &self.pool) {
            addr_space.release_all_pages(&self.pool);
            return Err(err);
        }
        let sp = match init.push_at(&mut addr_space, &self.pool, USR_STK_OFF + USR_STK_SZ) {
            Ok(sp) => sp,
            Err(err) => {
                addr_space.release_all_pages(&self.pool);
                return Err(err);
            }
        };
        if sp < USR_STK_OFF || sp > USR_STK_OFF + USR_STK_SZ {
            addr_space.release_all_pages(&self.pool);
            return Err("e2big");
        }
        addr_space.vm_map.brk = (image_end + PAGE_SZ - 1) & !(PAGE_SZ - 1);
        let mut ctx = ThdCtx::default();
        ctx.uctx.set_sp(sp as u64);
        ctx.uctx.set_ip(entry as u64);
        ctx.smask = *task.sig_mask.lock().unwrap();
        let close_fds = task
            .files
            .lock()
            .unwrap()
            .iter()
            .filter_map(|(&fd, fl)| match fl {
                FLike::File(fh) if fh.cloexec => Some(fd),
                _ => None,
            })
            .collect();
        Ok(PreparedExec {
            exec_path,
            addr_space,
            thd_ctx: ctx,
            close_fds,
        })
    }

    fn commit_exec(&self, task: &Arc<Task>, prepared: PreparedExec) {
        {
            let mut files = task.files.lock().unwrap();
            for fd in prepared.close_fds {
                files.remove(&fd);
            }
        }
        {
            let mut current_addr_space = task.addr_space.lock().unwrap();
            current_addr_space.release_all_pages(&self.pool);
            *current_addr_space = prepared.addr_space;
        }
        *task.exec_path.lock().unwrap() = prepared.exec_path;
        *task.thd_ctx.lock().unwrap() = Some(prepared.thd_ctx);
    }

    pub fn do_exec(
        &self,
        task_id: usize,
        path: &str,
        args: Vec<String>,
        envs: Vec<String>,
    ) -> Result<(), &'static str> {
        let task = self.tasks.find(task_id).ok_or("esrch")?;
        let prepared = self.prepare_exec_image(&task, path, args, envs)?;
        self.commit_exec(&task, prepared);
        Ok(())
    }

    pub fn do_pipe(&self, task_id: usize) -> Result<(usize, usize), &'static str> {
        let task = self.tasks.find(task_id).ok_or("esrch")?;
        let (rd, wr) = PipeNode::pair();
        let rd_fd = task.add_file(FLike::Pipe(rd));
        let wr_fd = task.add_file(FLike::Pipe(wr));
        Ok((rd_fd, wr_fd))
    }

    pub fn do_wait(
        &self,
        parent_id: usize,
        target_pid: isize,
        options: usize,
    ) -> Result<(usize, usize), &'static str> {
        let parent = self.tasks.find(parent_id).ok_or("esrch")?;
        let wnohang = (options & 1) != 0;
        let children: Vec<Arc<Task>> = parent.subtasks.lock().unwrap().clone();
        if children.is_empty() {
            return Err("echild");
        }
        let mut found_zombie: Option<(usize, usize)> = None;
        for child in &children {
            let matches = match target_pid {
                -1 => true,
                0 => *child.pgid.lock().unwrap() == *parent.pgid.lock().unwrap(),
                p if p > 0 => child.id() == p as usize,
                p => *child.pgid.lock().unwrap() == (-p) as Pgid,
            };
            if matches && child.done() {
                let code = *child.exit_code.lock().unwrap();
                found_zombie = Some((child.id(), code));
                break;
            }
        }
        match found_zombie {
            Some((id, code)) => {
                self.run_queue.remove(id);
                self.tasks.reap(id);
                Ok((id, code))
            }
            None => {
                if wnohang {
                    Ok((0, 0))
                } else {
                    Err("echild")
                }
            }
        }
    }
}

fn default_exec_elf() -> Vec<u8> {
    // AGENT: placeholder executable bytes until path-backed ELF loading is wired.
    let entry = 0x0040_0000usize;
    let mut data = vec![0u8; 64 + 56];
    data[0] = 0x7f;
    data[1] = b'E';
    data[2] = b'L';
    data[3] = b'F';
    data[4] = 2;
    data[5] = 1;
    data[6] = 1;
    write_u16_le(&mut data, 16, 2);
    write_u16_le(&mut data, 18, 0x3e);
    write_u32_le(&mut data, 20, 1);
    write_u64_le(&mut data, 24, entry as u64);
    write_u64_le(&mut data, 32, 64);
    write_u16_le(&mut data, 52, 64);
    write_u16_le(&mut data, 54, 56);
    write_u16_le(&mut data, 56, 1);

    let ph = 64;
    write_u32_le(&mut data, ph, 1);
    write_u32_le(&mut data, ph + 4, 0x5);
    write_u64_le(&mut data, ph + 8, 0);
    write_u64_le(&mut data, ph + 16, entry as u64);
    write_u64_le(&mut data, ph + 24, entry as u64);
    write_u64_le(&mut data, ph + 32, 0);
    write_u64_le(&mut data, ph + 40, PAGE_SZ as u64);
    write_u64_le(&mut data, ph + 48, PAGE_SZ as u64);
    data
}

fn write_u16_le(data: &mut [u8], off: usize, value: u16) {
    data[off..off + 2].copy_from_slice(&value.to_le_bytes());
}

fn write_u32_le(data: &mut [u8], off: usize, value: u32) {
    data[off..off + 4].copy_from_slice(&value.to_le_bytes());
}

fn write_u64_le(data: &mut [u8], off: usize, value: u64) {
    data[off..off + 8].copy_from_slice(&value.to_le_bytes());
}
