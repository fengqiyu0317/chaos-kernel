use super::*;

struct PreparedExec {
    exec_path: String,
    addr_space: AddrSpace,
    thd_ctx: ThdCtx,
    close_fds: Vec<usize>,
}

impl RuntimeKernel {
    // AGENT: read a stable executable snapshot from the unified path file table.
    fn read_file_for_exec(&self, path: &str) -> Result<Vec<u8>, &'static str> {
        let node = self
            .file_nodes
            .read()
            .unwrap()
            .get(path)
            .cloned()
            .ok_or("enoent")?;
        if node.kind != FileKind::Regular {
            return Err("eisdir");
        }
        if !node.executable.load(Ordering::Relaxed) {
            return Err("eacces");
        }
        let snapshot = node.data.lock().unwrap().clone();
        Ok(snapshot)
    }

    // AGENT: prepare exec from a path-backed executable file snapshot.
    fn prepare_exec_image(
        &self,
        task: &Arc<RuntimeTask>,
        path: &str,
        args: Vec<String>,
        envs: Vec<String>,
    ) -> Result<PreparedExec, &'static str> {
        let exec_path = self.lookup_path(path)?;
        let elf_data = self.read_file_for_exec(&exec_path)?;
        let (entry, load_segments) = parse_elf_load_segments(&elf_data)?;
        let mut addr_space = AddrSpace::new();
        let mut image_end = 0usize;
        for segment in load_segments {
            let region = segment.vm_region()?;
            let region_base = region.base;
            let region_len = region.len;
            let region_flags = region.flags;
            let region_end = region.end();
            let load_region = VmRegion {
                flags: region_flags | VM_WRITE,
                ..region
            };
            image_end = max(image_end, region_end);
            if let Err(err) = addr_space.map_region(load_region, &self.pool) {
                addr_space.release_all_pages(&self.pool);
                return Err(err);
            }
            let file_end = match segment.offset.checked_add(segment.file_size) {
                Some(end) => end,
                None => {
                    addr_space.release_all_pages(&self.pool);
                    return Err("ph_overflow");
                }
            };
            if file_end > elf_data.len() {
                addr_space.release_all_pages(&self.pool);
                return Err("ph_overflow");
            }
            if let Err(err) = addr_space.write_user_bytes(
                segment.vaddr,
                &elf_data[segment.offset..file_end],
                &self.pool,
            ) {
                addr_space.release_all_pages(&self.pool);
                return Err(err);
            }
            if let Err(err) = addr_space.protect(region_base, region_len, region_flags) {
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
            .process
            .files
            .lock()
            .unwrap()
            .iter()
            .filter_map(|(&fd, entry)| entry.is_cloexec().then_some(fd))
            .collect();
        Ok(PreparedExec {
            exec_path,
            addr_space,
            thd_ctx: ctx,
            close_fds,
        })
    }

    fn commit_exec(&self, task: &Arc<RuntimeTask>, prepared: PreparedExec) {
        {
            let mut files = task.process.files.lock().unwrap();
            for fd in prepared.close_fds {
                files.remove(&fd);
            }
        }
        {
            let mut current_addr_space = task.process.addr_space.lock().unwrap();
            current_addr_space.release_all_pages(&self.pool);
            *current_addr_space = prepared.addr_space;
        }
        *task.process.exec_path.lock().unwrap() = prepared.exec_path;
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
}
