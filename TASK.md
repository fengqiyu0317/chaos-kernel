# Chaos 项目交接状态

更新日期：2026-06-28

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
- 2026-06-24：`kernel-sim` 的 `sys_mmap()` 已补齐基础 eager 文件映射：从 fd 文件内容装入页，区分 `MAP_PRIVATE` / `MAP_SHARED`，共享映射写回 `FileNode`，私有映射不写回，文件页尾零填充，并校验 fd、offset、prot/flags 与共享写权限。
- 2026-06-24：`kernel-sim` 的文件映射路径已接到 `ProcessState.files` / `FHandle` / `FileNode`：`VmRegion::offset` 现在参与文件页内容来源，`PageTableEntry` 保存 file backing 元数据，`AddrSpace::write_user_bytes()` 会同步共享文件页。
- 2026-06-24: `kernel-sim/src/kernel/syscall/mm.rs` 的 `sys_munmap()` 已补齐入口参数校验：`len == 0` 返回 `einval`，`len + PAGE_SZ - 1` 和 `addr + aligned_len` 使用 checked arithmetic，拒绝超过 `KERN_BASE` 的用户地址范围，并在无当前 task 时返回 `esrch`。
- 2026-06-24：`kernel-sim` 的 `AddrSpace::unmap_range()` 已改为返回 `Result` 并接收 `FramePool`：先传播 `MAP_SHARED` 文件页 flush 错误，成功后再删除 VMA/PTE；最后一个 `PgFrame` 引用释放时归还 frame，`sys_munmap()`、`MAP_FIXED` 覆盖和 `brk` 收缩都复用该路径。新增 smoke 回归覆盖 munmap frame 回收、共享文件页写回错误传播、brk 收缩回收。
- 2026-06-24：`kernel-sim` 的 fd 表已改为 `FdEntry` + `OpenFileDescription` 两层模型：`FD_CLOEXEC` 留在 fd entry，dup/fork 共享 open-file description，普通文件 offset/status flags 继续由共享 `FHandle` 推进，pipe endpoint clone 会维护 readers/writers 计数。
- 2026-06-24：`sys_open()` / `sys_read()` / `sys_write()` 已接入真实用户地址空间和 `FileNode`/`FLike` 数据路径：open 从用户内存读取 C 字符串路径并使用统一路径文件表，read/write 通过 fd entry 调用普通文件或 pipe 对象，再用 `AddrSpace::{read,write}_user_bytes()` 搬运真实字节。新增 smoke 回归覆盖真实文件读、dup 共享 offset、EOF、bad fd、只写 fd、用户缓冲区 `efault`、pipe 数据读写与空 pipe `again`。
- 2026-06-26：`kernel-sim/src/kernel/core/time.rs` 的 `TimerWheel` 已接入 `Kernel` 状态：`Kernel` 持有全局 timer wheel，`schedule_tick()` 在 CPU0 更新逻辑时钟后推进它，并补充 smoke 回归覆盖 CPU0 tick 触发 timer wheel。
- 2026-06-26：`kernel-sim/src/kernel/core/time.rs` 的 `TimerEntry.callback_id` 占位已替换为 `TimerTarget` typed target；timer 到期后由 `dispatch_timer()` 分发到 `WaitToken` timeout、任务唤醒或信号投递路径，`WaitToken` 也已区分普通事件唤醒和 timeout 唤醒，futex syscall timeout 改为注册 timer wheel deadline。
- 2026-06-27：`kernel-sim` 新增显式 `KernelRuntimeTicker` runtime guard，可通过 `Arc<Kernel>` 启动后台 100Hz CPU0 tick，并在 `stop()` / `Drop` 时停止线程、释放单例 ticker 槽位；默认测试路径继续手动调用 `schedule_tick(0)`。
- 2026-06-27：`kernel-sim/src/kernel/core/net.rs` 的 `parse_ipv4_header()` 已改为返回结构化 `Ipv4HeaderInfo`，包含 header length、total length、payload range、TTL、protocol、源/目的地址和 flags/fragment 信息，并显式拒绝 `total_len < header_len`、`total_len > pkt.len()`、payload range 越界和 header checksum 错误；相关回归放在 `kernel-sim/tests/smoke.rs`。
- 2026-06-27：`kernel-sim/src/kernel/core/sync.rs` 的 `KernLock` 已改为 owner-checked `leave(id)`，新增 `KernLockGuard` RAII 释放路径并收紧 `flag` / `holder` / `depth` 字段可见性；`Kernel::tick()` 和 `BlockCache::sync_all()` 已统一改走 `GKL.guard(id)`，新增 smoke 回归覆盖递归深度、非 owner 释放、未持锁释放和 `try_guard()`。
- 2026-06-27：`kernel-sim/src/kernel/core/sync.rs` 的 `Spin` 已从裸 `AtomicBool` 改为私有 ticket-lock 状态，新增 `SpinGuard` RAII 释放、owner/depth 检查和 `SpinLock<T>`；`kernel-sim/src/kernel/core/current.rs` 负责维护由 `Kernel::set_cur()` 安装的 CPU-local current task id，避免 `Spin` 直接依赖全局 `Kernel`；`BlockCache`、`Channel`、runtime tick、`sys_close()` 已移除 `Spin.v` 直接访问，`BlockCache::fetch()` 不再持 chain 自旋锁执行 `thread::sleep()`，`Channel::recv()` 不再持自旋锁执行 `WaitToken::wait()`。
- 2026-06-27：`kernel-sim/src/kernel/core/current.rs` 的 current-task TLS 存储从 `Cell<usize>` 调整为 `AtomicUsize` relaxed load/store，保持 host-thread 本地隔离；`kernel-sim/tests/smoke.rs` 为直接设置 current-task 的 Spin/SpinLock/BlockCache 低层测试增加串行锁，避免默认并行测试下固定 task id 与 helper thread 假设互相干扰。
- 2026-06-27：`kernel-sim/src/kernel/core/sync.rs` 的 `EvBus` 已新增基于 `WaitToken` 的等待者队列；顶层 `wait_ev()` 现在在持有 `EvBus` 锁时检查事件位并原子入队，`EvBus::change()` 在事件位变化后唤醒 mask 匹配的等待者，去掉了原先的 `thread::yield_now()` 忙等路径；新增 `ev_bus_wait_ev_returns_existing_event` / `ev_bus_wait_ev_wakes_on_matching_event` smoke 回归。剩余事件模型、epoll 接线和 callback 锁外分发债务见相邻 M8 TODO。
- 2026-06-27：`kernel-sim` 的 pipe readiness 已接入 `EvBus::sub()` -> `EpInst::mark_ready()` 路径：`EvBus::sub()` 返回可取消订阅 id，`epoll_ctl(ADD/MOD/DEL)` 会为 pipe fd 注册/取消 readiness callback，`sys_epoll_wait()` 在无 ready fd 时睡入 `EpInst.waiters`，由 pipe 写入/关闭等状态变化唤醒；`PipeNode::poll()` 去掉重复锁定同一 mutex 的自锁风险。新增 `epoll_wait_wakes_when_pipe_becomes_readable` smoke 回归。
- 2026-06-28：新增 `kernel-qemu/` 最小 QEMU 裸机承载层：独立 `riscv64gc-unknown-none-elf` crate、linker script、`entry.S`、`#![no_std]` / `#![no_main]`、panic handler、SBI console、SBI shutdown，以及 `tools/qemu-smoke.sh` 启动/关机输出检查；该阶段只提供运行环境，不引入 `kernel-sim` 业务语义。
- 2026-06-28: 已建立最小 `kernel-qemu/` 承载层：`riscv64gc-unknown-none-elf` 构建、linker script、`entry.S`、`#![no_std]` / `#![no_main]`、panic handler、SBI console、SBI shutdown 和 `tools/qemu-smoke.sh`；该阶段只提供运行环境，不引入与 `kernel-sim` 冲突的业务语义。
- 2026-06-28：完成 M9 trap 第 3 点承载层：`kernel-qemu` 启动时实际安装 S-mode `stvec`，打开真实 timer interrupt 并在 QEMU smoke 中观测到 tick；同时补出 user trap 入口、`sscratch` 用户栈/内核栈切换、user trap return 和用户初始 trap frame 辅助。该阶段仍不启动用户 init，也不迁移完整 syscall/page fault 业务语义。
- 2026-06-28：完成 M9 trap 第 4 点 Rust trap handler 核心分发：`kernel-qemu/src/trap.rs` 将 timer interrupt、user `ecall`、page fault、非法指令和其他未处理 trap 拆成独立路径；user `ecall` 只推进 `sepc` 并转入 `kernel-qemu/src/syscall.rs` 的 RISC-V ABI 适配出口，syscall 语义入口仍以 `-ENOSYS` 占位等待后续迁移。

