use super::*;
use crate::trap::TrapFrame;

struct PreparedExec {
    exec_path: String,
    addr_space: AddrSpace,
    user_entry: UserEntry,
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
        let image = prepare_user_image(&elf_data, args, envs, &self.pool)?;
        let close_fds = task.cloexec_fds();
        Ok(PreparedExec {
            exec_path,
            addr_space: image.addr_space,
            user_entry: image.user_entry,
            close_fds,
        })
    }

    // AGENT: close FD_CLOEXEC descriptors, reset caught signal dispositions,
    // and mark successful exec so parent setpgid calls can reject children
    // after the exec boundary.
    fn commit_exec(&self, task: &Arc<Task>, prepared: PreparedExec) -> UserEntry {
        for fd in prepared.close_fds {
            let _ = task.close_fd(fd);
        }
        task.process.sig_state.lock().unwrap().reset_for_exec();
        {
            let mut current_addr_space = task.process.addr_space.lock().unwrap();
            current_addr_space.release_all_pages();
            *current_addr_space = prepared.addr_space;
        }
        *task.process.exec_path.lock().unwrap() = prepared.exec_path;
        task.process.did_exec.store(true, Ordering::SeqCst);
        task.sig_frames.lock().unwrap().clear();
        prepared.user_entry
    }

    // AGENT: perform the address-space and process-state exec transaction while
    // returning the architecture entry update to the caller that owns the live frame.
    pub(crate) fn do_exec_for_trap(
        &self,
        task_id: usize,
        path: &str,
        args: Vec<String>,
        envs: Vec<String>,
    ) -> Result<UserEntry, &'static str> {
        let task = self.tasks.find(task_id).ok_or("esrch")?;
        task.kernel_stack_top().ok_or("ekstk")?;
        let prepared = self.prepare_exec_image(&task, path, args, envs)?;
        Ok(self.commit_exec(&task, prepared))
    }

    // AGENT: retain the direct semantic API used by focused tests by installing
    // the returned entry into the off-CPU task's authoritative stack frame.
    pub fn do_exec(
        &self,
        task_id: usize,
        path: &str,
        args: Vec<String>,
        envs: Vec<String>,
    ) -> Result<(), &'static str> {
        let user_entry = self.do_exec_for_trap(task_id, path, args, envs)?;
        let task = self.tasks.find(task_id).ok_or("esrch")?;
        task.install_user_trap_frame(TrapFrame::for_user_entry(
            user_entry.entry,
            user_entry.stack_pointer,
        ))?;
        Ok(())
    }
}
