use super::*;

struct PreparedExec {
    exec_path: String,
    addr_space: AddrSpace,
    thd_ctx: ThdCtx,
    close_fds: Vec<usize>,
}

impl Kernel {
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
        node.read_all(&self.file_storage())
    }

    // AGENT: prepare exec from a path-backed executable file snapshot.
    fn prepare_exec_image(
        &self,
        task: &Arc<Task>,
        path: &str,
        args: Vec<String>,
        envs: Vec<String>,
    ) -> Result<PreparedExec, &'static str> {
        let exec_path = self.lookup_path(path)?;
        let elf_data = self.read_file_for_exec(&exec_path)?;
        // AGENT: delegate ELF mapping and stack construction to the common
        // user-image builder; exec retains only file and commit semantics.
        let mut image = prepare_user_image(&elf_data, args, envs, &self.pool)?;
        image.thd_ctx.smask = *task.sig_mask.lock().unwrap();
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
            addr_space: image.addr_space,
            thd_ctx: image.thd_ctx,
            close_fds,
        })
    }

    // AGENT: close FD_CLOEXEC descriptors, reset caught signal dispositions,
    // and mark successful exec so parent setpgid calls can reject children
    // after the exec boundary.
    fn commit_exec(&self, task: &Arc<Task>, prepared: PreparedExec) {
        for fd in prepared.close_fds {
            let _ = task.close_fd(fd);
        }
        task.process.sig_state.lock().unwrap().clear_non_caught();
        {
            let mut current_addr_space = task.process.addr_space.lock().unwrap();
            current_addr_space.release_all_pages(&self.pool);
            *current_addr_space = prepared.addr_space;
        }
        *task.process.exec_path.lock().unwrap() = prepared.exec_path;
        task.process.did_exec.store(true, Ordering::SeqCst);
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