## 关键文件

- `chaos/AGENTS.md`：Codex 项目级规则和长任务交接要求。
- `chaos/TASK.md`：当前任务状态和交接摘要。
- `chaos/NOTES.md`：迁移说明与工作约定。
- `chaos/kernel-sim/`：后续修 bug、通过测试、重写提升质量的目标目录。
- `chaos/kernel-sim/src/kernel/mm/address_space.rs`：模拟用户页内容和用户内存读写接口。
- `chaos/kernel-sim/src/kernel/core/kernel_base.rs`：`Kernel` 状态，包括统一路径文件节点表。
- `chaos/kernel-sim/src/kernel/fs/fd.rs`：`FileNode` / `FHandle` / `FdEntry` / `OpenFileDescription`，共享文件内容、open-file description 状态和 fd-local close-on-exec 状态。
- `chaos/kernel-sim/src/kernel/fs/pipe.rs`：`FLike` 分派和 pipe endpoint 生命周期/读写实现。
- `chaos/kernel-sim/src/kernel/syscall/fs.rs`：`sys_open()` / `sys_read()` / `sys_write()` / dup / fcntl 文件 syscall 包装。
- `chaos/kernel-sim/src/kernel/fs/fs_misc.rs`：ELF header / `PT_LOAD` 解析和映射区域生成。
- `chaos/kernel-sim/src/kernel/syscall/proc.rs`：`sys_exec()` 用户参数搬运、`sys_exit()`/`sys_wait4()` syscall 包装。
- `chaos/kernel-sim/src/kernel/core/kernel_ops.rs`：`do_exec()`、统一退出路径、`do_wait()`。
- `chaos/kernel-sim/src/kernel/core/sync.rs`：futex wait queue 和进程退出时的 waiters 唤醒。
- `chaos/kernel-sim/src/kernel/proc/task.rs`：进程状态、退出原因、进程/线程退出资源释放、reap/reparent 辅助。
- `chaos/kernel-sim/tests/smoke.rs`：exec syscall、exit/wait 回归测试。
- `chaos/kernel-sim/tests/elf.rs`：ELF segment alignment 回归测试。
- `chaos/kernel-qemu/`：M9 QEMU 裸机最小承载层，不作为新的业务语义来源。
- `chaos/kernel-qemu/src/trap.S`：S-mode 当前栈 trap 入口、user trap `sscratch` 切栈入口和 user `sret` 返回路径。
- `chaos/kernel-qemu/src/trap.rs`：kernel/user trap vector 安装、trap frame helper 和早期 trap 分发。
- `chaos/kernel-qemu/src/syscall.rs`：RISC-V `a7` / `a0..a5` syscall ABI 解码、`kernel-sim` 风格内部 syscall 编号映射和待迁移语义入口占位。
- `chaos/kernel-qemu/src/csr.rs`：`stvec`、`sscratch`、`sstatus`、`scause`、`stval`、`sie`、`time` 等 CSR helper。
- `chaos/kernel-qemu/src/timer.rs`：QEMU/OpenSBI timer interrupt 初始化、tick 计数和下一 tick 编程。
- `chaos/tools/qemu-smoke.sh`：构建并运行 `kernel-qemu` 的 QEMU 启动/关机 smoke 脚本。
- `chaos/kernel/src/kernel.rs`：禁止修改的原始内核文件。

