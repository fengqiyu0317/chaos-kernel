use super::*;
use crate::trap::TrapFrame;

struct PreparedExec {
    exec_path: String,
    addr_space: AddrSpace,
    user_entry: UserEntry,
    close_fds: Vec<usize>,
}

impl Kernel {
    // AGENT: resolve one executable pathname to the shared inode-like FileNode
    // and read it through the FileStorage selected by its FInstance mount.
    pub(crate) fn read_file_for_exec(&self, path: &str) -> Result<(String, Vec<u8>), &'static str> {
        let resolved = self.lookup_file_node(path)?;
        if resolved.path_ref.node.kind != FileKind::Regular {
            return Err("eisdir");
        }
        if !resolved.path_ref.node.executable.load(Ordering::Relaxed) {
            return Err("eacces");
        }
        let data = resolved
            .path_ref
            .node
            .read_all(resolved.path_ref.mount.fs().storage())?;
        Ok((resolved.display_path, data))
    }

    // AGENT: prepare exec from a path-backed executable file snapshot.
    fn prepare_exec_image(
        &self,
        task: &Arc<Task>,
        path: &str,
        args: Vec<UserCString>,
        envs: Vec<UserCString>,
    ) -> Result<PreparedExec, &'static str> {
        let (exec_path, elf_data) = self.read_file_for_exec(path)?;
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
        let PreparedExec {
            exec_path,
            addr_space,
            user_entry,
            close_fds,
        } = prepared;
        self.close_cloexec_task_fds(task, &close_fds);
        task.process.sig_state.lock().unwrap().reset_for_exec();
        // AGENT: publish the fully prepared address space in one lock-held swap,
        // then reclaim old mappings after unlock; commit performs no fallible work.
        let mut old_addr_space = {
            let mut current_addr_space = task.process.addr_space.lock().unwrap();
            mem::replace(&mut *current_addr_space, addr_space)
        };
        *task.process.exec_path.lock().unwrap() = exec_path;
        task.process.did_exec.store(true, Ordering::SeqCst);
        task.sig_frames.lock().unwrap().clear();
        old_addr_space.release_all_pages();
        user_entry
    }

    // AGENT: perform the address-space and process-state exec transaction while
    // returning the architecture entry update to the caller that owns the live frame.
    pub(crate) fn do_exec_for_trap(
        &self,
        task_id: usize,
        path: &str,
        args: Vec<UserCString>,
        envs: Vec<UserCString>,
    ) -> Result<UserEntry, &'static str> {
        let task = self.tasks.find_task(task_id).ok_or("esrch")?;
        task.kernel_stack_top().ok_or("ekstk")?;
        // AGENT: reject an already multithreaded process until the dedicated
        // begin_exec/finish_exec gate can retire siblings atomically in M9.
        if task.process.thread_count() != 1 {
            return Err("enotsup");
        }
        let prepared = self.prepare_exec_image(&task, path, args, envs)?;
        Ok(self.commit_exec(&task, prepared))
    }

    // AGENT: retain the direct semantic API used by focused tests by installing
    // the returned entry into the off-CPU task's authoritative stack frame.
    pub fn do_exec(
        &self,
        task_id: usize,
        path: &str,
        args: Vec<UserCString>,
        envs: Vec<UserCString>,
    ) -> Result<(), &'static str> {
        let user_entry = self.do_exec_for_trap(task_id, path, args, envs)?;
        let task = self.tasks.find_task(task_id).ok_or("esrch")?;
        task.install_user_trap_frame(TrapFrame::for_user_entry(
            user_entry.entry,
            user_entry.stack_pointer,
        ))?;
        Ok(())
    }
}
