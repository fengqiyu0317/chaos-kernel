use super::*;

impl Kernel {
    // AGENT: allocate both pipe endpoints in one fd allocator transaction.
    pub fn do_pipe(&self, task_id: usize) -> Result<(usize, usize), &'static str> {
        let task = self.tasks.find(task_id).ok_or("esrch")?;
        let (rd, wr) = PipeNode::pair();
        task.add_file_pair_with_cloexec(FLike::Pipe(rd), FLike::Pipe(wr), false)
    }
}