## 测试结果

本次 M9 trap 第 4 点修改后执行过：

```bash
cd kernel-qemu
cargo fmt
cargo build --release

cd ..
bash tools/qemu-smoke.sh
git diff --check -- kernel-qemu TASK.md docs/ai-record.md

cd kernel-sim
cargo test
```

结果：`cargo fmt` 通过；`cargo build --release` 通过；`tools/qemu-smoke.sh` 通过，QEMU 输出 `[kernel-qemu] trap vector installed stvec=0x80200020` 和 `[kernel-qemu] timer tick observed ticks=1`；`git diff --check -- kernel-qemu TASK.md docs/ai-record.md` 通过；`kernel-sim` 完整 `cargo test` 通过，其中 `elf` 测试 `3 passed`、`smoke` 测试 `74 passed`。

本次 M9 trap 第 3 点修改后执行过：

```bash
cd kernel-qemu
cargo fmt --check
cargo build --release

cd ..
bash tools/qemu-smoke.sh
git diff --check
```

结果：`cargo fmt --check` 通过；`cargo build --release` 通过；`tools/qemu-smoke.sh` 通过，QEMU 输出 `[kernel-qemu] trap vector installed stvec=0x80200020` 和 `[kernel-qemu] timer tick observed ticks=1`；`git diff --check` 通过。

本次 M9 `kernel-qemu` 最小承载层修改后执行过：

```bash
cd kernel-qemu
cargo fmt --check
cargo build --release

cd ..
bash tools/qemu-smoke.sh

cd kernel-sim
cargo test
```

结果：`cargo fmt --check` 通过；`cargo build --release` 通过；QEMU smoke 通过，OpenSBI 启动后输出 `[kernel-qemu] boot hart=0 dtb=0x87000000` 和 `[kernel-qemu] shutdown`，随后 SBI shutdown 结束 QEMU；`kernel-sim` 完整 `cargo test` 通过，其中 `elf` 测试 `3 passed`、`smoke` 测试 `74 passed`。

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

本次 mmap 基础文件映射修改后执行过：

```bash
cd kernel-sim
cargo fmt
git diff --check
cargo test --test smoke
cargo test
```

结果：`cargo fmt` 通过；`git diff --check` 通过；`cargo test --test smoke` 通过 `42 passed`；完整 `cargo test` 通过 `tests/elf.rs` 的 `3 passed` 和 `tests/smoke.rs` 的 `42 passed`。

本次 syscall 文件 I/O 与 fd 表长期模型修改后执行过：

```bash
cd kernel-sim
cargo fmt
cargo fmt --check
git diff --check
cargo test --test smoke
cargo test
```

结果：`cargo fmt` 通过；`cargo fmt --check` 通过；`git diff --check` 通过；`cargo test --test smoke` 通过 `51 passed`；完整 `cargo test` 通过 `tests/elf.rs` 的 `3 passed` 和 `tests/smoke.rs` 的 `51 passed`。

本次 timer typed target / WaitToken timeout 分发修改后执行过：

```bash
cd kernel-sim
cargo fmt
cargo test --test smoke
cargo fmt --check
git diff --check
cargo test
```

结果：`cargo fmt` 通过；`cargo test --test smoke` 通过 `53 passed`；`cargo fmt --check` 通过；`git diff --check` 通过；完整 `cargo test` 通过 `tests/elf.rs` 的 `3 passed` 和 `tests/smoke.rs` 的 `53 passed`。

本次 IPv4 header parser 结构化返回与边界校验修改后执行过：

```bash
cd kernel-sim
cargo fmt
cargo fmt --check
git diff --check
cargo test
```

结果：`cargo fmt` 通过；`cargo fmt --check` 通过；`git diff --check` 通过；完整 `cargo test` 通过 `tests/elf.rs` 的 `3 passed` 和 `tests/smoke.rs` 的 `60 passed`。

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

### 分类号

- `M0`: 仓库维护 / 交接记录
- `M1`: 进程、fork、身份、安全与 session/job-control
- `M2`: exec / ELF 装载
- `M3`: exit / wait / rusage
- `M4`: 内存管理、mmap、brk、地址空间与页表
- `M5`: 文件系统、fd、pipe 与用户缓冲区 I/O
- `M6`: timer、timeout 与 runtime ticker
- `M7`: 网络协议 helper 与 socket 路径
- `M8`: 同步原语、锁与 futex
- `M9`: `kernel-sim` 语义迁移到 QEMU / `no_std` 承载层

### M0 仓库维护 / 交接记录

- `[M0][普通]` 需要在 `chaos/` 中审查本次新增文件，然后执行 `git add`、`git commit`、`git push`。
- `[M0][普通]` 后续实际内核调试目标仍是 `chaos/kernel-sim/`；本轮已完成页表级 COW 重构，详见下方 2026-06-19 补充。

