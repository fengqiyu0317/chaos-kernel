# Chaos 项目交接状态

更新日期：2026-06-19

## 目标

当前目标是把后续 Codex 和 GitHub 工作重心迁入 `chaos/` 独立项目空间。成功标准：

- 在 `chaos/` 内可以直接启动 Codex，并读取项目级 `AGENTS.md`。
- AI 对话/学习记录在 `chaos/` 内有可提交副本。
- 长任务交接状态保存在 `chaos/TASK.md` 或 `chaos/NOTES.md`。
- 后续 Git 操作默认在 `chaos/` 仓库执行，而不是外层“操作系统”仓库。

## 已完成修改

- 从外层目录复制项目规则到 `chaos/AGENTS.md`。
- 创建 `chaos/docs/`。
- 创建本文件，记录 Chaos 项目的当前交接状态。
- 创建 `chaos/NOTES.md`，记录迁移说明、GitHub 仓库状态和后续工作约定。

## 关键文件

- `chaos/AGENTS.md`：Codex 项目级规则和长任务交接要求。
- `chaos/TASK.md`：当前任务状态和交接摘要。
- `chaos/NOTES.md`：迁移说明与工作约定。
- `chaos/kernel-sim/`：后续修 bug、通过测试、重写提升质量的目标目录。
- `chaos/kernel/src/kernel.rs`：禁止修改的原始内核文件。

## 测试结果

本次只迁移项目记录和工作空间配置，没有修改 Rust 源码，未运行 `cargo test`。

迁移前在 `chaos/` 中执行过：

```bash
git status --short --branch
```

结果显示 `chaos/` 仓库原本是干净的：

```text
## master...origin/master
```

后续如果开始修改 `kernel-sim/`，按需运行：

```bash
cargo test --test basic
cargo test --test advanced
cargo test --test pressure
```

## 未解决问题

- 需要在 `chaos/` 中审查本次新增文件，然后执行 `git add`、`git commit`、`git push`。
- 后续实际内核调试目标仍是 `chaos/kernel-sim/`；本轮已完成页表级 COW 重构，详见下方 2026-06-19 补充。
- TODO: `kernel-sim` 的多线程 fork 边界尚不完整；如果 `src` 是 `clone_thread` 生成的线程 task，`fork_task` 可能从线程 task 的默认/局部字段复制 fd、cwd、IPC、epoll、信号等进程级资源，而不是从其所属进程复制进程级资源、从调用线程复制 `ThdCtx`/TLS/clear_tid/信号 mask 等线程上下文。
- TODO: `kernel-sim` 尚未把 credentials、uid/gid、supplementary groups、capability sets、securebits、no_new_privs 等进程安全身份挂到 `Task`，因此 `fork_task` 也没有实现这些真实 Linux 属性的继承规则。
- TODO: `kernel-sim` 的 fork 失败条件目前主要受全局 `N_PROC` 限制约束；尚未建模 `RLIMIT_NPROC`、系统线程数上限、`pid_max`、cgroup pids 限制、PID namespace init 退出、内存压力导致的 `ENOMEM` 等真实错误路径。
- TODO: `kernel-sim` 尚未维护每进程 resource usage / CPU time counters / page fault / I/O 统计；`wait4` 只做地址检查，没有写出真实 `rusage`，fork 后子进程统计清零语义也未完整实现。
- TODO: `kernel-sim` 尚未建模 per-task `alarm`、`setitimer`、POSIX timer 等计时器集合；真实 fork 中 child 不继承 parent timers，目前只有全局/通用 timer wheel 和 `clock_gettime` 级别的时间读取。
- TODO: `kernel-sim` 尚未建模 `mlock/mlockall` 内存锁状态、`MADV_WIPEONFORK` 清零语义，以及完整 `madvise` fork 标志；已有 `VM_DONTCOPY` 只覆盖了 DONTFORK 类似行为的一部分。
- TODO: `kernel-sim` 的 futex 模型尚未覆盖真实 Linux 的 shared futex key、priority-inheritance futex、robust futex list、`OWNER_DIED` 标记和 owner 退出时唤醒等待者等语义。
- TODO: `kernel-sim` 的 file lock 模型尚未区分 POSIX process-associated record locks、open-file-description locks 和 `flock` locks；真实 fork 中这些锁的继承/不继承规则不同。
- TODO: `kernel-sim` 尚未建模 directory streams、POSIX message queue descriptors、AIO contexts、io_uring 等对象，因此也没有对应的 fork 继承或清空规则。
- TODO: `kernel-sim` 尚未建模 `prctl` 相关进程状态，例如 `PR_SET_PDEATHSIG` 重置、timer slack 继承、I/O port permission bits 不继承等 Linux-specific fork 语义。
- TODO: `kernel-sim` 的 session / controlling TTY / job control 模型不完整；目前主要有简化 `pgid` 和 `setsid/setpgid`，尚未完整实现 session membership、foreground process group、TTY job-control signal 等 fork 相关行为。
- TODO: `kernel-sim` 尚未建模 `pthread_atfork` handler、fork 后 child 在 `exec` 前只能调用 async-signal-safe 函数等用户态线程运行时约束。
- TODO: `kernel-sim` 尚未建模 seccomp filters、ptrace relationship、LSM/security label、keyrings、namespace/cgroup membership 等安全和隔离上下文的 fork 继承或重置规则。

