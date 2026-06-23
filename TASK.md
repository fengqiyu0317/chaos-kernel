# Chaos 项目交接状态

更新日期：2026-06-23

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
- 2026-06-20：`kernel-sim` 的 `Kernel::do_exec()` 已改为事务式准备/提交：先在临时 `AddrSpace` 中解析 ELF `PT_LOAD`、映射 text/stack、构造新 `ThdCtx` 并收集 `FD_CLOEXEC`，全部成功后再替换当前 task 状态。
- 2026-06-20：新增 exec smoke 回归，覆盖成功 exec 后地址空间/PC/SP/`FD_CLOEXEC` 提交，以及失败 exec 不破坏旧映像、不关闭 `FD_CLOEXEC` fd、不泄漏临时 frame。
- 2026-06-20：`kernel-sim` 的 `sys_exec()` 已接入 `Kernel::do_exec()`；syscall 层会从当前 task 的 `AddrSpace` 读取用户态 `path`、`argv`、`envp`，再调用事务式 exec 提交路径。
- 2026-06-20：`AddrSpace` 新增模拟页内容和 `read_user_bytes()` / `read_user_usize()` / `write_user_bytes()`，为 syscall 参数搬运和后续 ELF/用户栈写入提供基础。
- 2026-06-20：新增 exec syscall smoke 回归，覆盖从用户地址空间搬运参数后提交 exec，以及未映射用户 path 返回 `efault` 且不破坏旧进程映像。
- 2026-06-22：`kernel-sim` 的 `vm_token` 改为由 `AddrSpace` 统一分配和持有；删除 `Task.vm_token` 缓存字段，`fork`/`exec` 创建新地址空间时自然获得新 token，`clone_thread` 共享地址空间时通过 `Task::vm_token()` 读取同一 token。
- 2026-06-22：`kernel-sim` 的 `ProcInit::push_at()` 已改为真正构造用户初始栈：写入 `argc`、`argv`、`envp`、字符串区和 auxv 终止项；`Kernel::prepare_exec_image()` 先映射用户栈再通过 `AddrSpace::write_user_bytes()` 写入，并继续在失败时释放临时地址空间；exec auxv 至少包含 `AT_PAGESZ` 和 `AT_ENTRY`。
- 2026-06-23：`kernel-sim/src/kernel/fs/fs_misc.rs` 的 ELF `PT_LOAD` 解析已补齐 `p_align` 校验，拒绝非 2 的幂、`p_offset % p_align != p_vaddr % p_align` 以及页内偏移不一致的段；`ElfLoadSegment::vm_region()` 不再用 `saturating_sub()` 容错非法 offset。
- 2026-06-23：`kernel-sim` 的 exec loader 已移除 `default_exec_elf()` 占位数据源；`Kernel::prepare_exec_image()` 改为从注册的路径 ELF bytes 读取镜像，映射 `PT_LOAD` 后复制文件段到用户页并恢复段权限，新增 smoke 回归覆盖跨页段复制和 bss 零填充。
- 2026-06-23：已用共享 `ProcessState` 重构 `kernel-sim` 的进程/线程边界；`clone_thread` 复用进程级状态，`fork_task` 为子进程复制新的 `ProcessState` 并从调用线程复制 `ThdCtx`/TLS/`clear_tid`/signal mask。回归测试：`fork_from_cloned_thread_uses_shared_process_state_and_thread_context`。
- 2026-06-23：`kernel-sim` 已抽出统一进程退出路径：`sys_exit()` 委托 `Kernel::do_exit_current()`，分发层用 `SyscallOutcome::NoReturn` 避免把 exit 当普通成功返回处理；`ExitReason` 统一保存退出码/信号原因，`sys_wait4()` 复用 `Kernel::do_wait()`、按子进程关系筛选 zombie、写回 wait status 并完成 reap/reparent 闭环。
- 2026-06-23：`kernel-sim` 的 exec 文件来源已从专用 `exec_files` 表改为共享 `FileNode` 路径文件表；`FHandle` 只保存 fd 状态并共享底层文件节点，`Kernel::read_file_for_exec()` 会检查 regular file、execute 权限并返回 ELF bytes 快照，新增 smoke 回归覆盖同一路径普通文件写入后 exec 加载更新内容，以及非 executable、目录、缺失路径、非法 ELF 失败时不破坏旧地址空间和 fd 表。
- 2026-06-23：`kernel-sim` 的进程退出资源释放边界已拆开并收敛到资源拥有者：`Task::exit_proc()` 只负责首次记录退出原因、通知事件并置 zombie；`ProcessState::release_exit_resources()` 用 `mem::take` / `mem::replace` 取出 fd、epoll、pending signal、signal dispositions、IPC context 等进程资源并在锁外 drop，`Kernel::exit_task()` 在 wait 前释放用户地址空间页、futex waiters、内核栈和线程上下文；`TaskTable::reap()` 保留为 wait/reap 阶段删除 zombie 记录和同进程线程表项。

## 关键文件