### M1 进程、fork、身份、安全与 session/job-control

- `[M1][普通] TODO`: `kernel-sim` 尚未把 credentials、uid/gid、supplementary groups、capability sets、securebits、no_new_privs 等进程安全身份挂到 `Task`，因此 `fork_task` 也没有实现这些真实 Linux 属性的继承规则。
- `[M1][普通] TODO`: `kernel-sim` 的 fork 失败条件目前主要受全局 `N_PROC` 限制约束；尚未建模 `RLIMIT_NPROC`、系统线程数上限、`pid_max`、cgroup pids 限制、PID namespace init 退出、内存压力导致的 `ENOMEM` 等真实错误路径。
- `[M1][普通] TODO`: `kernel-sim` 尚未建模 `prctl` 相关进程状态，例如 `PR_SET_PDEATHSIG` 重置、timer slack 继承、I/O port permission bits 不继承等 Linux-specific fork 语义。
- `[M1][普通] TODO`: `kernel-sim` 的 session / controlling TTY / job control 模型不完整；目前主要有简化 `pgid` 和 `setsid/setpgid`，尚未完整实现 session membership、foreground process group、TTY job-control signal 等 fork 相关行为。
- `[M1][普通] TODO`: `kernel-sim` 尚未建模 `pthread_atfork` handler、fork 后 child 在 `exec` 前只能调用 async-signal-safe 函数等用户态线程运行时约束。
- `[M1][普通] TODO`: `kernel-sim` 尚未建模 seccomp filters、ptrace relationship、LSM/security label、keyrings、namespace/cgroup membership 等安全和隔离上下文的 fork 继承或重置规则。
- `[M1][普通] TODO`: `kernel-sim/src/kernel/syscall/proc.rs` 的 `sys_getpid()` / `sys_getppid()` 已返回 `ProcessState` 级 pid/parent pid，但无当前 task 时的兜底返回值仍是模拟器行为；后续若追求真实 syscall 语义，应去掉这种正常运行中不可达的 fallback，并配套覆盖 reparent、subreaper、PID namespace 等父进程语义。
- `[M1][普通] TODO`: `kernel-sim/src/kernel/syscall/proc.rs` 的 `sys_setpgid()` / `sys_getpgid()` 目前只维护单个 `ProcessState.pgid`，能支撑简化 `wait4`/`kill` 分组路径，但尚未校验同一 session、目标进程是否已 exec、pgid 是否对应合法进程组、完整 `EINVAL`/`EACCES`/`EPERM` 错误条件等真实 POSIX 规则。
- `[M1][普通] TODO`: `kernel-sim/src/kernel/syscall/proc.rs` 的 `sys_setsid()` 目前只是拒绝已有 process-group leader 后把 `pgid` 设为自身 pid；尚未引入权威 `sid/session` 字段、session leader 状态、控制终端脱离和 foreground process group 交互，因此不能视为完整 `setsid` 语义。

### M2 exec / ELF 装载

- `[M2][普通] TODO`: `kernel-sim/src/kernel/fs/fs_misc.rs` 目前接受 `ET_DYN`，但没有实现 PIE/load bias、地址随机化、动态段解析或重定位；后续要么补齐 `ET_DYN` 装载语义，要么在未实现前只接受可直接映射的 `ET_EXEC`。
- `[M2][普通] TODO`: `kernel-sim` 的 exec ELF loader 尚未处理 `PT_INTERP`、动态链接器路径、`PT_DYNAMIC` 和重定位；动态链接 ELF 目前不能被视为完整支持。
- `[M2][普通] TODO`: `kernel-sim` 的 ELF 段权限模型目前只把 `PF_R/PF_W/PF_X` 映射为 `VM_READ/VM_WRITE/VM_EXEC`；后续可补齐 W^X、RELRO、栈执行权限、私有/共享映射等更接近真实 exec 的权限语义。
- `[M2][普通] TODO`: `kernel-sim` 的 exec 状态提交边界仍需继续补齐多线程 exec 语义；当前 `commit_exec()` 已覆盖保留非 `FD_CLOEXEC` 文件描述符、关闭 close-on-exec fd、替换地址空间、重置入口 PC/SP、信号处理帧和 `clear_tid`。
- `[M2][普通] TODO`: `kernel-sim` 的 exec `brk` 初始化目前只按已映射镜像末尾页对齐；补齐真实 ELF 装载后，需要确认 data/bss、页内偏移、空洞段和 mmap 基址下的 `brk` 语义。
- `[M2][重要] TODO`: `kernel-sim/src/kernel/fs/fs_misc.rs` 的 ELF 解析尚未校验 `e_entry` 是否位于用户地址范围内、是否落在某个已映射且带执行权限的 `PT_LOAD` 段中；后续应拒绝入口地址未映射或不可执行的畸形 ELF。

### M3 exit / wait / rusage