## 不要改的部分

- 不要修改 `chaos/kernel/src/kernel.rs`。
- 对 `kernel-sim` 相关问题，只修改 `chaos/kernel-sim/`。
- 不要移动、复制或删除 `chaos/.git`。
- 不要把后续 Chaos 提交做在外层“操作系统”仓库里。

## 2026-06-19 补充：kernel-sim 页表级 COW 重构

### 目标

将 `kernel-sim` 的 COW 模型重构为以 `page_table` 为唯一事实来源：所有映射创建 PTE，删除 `cow_pages`，`fork_from()` 不再补隐式 PTE，而是直接遍历页表做 COW。

### 已完成修改

- 删除 `AddrSpace::cow_pages` 和 `ensure_page_entry()` / `default_frame_id()`。
- `fork_from()` 复制可继承 VMA 后，遍历父地址空间 PTE；私有可写映射标记 COW，共享映射保持 writable。
- `handle_cow_fault()` 改为只处理已有 PTE，按 `PgFrame` 引用计数决定复制 frame 或直接恢复 writable。
- `sys_mmap()` / `sys_brk()` 通过 `map_region()` / `resize_brk()` 创建 VMA 和 PTE。
- COW 相关测试改为检查 PTE 状态，不再读取 `cow_pages`。

### 关键文件

- `kernel-sim/src/kernel/mm/address_space.rs`
- `kernel-sim/src/kernel/syscall/mm.rs`
- `kernel-sim/src/kernel/core/kernel_base.rs`
- `kernel-sim/tests/smoke.rs`
- `docs/ai-record.md`

### 测试结果

```bash
cd kernel-sim
cargo test --test smoke
cargo fmt --check
cargo test
```

结果：全部通过。完整 `cargo test` 中 `smoke.rs` 为 `22 passed; 0 failed`。

补充运行：

```bash
cd chaos-tests
cargo test --test basic
cargo test --test advanced
cargo test --test pressure
```

结果：`basic` 为 `22 passed; 11 failed`；`advanced` 和 `pressure` 因缺少 `tests/advanced/main.rs`、`tests/pressure/main.rs` 无法解析测试目标。

### 未解决问题

- 外部 `chaos-tests` 尚未通过，且其 `src/lib.rs` 是指向 `kernel/src/kernel.rs` 的符号链接；本轮按规则没有修改该禁改文件。
- `unmap_range()` 仍只降低 `PgFrame` 引用计数，尚未把最后一个 frame id 归还 `FramePool`。

### 不要改的部分

- 不要修改 `chaos/kernel/src/kernel.rs`。
- 对 `kernel-sim` 相关问题，只修改 `chaos/kernel-sim/` 和必要的项目内记录文件。
- 不要移动、复制或删除 `chaos/.git`。

## 下一步建议

在 `chaos/` 目录下检查迁移结果：

```bash
git status --short
git diff --stat
```

确认无误后提交：

```bash
git add AGENTS.md TASK.md NOTES.md docs/ai-record.md
git commit -m "Add Codex handoff records"
git push origin master
```