- `chaos/AGENTS.md`：Codex 项目级规则和长任务交接要求。
- `chaos/TASK.md`：当前任务状态和交接摘要。
- `chaos/NOTES.md`：迁移说明与工作约定。
- `chaos/kernel-sim/`：后续修 bug、通过测试、重写提升质量的目标目录。
- `chaos/kernel-sim/src/kernel/mm/address_space.rs`：模拟用户页内容和用户内存读写接口。
- `chaos/kernel-sim/src/kernel/core/kernel_base.rs`：`Kernel` 状态，包括统一路径文件节点表。
- `chaos/kernel-sim/src/kernel/fs/fd.rs`：`FileNode` / `FHandle`，共享文件内容和 fd-local 状态。
- `chaos/kernel-sim/src/kernel/fs/fs_misc.rs`：ELF header / `PT_LOAD` 解析和映射区域生成。
- `chaos/kernel-sim/src/kernel/syscall/proc.rs`：`sys_exec()` 用户参数搬运、`sys_exit()`/`sys_wait4()` syscall 包装。
- `chaos/kernel-sim/src/kernel/core/kernel_ops.rs`：`do_exec()`、统一退出路径、`do_wait()`。
- `chaos/kernel-sim/src/kernel/core/sync.rs`：futex wait queue 和进程退出时的 waiters 唤醒。
- `chaos/kernel-sim/src/kernel/proc/task.rs`：进程状态、退出原因、进程/线程退出资源释放、reap/reparent 辅助。
- `chaos/kernel-sim/tests/smoke.rs`：exec syscall、exit/wait 回归测试。
- `chaos/kernel-sim/tests/elf.rs`：ELF segment alignment 回归测试。
- `chaos/kernel/src/kernel.rs`：禁止修改的原始内核文件。

## 测试结果

本次 `kernel-sim` exec syscall 修改后执行过：

```bash
cd kernel-sim
cargo fmt --check
cargo test --test smoke
cargo test
```

结果：`cargo fmt --check` 通过；`cargo test --test smoke` 通过 `27 passed`；完整 `cargo test` 通过 `27 passed`。

本次 ELF segment alignment 修改后执行过：

```bash
cd kernel-sim
cargo fmt --check
cargo test --test elf
cargo test --test smoke
cargo test
```

结果：`cargo fmt --check` 通过；`cargo test --test elf` 通过 `3 passed`；`cargo test --test smoke` 通过 `28 passed`；完整 `cargo test` 通过 `31 passed`。

本次 exec ELF loader 修改后执行过：

```bash
cd kernel-sim
cargo fmt --check
cargo test --test elf
cargo test --test smoke
cargo test
```

结果：`cargo fmt --check` 通过；`cargo test --test elf` 通过 `3 passed`；`cargo test --test smoke` 通过 `30 passed`；完整 `cargo test` 通过 `33 passed`。

本次 exit/wait/reap 统一路径修改后执行过：

```bash
cd kernel-sim
cargo fmt --check
cargo test --test smoke
cargo test
```

结果：`cargo fmt --check` 通过；`cargo test --test smoke` 通过 `34 passed`；完整 `cargo test` 通过 `37 passed`。

本次 exec 文件来源重构后执行过：

```bash
cd kernel-sim
cargo fmt --check
cargo test --test smoke
cargo test
```

结果：`cargo fmt --check` 通过；`cargo test --test smoke` 通过 `38 passed`；完整 `cargo test` 通过 `tests/elf.rs` 的 `3 passed` 和 `tests/smoke.rs` 的 `38 passed`。

本次 exit 资源释放边界拆分后执行过：

```bash
cd kernel-sim
cargo fmt --check
cargo test --test smoke
cargo test
```

结果：`cargo fmt --check` 通过；`cargo test --test smoke` 通过 `39 passed`；完整 `cargo test` 通过 `tests/elf.rs` 的 `3 passed` 和 `tests/smoke.rs` 的 `39 passed`。

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