- `[M3][普通] TODO`: `kernel-sim` 尚未维护每进程 resource usage / CPU time counters / page fault / I/O 统计；`wait4` 只做 `rusage` 地址检查，没有写出真实 `struct rusage`，fork 后子进程统计清零语义也未完整实现。
- `[M3][普通] TODO`: `kernel-sim` 的 `wait4` `options` 目前只识别最低位 `WNOHANG`，尚未实现 `WUNTRACED`、`WCONTINUED` 等选项，也没有对未知 flag 做完整 `EINVAL` 校验。
- `[M3][普通] TODO`: `kernel-sim` 的进程状态模型尚未支持 stopped / continued 子进程状态，因此 `wait4` 目前只能报告退出后的 zombie，不能按真实 wait 语义报告 job-control stop、continue 或更完整的 signal 状态变化。
- `[M3][普通] TODO`: `kernel-sim` 的 `wait4` 用户指针处理顺序仍需修正：当前先完成 `do_wait()`/`reap()`，再向 `status` 用户地址写入；如果真实页表写入失败，可能已经错误回收子进程，后续应在回收前完成可写性验证或改为可回滚提交。
- `[M3][重要] TODO`: `kernel-sim` 的真实进程/线程退出语义仍是简化模型；当前 `sys_exit()` 等价于进程级退出并释放整组资源，尚未区分单线程 `exit`、`exit_group`、`clear_child_tid` futex 写零/唤醒、robust futex owner 退出、线程组 leader 与非 leader 的 wait 语义。
- `[M3][重要] TODO`: `kernel-sim` 的 `wait4` 仍是简化语义：无 `WNOHANG` 且存在匹配但未退出子进程时目前直接返回 `echild`，尚未实现阻塞等待、被信号中断返回、等待队列唤醒等真实行为。

### M4 内存管理、mmap、brk、地址空间与页表

- `[M4][普通] TODO`: `kernel-sim` 尚未建模 `mlock/mlockall` 内存锁状态、`MADV_WIPEONFORK` 清零语义，以及完整 `madvise` fork 标志；已有 `VM_DONTCOPY` 只覆盖了 DONTFORK 类似行为的一部分。
- `[M4][普通] TODO`: `kernel-sim/src/kernel/mm/address_space.rs` 的 `page_table_root` / `vm_token` 目前只是全局递增的模拟地址空间 token，`asid_from_token()` 也只是把 token 映射到非零 `u16`；尚未建模真实 `satp`/页表根、ASID generation、ASID 复用时的 TLB flush/shootdown 等完整 MMU 语义。
- `[M4][普通] TODO`: `kernel-sim/src/kernel/mm/bits.rs` 目前只被公开导出，尚未接入实际 `FramePool` / VMA / 页表路径；若后续把 frame allocator 改为 bitmap 或 buddy allocator，应让该模块承担空闲 bit 查找、空闲页统计、2 的幂/order 计算、地址/页号按阶对齐，以及连续页块拆分/合并所需的底层位操作，并补充对应分配/碎片回归测试。
- `[M4][普通] TODO`: `kernel-sim/src/kernel/mm/bits.rs` 后续应先补模块级回归测试，再考虑接入主分配路径；测试至少覆盖 `popcount64` / `clz64` / `ffs64` 与 Rust 内建结果一致、`align_up` / `align_down` / `log2_floor` 的边界行为，以及 `BuddyAllocator` 分配、拆分、释放后合并和碎片统计。
- `[M4][普通] TODO`: `kernel-sim/src/kernel/mm/bits.rs` 的 `BuddyAllocator::free_order()` 需要补输入校验并返回可观察错误，至少检查 `order <= max_order`、地址页对齐、地址位于 `[base_addr, base_addr + total_pages * PAGE_SZ)` 内，并避免重复 free 导致 free list 出现重复块。
- `[M4][普通] TODO`: `kernel-sim/src/kernel/mm/bits.rs` 的 buddy 地址计算应按 `base_addr` 的相对偏移处理，避免 `base_addr != 0` 时直接用 `current_addr ^ block_size` 导致合并目标错误；修复时需新增非零 base 的释放合并回归。
- `[M4][普通] TODO`: 暂缓把 `BuddyAllocator` 直接接入 `FramePool` 主路径；在替换 `FramePool.slots` 之前，必须先设计单一页帧状态来源，并覆盖 `frame_alloc` / `frame_dealloc`、COW fault、`munmap` 回收、`brk` 收缩和 exec 失败回滚等页帧生命周期回归，避免 `FramePool.slots` 与 `BuddyAllocator.free_lists` 双重记账不一致。
- `[M4][普通] TODO`: `kernel-sim` 的 `sys_mmap()` 参数校验仍可继续贴近真实 syscall：当前已校验非匿名映射 offset 页对齐、flags/prot 基本组合、`prot` 与文件打开权限、`len + addr` 溢出；后续仍需决定匿名映射 fd 兼容规则和更多 Linux mmap flags。
- `[M4][普通] TODO`: `kernel-sim` 的 `mmap` 目前仍是 eager 模型：匿名 `map_region()` 和文件 `map_file_region()` 都会立即为整个区间分配物理页；如果要靠近真实语义，需要改成先登记 VMA、缺页时再分配/装入页面，并配套覆盖 `fork` COW、`munmap`、文件共享页和 frame 回收测试。
- `[M4][普通] TODO`: `kernel-sim/src/kernel/syscall/mm.rs` 的 `sys_brk()` 目前把请求地址向上页对齐后直接保存并返回；真实语义应区分字节级 program break 和页对齐的 heap 映射范围，`brk(0)`/成功返回也应反映未页对齐的当前 break。
- `[M4][普通] TODO`: `kernel-sim` 的 `brk` 失败返回仍是简化 `Err("enomem")` 模型；若模拟 raw Linux `brk` syscall，应在失败时返回原 current break，若模拟 libc `brk()` 包装，则需要明确转换为 `0/-1` 与 errno 的边界。
- `[M4][普通] TODO`: `kernel-sim` 尚未维护 `start_brk`/最小 break 与完整 heap VMA 边界；后续应防止 `brk` 收缩误删 ELF text/data/bss 或其他非 heap 映射，并处理 heap 与 mmap/stack/resource limit 冲突时的失败回滚。
- `[M4][普通] TODO`: `kernel-sim` 的 `brk` 增长目前通过 `resize_brk()` / `map_region()` eager 分配整段物理页；若贴近真实内核，应先登记 heap VMA、缺页时再分配页面，并增加未对齐增长/收缩、失败保持原 break、低于最小 break、与 mmap 碰撞等 smoke 回归。
- `[M4][重要] TODO`: `kernel-sim` 的 `MAP_FIXED` / 重叠映射语义仍需继续完善；当前已支持页对齐 fixed 地址先拆除旧映射再建立新映射，但尚未区分 `MAP_FIXED_NOREPLACE`，也未实现失败回滚等完整真实语义。

### M5 文件系统、fd、pipe 与用户缓冲区 I/O

- `[M5][普通] TODO`: `kernel-sim` 的 file lock 模型尚未区分 POSIX process-associated record locks、open-file-description locks 和 `flock` locks；真实 fork 中这些锁的继承/不继承规则不同。
- `[M5][普通] TODO`: `kernel-sim` 尚未建模 directory streams、POSIX message queue descriptors、AIO contexts、io_uring 等对象，因此也没有对应的 fork 继承或清空规则。
- `[M5][重要] TODO`: `kernel-sim` 的 syscall 文件 I/O 已有 fd entry / open-file description 基础模型，但仍未实现 `readv`/`writev`、`pread`/`pwrite`、`lseek` syscall、目录 fd 语义、权限/credential 检查、真实设备/tty 行规程等更完整文件系统行为。
- `[M5][重要] TODO`: `kernel-sim/src/kernel/syscall/fs.rs` 的 `sys_open()` 已从用户地址空间读取路径并接入 `FileNode` 表，但路径解析仍是简化绝对路径模型；后续应补齐 cwd 相对路径、目录遍历、符号链接、mode/umask、真实 `EISDIR`/`ENOTDIR`/`ELOOP` 等错误边界。
- `[M5][重要] TODO`: `kernel-sim` 的 pipe read/write 已走真实 `PipeNode` 队列，但空 pipe 目前直接返回 `again`，尚未实现阻塞等待、`O_NONBLOCK` 差异、关闭写端后的 EOF 唤醒、`SIGPIPE`/`EPIPE` 等完整 pipe 语义。
- `[M5][重要] TODO`: `kernel-sim` 的 syscall 用户缓冲区复制目前用 contiguous readable/writable prefix 产生 short I/O；后续若实现 lazy page fault，应让 copy-in/copy-out 能触发缺页装入并精确区分 fault 前后已搬运字节。

### M6 timer、timeout 与 runtime ticker

- `[M6][普通] TODO`: 真实 fork 中 child 不继承 parent timers，目前只有全局/通用 timer wheel 和 `clock_gettime` 级别的时间读取。
- `[M6][普通] TODO`: `kernel-sim` 的带超时等待仍有路径分散使用 host `Instant` / `thread::park_timeout` 或轮询；后续应继续让 `WaitQueue::sleep_timeout`、`SyncQueue::wait_timeout`、`epoll_wait(timeout)` 等统一通过 timer wheel 注册 deadline。当前 futex syscall timeout 已接入 timer wheel。
- `[M6][普通] TODO`: `kernel-sim/src/kernel/core/kernel_ops/runtime.rs` 的 `KernelRuntimeTicker` 停机路径当前仍直接使用 `std::sync::Condvar` 管理宿主线程 wait/notify；项目长期应提供自有 runtime wait primitive 包住这层宿主语义，避免业务代码直接依赖 Rust 自带 `Condvar`，但该 primitive 不能依赖由 ticker 自己推进的逻辑 timer wheel。
- `[M6][普通] TODO`: `kernel-sim/src/kernel/core/time.rs` 的 timer wheel 超过一圈 deadline 和 repeat timer 已依赖绝对 deadline 复查避免提前触发；后续仍应补充显式回归覆盖远期 deadline、重复 timer 重排和取消竞态。
- `[M6][普通] TODO`: `kernel-sim` 尚未建模每进程 timer 集合，因此 exit 资源释放目前没有取消 per-process alarm / interval timer / POSIX timer；等 timer 状态挂到 `ProcessState` 后，需要纳入 `release_process_exit_resources()`。

### M7 网络协议 helper 与 socket 路径