### not very important
- 需要在 `chaos/` 中审查本次新增文件，然后执行 `git add`、`git commit`、`git push`。
- 后续实际内核调试目标仍是 `chaos/kernel-sim/`；本轮已完成页表级 COW 重构，详见下方 2026-06-19 补充。
- TODO: `kernel-sim` 尚未把 credentials、uid/gid、supplementary groups、capability sets、securebits、no_new_privs 等进程安全身份挂到 `Task`，因此 `fork_task` 也没有实现这些真实 Linux 属性的继承规则。
- TODO: `kernel-sim` 的 fork 失败条件目前主要受全局 `N_PROC` 限制约束；尚未建模 `RLIMIT_NPROC`、系统线程数上限、`pid_max`、cgroup pids 限制、PID namespace init 退出、内存压力导致的 `ENOMEM` 等真实错误路径。
- TODO: `kernel-sim` 尚未建模 per-task `alarm`、`setitimer`、POSIX timer 等计时器集合；真实 fork 中 child 不继承 parent timers，目前只有全局/通用 timer wheel 和 `clock_gettime` 级别的时间读取。
- TODO: `kernel-sim` 尚未建模 `mlock/mlockall` 内存锁状态、`MADV_WIPEONFORK` 清零语义，以及完整 `madvise` fork 标志；已有 `VM_DONTCOPY` 只覆盖了 DONTFORK 类似行为的一部分。
- TODO: `kernel-sim` 的 futex 模型尚未覆盖真实 Linux 的 shared futex key、priority-inheritance futex、robust futex list、`OWNER_DIED` 标记和 owner 退出时唤醒等待者等语义。
- TODO: `kernel-sim` 的 file lock 模型尚未区分 POSIX process-associated record locks、open-file-description locks 和 `flock` locks；真实 fork 中这些锁的继承/不继承规则不同。
- TODO: `kernel-sim` 尚未建模 directory streams、POSIX message queue descriptors、AIO contexts、io_uring 等对象，因此也没有对应的 fork 继承或清空规则。
- TODO: `kernel-sim` 尚未建模 `prctl` 相关进程状态，例如 `PR_SET_PDEATHSIG` 重置、timer slack 继承、I/O port permission bits 不继承等 Linux-specific fork 语义。
- TODO: `kernel-sim` 的 session / controlling TTY / job control 模型不完整；目前主要有简化 `pgid` 和 `setsid/setpgid`，尚未完整实现 session membership、foreground process group、TTY job-control signal 等 fork 相关行为。
- TODO: `kernel-sim` 尚未建模 `pthread_atfork` handler、fork 后 child 在 `exec` 前只能调用 async-signal-safe 函数等用户态线程运行时约束。
- TODO: `kernel-sim` 尚未建模 seccomp filters、ptrace relationship、LSM/security label、keyrings、namespace/cgroup membership 等安全和隔离上下文的 fork 继承或重置规则。
- TODO: `kernel-sim/src/kernel/mm/address_space.rs` 的 `page_table_root` / `vm_token` 目前只是全局递增的模拟地址空间 token，`asid_from_token()` 也只是把 token 映射到非零 `u16`；尚未建模真实 `satp`/页表根、ASID generation、ASID 复用时的 TLB flush/shootdown 等完整 MMU 语义。
- TODO: `kernel-sim/src/kernel/fs/fs_misc.rs` 目前接受 `ET_DYN`，但没有实现 PIE/load bias、地址随机化、动态段解析或重定位；后续要么补齐 `ET_DYN` 装载语义，要么在未实现前只接受可直接映射的 `ET_EXEC`。
- TODO: `kernel-sim` 的 exec ELF loader 尚未处理 `PT_INTERP`、动态链接器路径、`PT_DYNAMIC` 和重定位；动态链接 ELF 目前不能被视为完整支持。
- TODO: `kernel-sim` 的 ELF 段权限模型目前只把 `PF_R/PF_W/PF_X` 映射为 `VM_READ/VM_WRITE/VM_EXEC`；后续可补齐 W^X、RELRO、栈执行权限、私有/共享映射等更接近真实 exec 的权限语义。
- TODO: `kernel-sim` 的 exec 状态提交边界仍需继续补齐多线程 exec 语义；当前 `commit_exec()` 已覆盖保留非 `FD_CLOEXEC` 文件描述符、关闭 close-on-exec fd、替换地址空间、重置入口 PC/SP、信号处理帧和 `clear_tid`。
- TODO: `kernel-sim` 的 exec `brk` 初始化目前只按已映射镜像末尾页对齐；补齐真实 ELF 装载后，需要确认 data/bss、页内偏移、空洞段和 mmap 基址下的 `brk` 语义。
- TODO: `kernel-sim` 尚未维护每进程 resource usage / CPU time counters / page fault / I/O 统计；`wait4` 只做地址检查，没有写出真实 `rusage`，fork 后子进程统计清零语义也未完整实现。
- TODO: `kernel-sim` 尚未建模每进程 timer 集合，因此 exit 资源释放目前没有取消 per-process alarm / interval timer / POSIX timer；等 timer 状态挂到 `ProcessState` 后，需要纳入 `release_process_exit_resources()`。

### important
- TODO: `kernel-sim` 的 syscall 层 `sys_open` / `sys_read` / `sys_write` 仍未完整接入用户地址空间路径解析和真实 fd 数据搬运；当前统一 `FileNode` 已服务内核内部 `install_file()` / `write_file_at()` / `read_file_for_exec()` 和 `FHandle` 共享节点，后续可继续把 syscall 文件 I/O 接到同一套路径文件表。
- TODO: `kernel-sim/src/kernel/fs/fs_misc.rs` 的 ELF 解析尚未校验 `e_entry` 是否位于用户地址范围内、是否落在某个已映射且带执行权限的 `PT_LOAD` 段中；后续应拒绝入口地址未映射或不可执行的畸形 ELF。
- TODO: `kernel-sim` 的真实进程/线程退出语义仍是简化模型；当前 `sys_exit()` 等价于进程级退出并释放整组资源，尚未区分单线程 `exit`、`exit_group`、`clear_child_tid` futex 写零/唤醒、robust futex owner 退出、线程组 leader 与非 leader 的 wait 语义。

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