- `[M7][普通] TODO`: `kernel-sim/src/kernel/core/net.rs` 目前只公开 IPv4/TCP checksum 与 IPv4 header 解析 helper，尚未接入任何运行时路径；后续若实现网络能力，应先新增 socket 文件对象并接入 `FLike` / fd 表 / `read` / `write` / `poll` / `epoll` 路径。
- `[M7][普通] TODO`: `kernel-sim/src/kernel/core/net.rs` 后续应把 `parse_ipv4_header()` 的 `Option` 返回改为可诊断错误类型，区分 too short、not IPv4、bad IHL、bad total length、bad checksum 等失败原因，便于单元测试和后续包接收路径处理。
- `[M7][普通] TODO`: `kernel-sim/src/kernel/core/net.rs` 的 checksum helper 应更适合作为通用协议工具：`compute_inet_checksum()` 使用更宽累加类型避免长输入溢出，补充 `verify_inet_checksum()`，并把 IPv4 header checksum 验证表达为对 header 数据的统一校验。
- `[M7][普通] TODO`: `kernel-sim/src/kernel/core/net.rs` 的 TCP helper 需要明确输入是 TCP segment 而非普通 payload：后续应拆出 `tcp_checksum_ipv4()` / `verify_tcp_checksum_ipv4()`，校验 segment 长度、最小 TCP header 长度，并明确 checksum 字段由调用者清零还是函数内部清零。
- `[M7][普通] TODO`: `kernel-sim/src/kernel/core/net.rs` 的 `build_pseudo_header()` 可改为返回固定 `[u8; 12]`，避免为 IPv4 pseudo header 分配 `Vec`；同时补充奇数字节长度、带 IPv4 options、checksum 错误、长度字段异常、TCP segment 超长等单元测试。
- `[M7][普通] TODO`: `kernel-sim` 尚未实现 `SYS_SOCKET`、`SYS_BIND`、`SYS_CONNECT`、`SYS_ACCEPT`、`SYS_SENDTO`、`SYS_RECVFROM` 等 socket syscall；若补齐 `AF_INET` / `SOCK_STREAM` / `SOCK_DGRAM`，应让 syscall 创建或操作 `FLike::Socket`，而不是绕开现有 fd 模型。
- `[M7][普通] TODO`: `kernel-sim` 尚未维护 loopback/虚拟网卡、端口绑定表、socket receive queue、连接状态和非阻塞/阻塞唤醒规则；短期可先做进程内 loopback socket，长期再把 `net.rs` 的 IPv4/TCP 解析和 checksum helper 用到真实包收发路径。
- `[M7][普通] TODO`: `kernel-sim` 若实现真实 IPv4/TCP 包模拟，发送路径应使用 `compute_inet_checksum()` 计算 IP header checksum，并使用 `tcp_checksum()` 或 `build_pseudo_header()` 计算 TCP/UDP pseudo-header checksum；接收路径应通过 `parse_ipv4_header()` 校验版本、IHL、total length、protocol 和 header checksum，再按 protocol 分发。

### M8 同步原语、锁与 futex

- `[M8][普通] TODO`: `kernel-sim` 的 futex 模型尚未覆盖真实 Linux 的 shared futex key、priority-inheritance futex、robust futex list、`OWNER_DIED` 标记和 owner 退出时唤醒等待者等语义。
- `[M8][重要] TODO`: `kernel-sim/src/kernel/core/sync.rs` 的 `SyncQueue` 通用等待 helper 尚未实现真实 condition-variable / wait-queue 原子语义；`park_on()` / `wait_ev()` / `wait_events()` 会在检查条件后释放条件锁，再把 `WaitToken` 放入队列，存在条件变化与 `signal()` 发生在入队前导致 lost wakeup 的风险。
- `[M8][重要] TODO`: `SyncQueue::wait_guard()` / `SyncQueue::wait_timeout()` 当前只接收 `&Mutex<T>` 并在内部重新 `lock()` 后立即 `drop()`，不能释放调用者已经持有的 `MutexGuard`，也不能在唤醒后重新持有该 guard；后续应改成接收 guard/token registration 的 API，明确“入队、释放锁、睡眠、唤醒后重拿锁”的边界，避免自死锁和误用。
- `[M8][普通] TODO`: `SyncQueue::wait_timeout()` 仍通过 `WaitToken::wait(Some(timeout))` 使用 host `Instant` / `thread::park_timeout`；后续应接入已有 `WaitToken::wait_with_timer()` / timer wheel deadline，并与 `WaitQueue::sleep_timeout`、`epoll_wait(timeout)` 的超时语义统一。
- `[M8][普通] TODO`: `SyncQueue` 的 `RegEp` / `eq` 目前只是本地登记表，`signal()` / `broadcast()` / `signal_n()` 不会向 `EpInst` 或 `sys_epoll_wait()` 发布 readiness；当前 pipe-backed epoll 已有 `EvBus` callback 唤醒路径，但 `SyncQueue` 自身仍未接线，后续应把 readiness、等待队列和 epoll wakeup 接成同一条路径，或删除未接线的 `RegEp` 接口。
- `[M8][普通] TODO`: `Channel` 目前绕过 `SyncQueue` 通用 helper，直接访问 `wq.q` 并依赖 `buf` 锁与队列锁的手写顺序避免丢失唤醒；后续应为该模式提供安全封装或专用 API，避免其他调用者复制裸队列访问方式，并补充 send/recv/close 并发回归。
- `[M8][普通] TODO`: `kernel-sim/src/kernel/core/sync.rs` 的 `EvBus` 目前只是 `u32` 事件位图加 callback 列表，缺少事件来源、事件类型载荷、事件计数、一次性/持续性事件、边沿触发/水平触发等完整事件模型；连续同类事件会被同一个 bit 合并。
- `[M8][普通] TODO`: `EvBus::sub()` 已用于 pipe -> epoll readiness 唤醒，顶层 `wait_ev()` 仍没有接入主要 syscall 等待路径；实际阻塞等待仍分散在 `WaitToken` / `SyncQueue` / `WaitQueue` / `EpInst.waiters`。后续应继续统一 readiness state、wait queue、epoll registration、取消注册、timeout 和 wake one/all 语义。
- `[M8][普通] TODO`: `EvBus::change()` 在事件状态更新过程中同步执行 callbacks，且通常发生在外层 `Mutex<EvBus>` 持锁期间；后续应拆分状态更新、待唤醒对象收集和锁外分发，降低 callback 重入、锁顺序反转或死锁风险。
- `[M8][普通] TODO`: `EvBus` 与文件 readiness / semaphore 统计的连接仍是简化模型：pipe 已开始维护 `READABLE` / `WRITABLE` / `CLOSED` / `ERROR` 到 epoll 的唤醒映射，但其他文件对象、semaphore 统计和真实等待者计数仍未统一；`Sema::get_ncnt()` 依赖 `cb_len()` 但 acquire 路径没有登记真实等待者。
- `[M8][普通] TODO`: `Spin` 剩余真实内核语义债务：当前 ticket-lock 仍是 userspace simulator 模型，没有接入抢占关闭、中断屏蔽、CPU 本地状态或调度器临界区约束；后续若继续贴近内核语义，应定义 spin 临界区是否允许 host mutex、是否需要 irqsave/irqrestore 变体，并逐步把适合短临界区的数据从“`Spin` + 其他锁”迁移到 `SpinLock<T>`。
- `[M8][重要] TODO`: `kernel-sim` 的 `KernLock` 目前仍只是可重入自旋式模拟锁，缺少公平性、阻塞等待、抢占/中断控制等真实内核大锁语义；当前 guard 只解决 owner-checked 释放和 guard 路径的自动释放，若后续要把它作为真实大内核锁模型，应继续补齐这些语义或在接口文档中明确它只是 simulator 简化实现。

### M9 `kernel-sim` 语义迁移到 QEMU / `no_std` 承载层

- `[M9][重要] TODO`: 迁移设计以 `docs/kernel-sim-qemu-migration-design.md` 为准；核心目标是把 `kernel-sim` 已稳定的进程、地址空间、ELF/exec、fd/open-file-description、exit/wait、timer、pipe/epoll、同步等待等语义迁移到 QEMU 裸机环境，而不是重新设计一套新内核。
- `[M9][重要] TODO`: 建立迁移清单和语义基线：每个第一批迁移对象都要标出 `kernel-sim` 中的语义源文件或 smoke/elf 测试、QEMU 侧必须替换的 host 依赖、可抽到 `kernel-common/` 的 no_std/alloc 纯逻辑，以及必须留在 `kernel-qemu/` 的裸机适配代码。
- `[M9][重要] TODO`: 保留 `kernel-sim/` 作为 host 语义回归基准；迁移过程中不得删除或替换 `kernel-sim/`，不得把 host 测试路径改成依赖 QEMU，也不得修改 `chaos/kernel/src/kernel.rs`。
- `[M9][重要] TODO`: 实现 RISC-V trap / interrupt / syscall ABI 适配层：设置 `stvec`，定义 trap frame，处理 user `ecall`、timer interrupt、page fault 和非法指令；syscall 层只负责从 `a7` / `a0..a5` 解码到迁移后的 `kernel-sim` syscall 语义入口，返回值写回 `a0` 并推进 `sepc`，不要在 trap 层重新定义 syscall 行为。
- `[M9][重要] TODO`: 用真实 timer interrupt 替换 host 后台时间推进：`KernelRuntimeTicker` 不进入裸机路径，timer tick 需要对接后续 `kernel-sim` 等待/超时语义所需的 deadline、timeout 和 wakeup 接口。
- `[M9][重要] TODO`: 用真实物理页和 Sv39 页表承载 `kernel-sim` 地址空间语义：从 QEMU 物理内存范围初始化 frame allocator，映射 kernel text/rodata/data/bss、内核栈、trap/trampoline 和用户页；`AddrSpace` 的 VMA 权限、映射生命周期、COW、`mmap`/`munmap`/`brk` 错误返回和 frame 回收语义要保留，底层页内容不能继续依赖 `Arc<Mutex<Vec<u8>>>`。
- `[M9][重要] TODO`: 迁移第一个用户进程路径：内嵌 init ELF 或 initramfs 只作为启动载体，ELF `PT_LOAD`、用户栈 `argc/argv/envp/auxv`、pid/task 初始化和 `exec` 地址空间替换语义应对齐 `kernel-sim`；第一阶段成功标准是 init 能通过 `write` 输出并 `exit`。
- `[M9][普通] TODO`: 把等待、同步和调度从 host-thread 承载改成 task 承载：`WaitToken`、futex、epoll、pipe、timer timeout 的可观察唤醒语义要迁移到 `TaskRunState`、run queue、wait queue 和 trap/tick 调度边界，而不是 `std::thread::park()` / `unpark()`。
- `[M9][普通] TODO`: fd/文件层先迁移 `kernel-sim` 的 fd table、open-file-description、共享 offset、`FD_CLOEXEC`、dup/dup2/fcntl、pipe readiness、epoll ready list 和 waiter 唤醒语义；SBI/UART 只是 fd `1`/`2` 的最小字符设备后端，不要先扩展完整文件系统、网络或 virtio-blk。
- `[M9][普通] TODO`: `kernel-common/` 只能放不依赖 `std`、host 线程、host 锁、host 文件系统的代码，例如 syscall 常量、ELF 解析结构、地址对齐 helper、纯数据结构和部分错误码定义；暂时不要共享 `KernelRuntimeTicker`、host lock/thread/time、模拟地址空间或测试专用 helper。
- `[M9][普通] TODO`: 保留 `kernel-sim` 的 `cargo test` / `smoke.rs` 作为 host 语义回归，同时新增 QEMU smoke 测试脚本，至少自动检查裸机启动打印、timer trap 生效、内嵌 init 输出、`exit` 后关机；不要把 `chaos-tests` 直接当成 QEMU 移植的回归标准，除非后续明确接入该测试体系。
- `[M9][普通] TODO`: 每完成一个 M9 里程碑后，同步更新 `TASK.md` / `docs/ai-record.md`，记录目标、已完成修改、关键文件、QEMU 命令、host 测试结果、QEMU smoke 结果、剩余限制和禁止修改范围；新增 QEMU 行为时必须说明对应的 `kernel-sim` 语义是否已经存在，不存在则进入 TODO。

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
