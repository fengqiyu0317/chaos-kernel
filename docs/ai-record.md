# Chaos AI 工作日志

更新时间：2026-06-27

## 维护约定

- 本文件用于保存 Chaos 项目中重要的 Codex 对话结论、工作状态、测试结果和迁移记录。
- 本轮整理的来源是 Codex 会话记录：
  - `/home/huawei/.codex/sessions/2026/06/18/rollout-2026-06-18T23-48-38-019edb6b-8770-72c3-bc57-675cb839477d.jsonl`
  - `/home/huawei/.codex/sessions/2026/06/18/rollout-2026-06-18T23-53-27-019edb6f-f16f-7380-85d6-86b3469f3b3d.jsonl`
  - `/home/huawei/.codex/sessions/2026/06/18/rollout-2026-06-18T23-57-15-019edb73-6ad9-7b23-8162-c76a589a57a9.jsonl`
- 上一层 `record.md` 不作为本文件的事实来源；以后若要补日志，应优先查 Codex session JSONL 或当前项目内的 `TASK.md` / `NOTES.md`。
- 涉及 `kernel-sim` 的修改目标是 `chaos/kernel-sim/`，不要修改 `chaos/kernel/src/kernel.rs`。

## 2026-06-18：kernel-sim 阶段状态

用户要求根据对话记录和修改情况总结当前工作进度。Codex 在上一层目录 `/mnt/d/Tomato_Fish/豫文化课/新时代/大二春/操作系统` 中检查了 `chaos/` 子仓库、`kernel-sim` 代码、测试和 Git 状态。

当时确认的 `chaos` 仓库 HEAD：

```text
6fe75e8 Fix kernel-sim signal delivery
```

已经完成的主要工作：

- `kernel-sim` 已经按模块拆开，不再是单个大文件。
- 已完成一轮时间、调度、futex、signal 相关修复。
- `Kernel` 已接入 `run_queue`。
- `proc_init()` 会设置 CPU0 当前任务。
- `schedule_tick()` 会扣时间片并切换 runnable task。
- signal 已支持默认动作、用户 handler、`sigreturn` 恢复上下文。
- futex 已覆盖 wait、wake、requeue、cmp_requeue、wake_op 等路径。
- 代码中已有 `// AGENT` / `// HUMAN` 标记。

当时测试结果：

```bash
cd chaos/kernel-sim
cargo test
```

结果：`15 passed; 0 failed`。

`chaos-tests` 尚未通过：

```bash
cd chaos/chaos-tests
cargo test --test basic
```

结果：`33` 个 basic 测试中 `21 passed / 12 failed`。

失败方向记录：

- `group_01`：`GKL` 对 thread id 的限制太死，测试使用 `1001/1002/1003`，会触发 `MAX_THREAD_ID=255` 断言。
- `group_02`：sleep under spinlock 行为不对。
- `group_03`：`SyncQueue` 的 signal-before-wait 和 spurious wake 语义不对。
- `group_06`：`Disk::read_block` 成功路径没有把 buffer 填成 `0xAA`。
- `group_07`：并发 mount/lookup 还会卡住或未完成。
- `group_09`：中断 mask、page fault 处理不符合测试预期。
- `group_10/group_11`：`check_access` / overflow 判断还有问题。

补充测试状态：

- `cargo test --test advanced` 当时直接无法编译，因为 `chaos-tests/tests/advanced/main.rs` 不存在。
- 当时也没有看到 `pressure` 测试目录。
- 因此不能宣称 basic/advanced/pressure 全部通过。

文档状态：

- `kernel-sim/SCHEDULE.md` 已经过时。
- 该文档仍写着调度器没有真正接入，但实际代码已经有 `RunQueue`、`TaskRunState`、时间片切换和当前任务切换。
- 后续应更新该文档，或不要把它当作当前实现状态依据。

## 2026-06-18：长任务交接规则

用户要求修改项目指令文件，使 Codex 在长任务或 context 不足时先更新 handoff summary / 当前状态总结，而不是无限追加到同一对话。

Codex 检查后确认仓库中是 `AGENTS.md`，不是 `AGENT.md`，并在外层目录的 `AGENTS.md` 中补充了“长任务交接”规则。

规则要求：

- 长任务、上下文即将不足，或继续在同一对话中追加会降低稳定性时，先更新 handoff summary / 当前状态总结。
- 总结写入 `TASK.md`、`NOTES.md`，或对应的 issue/comment。
- 至少包括：目标、已完成修改、关键文件、测试结果、未解决问题、不要改的部分。
- 新对话继续时，应附上当前 `git diff` / `git diff --stat` 和最新测试结果。

验证说明：

- 这次只是文档修改，未运行测试。
- 当时 `AGENTS.md` 是 Git 未跟踪文件，所以 `git diff -- AGENTS.md` 没有输出，`git status --short AGENTS.md` 才能看到 `?? AGENTS.md`。

## 2026-06-18：迁移到 chaos 项目空间

用户提出以后要单独在 `chaos/` 项目空间中使用 Codex 和 GitHub，而不是继续在外层“操作系统”目录中操作。

Codex 检查结果：

- 外层目录和 `chaos/` 内部都存在 `.git`。
- `chaos/` 是独立 Git 仓库。
- `chaos/` 远程仓库：

```text
origin   https://github.com/fengqiyu0317/chaos-kernel.git
upstream https://github.com/peterzheng98/chaos.git
```

迁移原则：

- 不移动、复制或删除 `chaos/.git`。
- GitHub 历史不需要迁移，因为它已经属于 `chaos/` 仓库。
- 以后在以下目录中启动 Codex 和执行 Git 命令：

```bash
cd "/mnt/d/Tomato_Fish/豫文化课/新时代/大二春/操作系统/chaos"
codex
git status
```

后续用户要求按 `AGENTS.md` 的“长任务交接”先把属于 Chaos 的工作状态记录在 `chaos/` 内，再执行转移操作。Codex 创建或准备了以下项目内记录文件：

- `AGENTS.md`：项目规则和长任务交接要求。
- `TASK.md`：当前任务状态和交接摘要。
- `NOTES.md`：迁移说明与工作约定。
- `docs/ai-record.md`：AI 工作日志。

本轮重新整理时的当前实际状态：

```text
## master...origin/master
?? AGENTS.md
?? NOTES.md
?? TASK.md
```

当前 `docs/` 目录此前为空；本文件是在 2026-06-19 根据 Codex session JSONL 新建的项目日志。

## 2026-06-19：kernel-sim 页表级 COW 收敛

目标：按用户要求立即重构 `kernel-sim` 的 COW 内存模型，不再用 `cow_pages` 作为真实状态表，不再在 `fork_from()` 中用 `ensure_page_entry()` 补隐式 PTE，而是让所有映射入口创建 PTE，并由 `page_table` 直接驱动 fork COW。

已完成修改：

- 删除 `AddrSpace::cow_pages` 和 `ensure_page_entry()` / `default_frame_id()` 兼容层。
- `AddrSpace::fork_from()` 先复制可继承 `VmRegion`，再遍历父进程 `page_table`；对私有可写页标记父子 PTE 为 COW，对共享映射保持 writable。
- `AddrSpace::handle_cow_fault()` 只处理已存在 PTE；共享计数大于 1 时分配新 frame 并 resolve write，计数为 1 时直接恢复 writable。
- `sys_mmap()` 通过 `AddrSpace::map_region()` 创建区域和对应 PTE。
- `sys_brk()` 改为调用 `AddrSpace::resize_brk()`，堆增长时创建 heap `VmRegion` 和 PTE，避免 brk 页在 fork 页表遍历中丢失。
- `rss_pages()` / `cow_sharers()` 改为从 `page_table` 派生统计。
- `kernel-sim/tests/smoke.rs` 的 COW 测试改为断言 PTE 的 `cow`、`writable`、`frame.count()`，不再依赖 `cow_pages`。

关键文件：

- `kernel-sim/src/kernel/mm/address_space.rs`
- `kernel-sim/src/kernel/syscall/mm.rs`
- `kernel-sim/src/kernel/core/kernel_base.rs`
- `kernel-sim/tests/smoke.rs`

测试结果：

```bash
cd kernel-sim
cargo test --test smoke
cargo fmt --check
cargo test
```

结果：`cargo test --test smoke` 通过 `22 passed`；`cargo fmt --check` 通过；完整 `cargo test` 通过 `22 passed`。

补充验证：

```bash
cd chaos-tests
cargo test --test basic
cargo test --test advanced
cargo test --test pressure
```

结果：`basic` 为 `22 passed; 11 failed`。失败集中在 `group_01`、`group_02`、`group_03`、`group_06`、`group_09`、`group_10`、`group_11`，对应 `chaos-tests/src/lib.rs -> ../../kernel/src/kernel.rs` 的外部测试模拟内核路径；本轮未修改该禁改文件。`advanced` 和 `pressure` 因 `tests/advanced/main.rs`、`tests/pressure/main.rs` 不存在而无法解析测试目标。

未解决问题：

- 外部 `chaos-tests` 仍未通过；由于其 `src/lib.rs` 是指向 `kernel/src/kernel.rs` 的符号链接，本轮按规则未修改该文件。
- `unmap_range()` 仍沿用当前模型，只降低 `PgFrame` 引用计数，没有把最后一个 frame id 归还 `FramePool`；如果后续压力测试覆盖反复 mmap/munmap，需要继续评估释放语义。

## 2026-06-20：kernel-sim 事务式 do_exec

目标：修复 `Kernel::do_exec()` 直接修改当前 task 状态的问题，改为先准备完整的新 exec 映像，全部成功后再一次性提交，保证失败 exec 不破坏旧进程映像。

已完成修改：

- 新增 `PreparedExec` 准备结构，将 `exec_path`、临时 `AddrSpace`、新 `ThdCtx`、新 `vm_token` 和待关闭的 `FD_CLOEXEC` fd 先收集起来。
- `Kernel::prepare_exec_image()` 先解析 ELF `PT_LOAD`，在临时地址空间映射 text 和用户栈，计算初始栈指针并创建新线程上下文。
- `Kernel::commit_exec()` 只在准备全部成功后执行：关闭 close-on-exec fd，释放旧地址空间页，替换 `AddrSpace`、`exec_path`、`thd_ctx` 和 `vm_token`。
- `validate_elf_header()` 保持原 API，同时新增 `parse_elf_load_segments()` 返回 entry 和 `ElfLoadSegment` 列表，供 exec 装载路径使用。
- `AddrSpace::release_all_pages()` 用于 exec 回滚和提交替换时释放临时/旧映射占用的 frame。
- 新增 `do_exec_commits_new_address_space_context_and_cloexec` 与 `do_exec_failure_preserves_old_image_and_cloexec_fds` 两个 smoke 回归。

关键文件：

- `kernel-sim/src/kernel/core/kernel_ops.rs`
- `kernel-sim/src/kernel/fs/fs_misc.rs`
- `kernel-sim/src/kernel/mm/address_space.rs`
- `kernel-sim/tests/smoke.rs`
- `TASK.md`

测试结果：

```bash
cd kernel-sim
cargo fmt --check
cargo test --test smoke
cargo test
```

结果：`cargo fmt --check` 通过；`cargo test --test smoke` 通过 `25 passed`；完整 `cargo test` 通过 `25 passed`。

未解决问题：

- `do_exec()` 仍使用内置最小 ELF 占位数据作为可执行文件来源；真实 `path` 打开/读取 ELF 字节仍待实现。
- 当时地址空间只建模页表和 frame 元数据；后续已新增基础用户内存读写接口，但 ELF 段内容和用户栈 `argc/argv/envp/auxv` 写入仍未接入 loader。
- 当时 `sys_exec()` 仍未搬运用户参数；后续已接入从当前 task 地址空间读取 path/argv/envp 并调用 `Kernel::do_exec()` 的 syscall 路径。
- 多线程 exec 语义仍待补齐。

## 当前不要改的部分

- 不要修改 `chaos/kernel/src/kernel.rs`。
- `kernel-sim` 相关修复应进入 `chaos/kernel-sim/`。

## 2026-06-25：kernel-sim core 模块拆分

目标：将 `kernel-sim/src/kernel/core` 中过大的核心实现文件按功能拆分，保持原有 `Kernel` API 和运行语义不变，便于后续阅读和维护。

已完成修改：

- 将 `kernel-sim/src/kernel/core/arch.rs` 拆分为 `arch/clock.rs`、`arch/context.rs`、`arch/serial.rs`、`arch/trap.rs` 和 `arch/mod.rs`，由 `arch/mod.rs` 继续导出原有符号。
- 将 `kernel-sim/src/kernel/core/kernel_ops.rs` 改为聚合模块，并把实现拆到 `kernel_ops/sched_signal.rs`、`process.rs`、`exec.rs`、`fs_store.rs`、`memory.rs`、`pipe.rs`。
- 保持已有函数体逻辑和公开方法名不变；未为每个搬迁函数新增 `// AGENT` 注释。

关键文件：

- `kernel-sim/src/kernel/core/arch.rs`
- `kernel-sim/src/kernel/core/arch/`
- `kernel-sim/src/kernel/core/kernel_ops.rs`
- `kernel-sim/src/kernel/core/kernel_ops/`

测试结果：

```bash
cd kernel-sim
cargo fmt --check
cargo test
```

结果：`cargo fmt --check` 通过；完整 `cargo test` 通过，其中 `tests/elf.rs` 通过 `3 passed`，`tests/smoke.rs` 通过 `51 passed`。

未解决问题：

- 本次是文件结构拆分，不改变现有内核语义；已有 exec、wait4、信号、mmap 等后续语义 TODO 仍以 `TASK.md` 中记录为准。

不要改的部分：

- 不要修改 `chaos/kernel/src/kernel.rs`。
- `kernel-sim` 相关修复应进入 `chaos/kernel-sim/`。

## 2026-06-27：kernel-sim Spin current task owner 语义

目标：让 `Spin` 的 owner 检查基于 simulator 当前 `Task::id()`，避免低层同步原语依赖宿主线程身份或完整 `Kernel` 对象，同时保持 guard 自动释放和现有阻塞路径不在自旋锁内睡眠。

已完成修改：

- 新增 `kernel-sim/src/kernel/core/current.rs`，维护 CPU-local/current-task id 上下文，并由 `Kernel::set_cur()` 在 CPU0 安装当前任务 id。
- `Spin` 改为通过当前 simulator task id 做递归 acquire、非 owner release 和 guard drop 检查；`SpinGuard` 记录 acquire 时的 owner，drop 时无需再次依赖当前任务上下文。
- `sys_close()` 在获取 cache chain `SpinGuard` 前取得当前 task，避免锁内才查询 current task。
- smoke 测试显式安装测试任务 id，覆盖 `Spin` owner、非 owner、递归 acquire、`SpinLock<T>` 和 `BlockCache::fetch()` 锁外 sleep 行为。
- `TASK.md` 已把本轮 `Spin` 状态记录移动到已完成修改，并保留后续真实内核语义 TODO。

关键文件：

- `kernel-sim/src/kernel/core/current.rs`
- `kernel-sim/src/kernel/core/sync.rs`
- `kernel-sim/src/kernel/core/kernel_ops/runtime.rs`
- `kernel-sim/src/kernel/syscall/fs.rs`
- `kernel-sim/tests/smoke.rs`
- `TASK.md`

测试结果：

```bash
cd kernel-sim
cargo fmt --check
cargo test --test smoke
cargo test
```

结果：`cargo fmt --check` 通过；`cargo test --test smoke` 通过 `73 passed`；完整 `cargo test` 通过，其中 `elf` 测试 `3 passed`、`smoke` 测试 `73 passed`、doc tests 通过。

未解决问题：

- `Spin` 仍是 userspace simulator ticket-lock 模型，尚未接入抢占关闭、中断屏蔽、CPU 本地状态或调度器临界区约束。
- `SyncQueue`、`EvBus`、epoll/readiness 与 timeout 等等待路径仍有统一语义和 lost wakeup 风险 TODO。

不要改的部分：

- 不要修改 `chaos/kernel/src/kernel.rs`。
- `kernel-sim` 相关修复应进入 `chaos/kernel-sim/`。

## 2026-06-27：kernel-sim 锁语义和 TODO 分类推送记录

目标：将本地领先 `origin/master` 的 kernel-sim 锁语义修复、TODO 分类和回归测试更新到 GitHub 仓库。

已完成修改：

- 本地待推送提交包括 `f11e165 Harden kernel-sim GKL release`、`8395ad2 Classify TASK.md TODOs`、`1e53802 Refine kernel-sim spin locking`。
- GKL / spin lock 相关实现改为更明确的 guard 生命周期和 owner 校验，避免持锁睡眠、非 owner 释放和递归获取等错误语义。
- `block_cache`、`channel`、runtime tick 路径跟随锁语义调整，减少在 spin guard 内阻塞或执行复杂逻辑。
- `TASK.md` 重新分类 TODO，并记录本轮已完成的锁相关事项。
- `kernel-sim/tests/smoke.rs` 扩充锁、channel、block cache、runtime ticker 等回归覆盖。

关键文件：

- `TASK.md`
- `kernel-sim/src/kernel/core/sync.rs`
- `kernel-sim/src/kernel/core/kernel_ops/runtime.rs`
- `kernel-sim/src/kernel/fs/block_cache.rs`
- `kernel-sim/src/kernel/fs/channel.rs`
- `kernel-sim/src/kernel/syscall/fs.rs`
- `kernel-sim/tests/smoke.rs`

测试结果：

```bash
cd kernel-sim
cargo fmt --check
cargo test
```

结果：`cargo fmt --check` 通过；完整 `cargo test` 通过，其中 `tests/smoke.rs` 为 `71 passed; 0 failed`。

未解决问题：

- 本轮只覆盖 `kernel-sim` 的锁/同步模拟路径；外部 `chaos-tests` 仍不作为本次推送的验证目标。
- `TASK.md` 中其他 syscall、网络、POSIX 语义和真实内核模型差异仍待后续处理。

不要改的部分：

- 不要修改 `chaos/kernel/src/kernel.rs`。
- `kernel-sim` 相关修复应进入 `chaos/kernel-sim/`。

来源：

- 当前本地提交区间：`origin/master..HEAD`。
- 本轮 Codex 验证命令输出。

## 2026-06-26：kernel-sim Kernel 辅助方法继续拆分

### 目标

继续收敛 `kernel-sim/src/kernel/core/kernel_base.rs` 的职责，让 `Kernel` 结构体定义只保留共享模拟器状态和构造函数，把运行时、IPC、TTY、页错误和 init task 辅助方法移动到 `kernel_ops/` 子模块。按用户要求，本次发布不提交 `TASK.md` 的本地修改。

### 已完成修改

- `kernel_base.rs` 保留 `Kernel` 字段与 `Kernel::new()`，移除原本混在状态定义文件里的行为方法。
- 新增 `kernel_ops/runtime.rs`，保存 `tick()`、`cur_task()`、`set_cur()`。
- 新增 `kernel_ops/ipc.rs`，保存 `get_sem()`、`get_shm()`。
- 新增 `kernel_ops/tty.rs`，保存 `tty_push()`、`tty_pop()`。
- 将 `handle_pgfault()`、`handle_pgfault_ext()` 移入 `kernel_ops/memory.rs`，将 `proc_init()` 移入 `kernel_ops/process.rs`。
- `kernel_ops.rs` 继续作为聚合模块导入新增子模块。
- `net.rs`、`time.rs` 增加 `// AGENT TODO`，标明网络 helper 和 timer wheel 尚未接入真实运行时路径。

### 关键文件

- `kernel-sim/src/kernel/core/kernel_base.rs`
- `kernel-sim/src/kernel/core/kernel_ops.rs`
- `kernel-sim/src/kernel/core/kernel_ops/runtime.rs`
- `kernel-sim/src/kernel/core/kernel_ops/ipc.rs`
- `kernel-sim/src/kernel/core/kernel_ops/tty.rs`
- `kernel-sim/src/kernel/core/kernel_ops/memory.rs`
- `kernel-sim/src/kernel/core/kernel_ops/process.rs`
- `kernel-sim/src/kernel/core/net.rs`
- `kernel-sim/src/kernel/core/time.rs`
- `docs/ai-record.md`

### 测试结果

```bash
cd kernel-sim
cargo fmt --check
git diff --check
cargo test --test smoke
cargo test
```

结果：`cargo fmt --check` 通过；`git diff --check` 通过；`cargo test --test smoke` 通过 `51 passed`；完整 `cargo test` 通过，其中 `tests/elf.rs` 为 `3 passed`，`tests/smoke.rs` 为 `51 passed`。

### 未解决问题

- 本次是结构拆分和 TODO 标注，不改变已有内核语义；exec、wait4、mmap、timer、network 等后续语义缺口仍以项目 TODO 记录为准。
- `TASK.md` 仍有本地修改，但本次按用户要求没有提交。

### 不要改的部分

- 不要修改 `chaos/kernel/src/kernel.rs`。
- `kernel-sim` 相关修复应进入 `chaos/kernel-sim/`。

来源：Codex 本轮文件拆分与验证记录。

## 2026-06-26：kernel-sim timer wheel 接入调度 tick

### 目标

把 `kernel-sim/src/kernel/core/time.rs` 中已有的 `TimerWheel` 从孤立 helper 接入 `Kernel` 运行时状态，并从 `schedule_tick()` 的 CPU0 tick 路径推进它；同时记录 `sync.rs` 中大内核锁和等待同步 helper 的后续语义 TODO。

### 已完成修改

- `Kernel` 新增 `timers: Mutex<TimerWheel>`，由 `Kernel::new()` 初始化全局 timer wheel。
- `schedule_tick()` 在 `dtk(cpu)` 更新逻辑时钟后，对 CPU0 调用 `advance_timers()` 推进 timer wheel；非 CPU0 仍只推进 `CLK_ALL` 并返回。
- 新增 `Kernel::advance_timers()` 和占位 `dispatch_timer()`，为后续 typed timer target 分发预留集中入口。
- `TimerEntry::expired()` 改为 `CLK >= deadline`，timer 在到达 deadline 的 tick 当场过期；`TimerWheel::new()` 的初始槽位对齐当前 `CLK % TIMER_WHEEL_SIZE`。
- 新增 smoke 回归 `cpu0_schedule_tick_advances_kernel_timer_wheel`，覆盖 CPU1 tick 不触发 wheel、CPU0 tick 触发到期 timer。
- `kernel-sim/src/kernel/core/sync.rs` 增加当前使用路径说明和 `KernLock` 后续收紧 TODO；`TASK.md` 同步记录 timer wheel 接入完成与剩余 timeout / typed target / 长 deadline 语义问题。

### 关键文件

- `kernel-sim/src/kernel/core/time.rs`
- `kernel-sim/src/kernel/core/kernel_base.rs`
- `kernel-sim/src/kernel/core/kernel_ops/sched_signal.rs`
- `kernel-sim/src/kernel/core/sync.rs`
- `kernel-sim/tests/smoke.rs`
- `TASK.md`
- `docs/ai-record.md`

### 测试结果

```bash
cd kernel-sim
cargo fmt --check
git diff --check
cargo test
```

结果：`cargo fmt --check` 通过；`git diff --check` 通过；完整 `cargo test` 通过，其中 `tests/elf.rs` 为 `3 passed`，`tests/smoke.rs` 为 `52 passed`。

### 未解决问题

- timer 到期后的真实分发仍未接入，`TimerEntry.callback_id` 还没有替换为 `WaitToken`、futex waiter、epoll waiter、task wakeup 或 process timer/signal 等 typed target。
- `WaitQueue::sleep_timeout`、`SyncQueue::wait_timeout`、futex wait timeout、`epoll_wait(timeout)` 仍未统一通过 timer wheel 注册 deadline。
- timer wheel 对超过一圈的 deadline 仍缺少显式 round/counting 机制；重复 timer 重排语义仍需继续完善。
- `KernLock` 仍是简化模拟锁，owner 校验、RAII guard、公平性和真实内核抢占/中断语义仍待补齐或明确标注为非目标。

### 不要改的部分

- 不要修改 `chaos/kernel/src/kernel.rs`。
- `kernel-sim` 相关修复应进入 `chaos/kernel-sim/`。

来源：Codex 本轮 timer wheel 接入、同步模块 TODO 标注与验证记录。

## 2026-06-27：kernel-sim typed timer target 与 futex timeout

### 目标

把 timer wheel 从占位 callback id 推进到可分发的 typed target，并让 futex syscall timeout 通过 kernel-sim 逻辑 timer wheel 到期唤醒；同时收敛 `FutexBucket::wait()` 和 `wait_with_timer()` 的重复等待入队逻辑。

### 已完成修改

- `TimerEntry.callback_id` 替换为 `TimerTarget`，支持 `Noop`、`WakeToken`、`WakeTask` 和 `SignalTask`。
- `TimerWheel` 新增可取消的 `register_timer()` id 分配路径，并用绝对 deadline 复查避免超过一圈的 timer 被过早触发。
- `Kernel::timers` 改为指向 simulator-wide global timer wheel，`dispatch_timer()` 按 typed target 分发到 wait token timeout、任务唤醒或信号投递路径。
- `WaitToken` 从单一 woken bool 改为 `WaitOutcome::{Event, Timeout}`，区分普通唤醒和超时唤醒；普通 wake 返回是否真正唤醒，避免 timeout 后的 stale token 被重复计数。
- `SYS_FUTEX` 的 wait timeout 改走 `FutexBucket::wait_with_timer()`，有 timeout 时注册 timer wheel deadline；无 timeout 时保持阻塞等待。
- `FutexBucket::wait()` 和 `wait_with_timer()` 共用私有 `wait_inner()`，保留 host-time 与 kernel-timer 两种 timeout 后端，避免重复的 futex word 检查、入队和超时清理代码。
- 新增/调整 smoke 回归覆盖 `TimerTarget::WakeToken` timeout、timer 测试串行化，以及 futex timeout 后清理 stale waiter 的路径。
- `TASK.md` 同步更新 timer/futex timeout 当前状态和剩余 TODO。

### 关键文件

- `kernel-sim/src/kernel/core/time.rs`
- `kernel-sim/src/kernel/core/kernel_base.rs`
- `kernel-sim/src/kernel/core/kernel_ops/sched_signal.rs`
- `kernel-sim/src/kernel/core/sync.rs`
- `kernel-sim/src/kernel/proc/wait.rs`
- `kernel-sim/src/kernel/syscall/sync.rs`
- `kernel-sim/tests/smoke.rs`
- `TASK.md`
- `docs/ai-record.md`

### 测试结果

```bash
cd kernel-sim
cargo fmt --check
git diff --check
cargo test --test smoke
cargo test
```

结果：`cargo fmt --check` 通过；`git diff --check` 通过；`cargo test --test smoke` 通过 `53 passed`；完整 `cargo test` 通过，其中 `tests/elf.rs` 为 `3 passed`，`tests/smoke.rs` 为 `53 passed`，doc-tests 为 `0 passed`。

补充定点验证：

```bash
cd kernel-sim
cargo test --test smoke futex
cargo test --test smoke timer_target_wakes_wait_token_as_timeout
```

结果：futex 过滤测试 `9 passed`；`timer_target_wakes_wait_token_as_timeout` 通过。

### 未解决问题

- `WaitQueue::sleep_timeout`、`SyncQueue::wait_timeout` 和 `epoll_wait(timeout)` 仍未统一通过 timer wheel 注册 deadline，当前 futex syscall timeout 已完成迁移。
- POSIX timer / alarm / setitimer 的完整真实语义仍需继续补齐，尤其是 CPU-time accounting、overrun、`sigevent` 细节。
- timer wheel 还应补充远期 deadline、repeat timer 重排和取消竞态的更细回归。

### 不要改的部分

- 不要修改 `chaos/kernel/src/kernel.rs`。
- `kernel-sim` 相关修复应进入 `chaos/kernel-sim/`。

来源：Codex 本轮 typed timer target、futex timeout 和等待路径合并验证记录。

## 2026-06-25：kernel-sim 文件 I/O 与 pipe 用户内存路径

### 目标

将 `kernel-sim` 的 `open/read/write/dup/pipe` 路径从占位行为推进到真实用户内存 copy-in/copy-out、真实 fd 权限检查和真实 open-file description 共享 offset 语义，并补充 smoke 回归。

### 已完成修改

- `sys_open()` 从当前 task 用户地址空间读取 NUL 结尾路径，接入共享 `FileNode` 表、`O_CREAT`、`O_CLOEXEC`、访问模式和 `O_TRUNC` 基础语义。
- `sys_read()` / `sys_write()` 先按用户缓冲区可写/可读前缀做 copy-out/copy-in，再调用 fd entry 的真实 read/write 实现。
- fd 层区分 per-fd `FD_CLOEXEC` 和共享 open-file description，`dup`/`dup2` 共享文件 offset 与状态。
- pipe 层使用共享队列搬运真实字节，并维护读端/写端引用计数，空 pipe 且写端仍存在时返回 `again`。
- 地址空间新增 readable/writable prefix helper，用于 syscall 用户缓冲区校验和 short I/O。
- `smoke.rs` 新增文件 read/dup offset、read 错误路径、pipe read/write 三个回归测试，并把旧 open 相关测试改为写入真实用户路径。
- `TASK.md` 补充文件 I/O、pipe、用户缓冲区复制和后续语义缺口。

### 关键文件

- `kernel-sim/src/kernel/syscall/fs.rs`
- `kernel-sim/src/kernel/fs/fd.rs`
- `kernel-sim/src/kernel/fs/pipe.rs`
- `kernel-sim/src/kernel/mm/address_space.rs`
- `kernel-sim/src/kernel/proc/task.rs`
- `kernel-sim/tests/smoke.rs`
- `TASK.md`
- `docs/ai-record.md`

### 测试结果

```bash
cd kernel-sim
cargo fmt --check
git diff --check
cargo test --test smoke
cargo test
```

结果：`cargo fmt --check` 通过；`git diff --check` 通过；`cargo test --test smoke` 通过 `51 passed`；完整 `cargo test` 通过，其中 `elf.rs` 为 `3 passed`，`smoke.rs` 为 `51 passed`。

### 未解决问题

- `sys_open()` 路径解析仍是简化模型，尚未补齐 cwd 相对路径、目录遍历、符号链接、mode/umask 和完整错误边界。
- pipe 仍未实现阻塞等待、`O_NONBLOCK` 差异、写端关闭后的 EOF 唤醒、`SIGPIPE`/`EPIPE` 等完整语义。
- 用户缓冲区复制目前按 contiguous prefix 产生 short I/O，尚未接入 lazy page fault。

### 不要改的部分

- 不要修改 `chaos/kernel/src/kernel.rs`。
- 对 `kernel-sim` 相关问题，只修改 `chaos/kernel-sim/` 和必要的项目内记录文件。
- 不要移动、复制或删除 `chaos/.git`。

## 2026-06-24：kernel-sim munmap 回收与错误传播

### 目标

补齐 `kernel-sim` 中 `munmap` / `unmap_range` 的资源释放语义：解除映射时不再只降低 `PgFrame` 引用计数，而是在最后一个引用释放时归还 `FramePool`；同时让 `MAP_SHARED` 文件页写回错误能向 syscall 层传播，避免失败时已经删除 VMA/PTE。

### 已完成修改

- `PageTableEntry` 新增释放 resident frame 引用的辅助路径，最后一个引用释放时调用 `FramePool::put()` 回收 frame。
- `AddrSpace::unmap_range()` 改为接收 `FramePool` 并返回 `Result<usize, &'static str>`：先 flush 共享文件页，全部成功后再删除 VMA/PTE 并回收 frame。
- `sys_munmap()`、`MAP_FIXED` 覆盖路径和 `resize_brk()` 收缩路径都改为传播 `unmap_range()` 的错误结果。
- 新增 smoke 回归覆盖 `munmap` frame 回收、共享文件页写回错误不删除映射、`brk` 收缩归还 heap page。
- `TASK.md` 已把完成项移入“已完成修改”，并保留 eager mmap、更多 flags、brk 真实语义等后续 TODO。

### 关键文件

- `kernel-sim/src/kernel/mm/address_space.rs`
- `kernel-sim/src/kernel/syscall/mm.rs`
- `kernel-sim/tests/smoke.rs`
- `TASK.md`
- `docs/ai-record.md`

### 测试结果

```bash
cd kernel-sim
cargo fmt --check
cargo test --test smoke
cargo test
```

结果：`cargo fmt --check` 通过；`cargo test --test smoke` 通过 `48 passed`；完整 `cargo test` 通过，其中 `tests/elf.rs` 为 `3 passed`，`tests/smoke.rs` 为 `48 passed`。

补充检查：

```bash
git diff --check
```

结果：通过。

### 未解决问题

- `mmap` / `brk` 仍是 eager 分配模型，尚未实现真实内核式 VMA 先登记、缺页时再分配或装入页面。
- `MAP_FIXED_NOREPLACE`、更多 Linux mmap flags、匿名映射 fd 兼容规则等仍在 `TASK.md` 中作为后续 TODO。
- 外部 `chaos-tests` 仍指向禁改的 `kernel/src/kernel.rs`，本轮未修改该路径。

### 不要改的部分

- 不要修改 `chaos/kernel/src/kernel.rs`。
- `kernel-sim` 相关修复应进入 `chaos/kernel-sim/`。

### 来源

- 当前 Codex 对话与本轮实际 `git diff` / 测试输出。

## 2026-06-24：kernel-sim mmap 文件映射基础语义

目标：补齐 `kernel-sim` 中 `sys_mmap()` 的基础文件映射行为，让 mmap 能从 fd 对应的普通文件装入页内容，并区分 `MAP_PRIVATE` 与 `MAP_SHARED` 的写回语义。

已完成修改：

- `sys_mmap()` 校验 `prot` / `flags`、页对齐 offset、`MAP_SHARED` 与 `MAP_PRIVATE` 互斥、`MAP_FIXED` 地址合法性和用户地址空间上界。
- `FHandle::mmap()` 增加 regular file mmap 准入检查，拒绝非法范围、非普通文件、pipe 和不可读 fd。
- `AddrSpace` 新增文件页 backing 元数据，`map_file_region()` 创建 eager 文件映射页并保存文件偏移、有效长度和 shared/private 属性。
- `write_user_bytes()`、`unmap_range()`、`release_all_pages()` 会把 `MAP_SHARED` 文件页的有效文件范围写回底层 `FileNode`；`MAP_PRIVATE` 映射只保留私有快照，不写回文件。
- 新增 smoke 回归覆盖私有文件映射不写回、共享文件映射写回且不扩展 EOF 后页尾、offset 对齐和 shared writable fd 权限检查。

关键文件：

- `kernel-sim/src/kernel/syscall/mm.rs`
- `kernel-sim/src/kernel/mm/address_space.rs`
- `kernel-sim/src/kernel/fs/fd.rs`
- `kernel-sim/src/kernel/core/prelude.rs`
- `kernel-sim/tests/smoke.rs`
- `TASK.md`

测试结果：

```bash
cd kernel-sim
cargo fmt --check
git diff --check
cargo test --test smoke
cargo test
```

结果：`cargo fmt --check` 通过；`git diff --check` 通过；`cargo test --test smoke` 通过 `42 passed`；完整 `cargo test` 通过 `tests/elf.rs` 的 `3 passed` 和 `tests/smoke.rs` 的 `42 passed`。

未解决问题：

- `mmap` 仍是 eager 模型，尚未改成真实系统常见的 VMA 登记加缺页装入。
- `MAP_FIXED_NOREPLACE`、失败回滚、更多 Linux mmap flags 和匿名映射 fd 兼容规则仍待完善。
- `sys_read()` 等 syscall 文件 I/O 仍未完整接入 `ProcessState.files` 与用户缓冲区真实拷贝。

不要改的部分：

- 不要修改 `chaos/kernel/src/kernel.rs`。
- `kernel-sim` 相关修复应进入 `chaos/kernel-sim/`。

## 2026-06-27：kernel-sim runtime ticker guard

目标：为 `kernel-sim` 增加显式可控的后台 runtime ticker，使需要真实运行时推进逻辑 timer wheel 的场景可以通过 `Arc<Kernel>` 启动 100Hz CPU0 tick，同时保持普通测试仍可手动调用 `schedule_tick(0)`。

已完成修改：

- 新增 `KernelRuntimeTicker` RAII guard，通过后台线程周期性调用 `kernel.schedule_tick(0)`，并用全局单例标志避免多个 ticker 同时推进全局逻辑时钟和 timer wheel。
- `KernelRuntimeTicker::stop()` / `Drop` 会唤醒后台线程、等待退出并释放单例槽位。
- `kernel_ops.rs` 重新导出 `KernelRuntimeTicker`，不公开整个 runtime helper module。
- 新增 smoke 回归 `runtime_ticker_guard_drives_timer_waits_and_stops_cleanly`，覆盖 ticker 启动、重复启动失败、timer wait 被后台 tick 超时唤醒、停止后可重新启动。
- 补充 `TASK.md` 和 `net.rs` 中关于网络 helper 后续协议边界、checksum、socket 路径的 TODO 记录。

关键文件：

- `kernel-sim/src/kernel/core/kernel_ops.rs`
- `kernel-sim/src/kernel/core/kernel_ops/runtime.rs`
- `kernel-sim/tests/smoke.rs`
- `kernel-sim/src/kernel/core/net.rs`
- `TASK.md`

测试结果：

```bash
cd kernel-sim
cargo fmt --check
git diff --check
cargo test --test smoke
cargo test
```

结果：`cargo fmt --check` 通过；`git diff --check` 通过；`cargo test --test smoke` 通过 `54 passed`；完整 `cargo test` 通过 `elf 3 passed`、`smoke 54 passed`。

未解决问题：

- `KernelRuntimeTicker` 只是 opt-in runtime guard，默认测试和确定性调度路径仍应显式调用 `schedule_tick(0)`。
- `kernel-sim` 的 timer wheel 仍是 simulator-wide 全局状态，timer 相关测试需要继续使用串行化锁避免互相触发。
- `net.rs` 仍是 helper-only，尚未接入 socket syscall、`FLike::Socket`、loopback/虚拟网卡或真实包收发路径。

不要改的部分：

- 不要修改 `chaos/kernel/src/kernel.rs`。
- `kernel-sim` 相关修复应进入 `chaos/kernel-sim/`。

## 2026-06-27：kernel-sim IPv4 header parser 结构化返回

目标：把 `kernel-sim/src/kernel/core/net.rs` 中的 IPv4 header 解析 helper 从 tuple 返回推进到结构化返回，并补齐 total length、payload range、header checksum、TTL 和 flags/fragment 等基础协议边界检查。

已完成修改：

- 新增 `Ipv4HeaderInfo`，显式返回源/目的地址、protocol、TTL、header length、total length、payload range 和 fragment 信息。
- 新增 `Ipv4FragmentInfo`，解码 raw flags/fragment 字段、reserved、DF、MF 和 13-bit fragment offset。
- `parse_ipv4_header()` 现在拒绝 `total_len < header_len`、`total_len > pkt.len()`、缺失 options、payload range 越界和 header checksum 错误。
- `kernel-sim/tests/smoke.rs` 新增 IPv4 synthetic packet helper，并覆盖普通包、options、fragment offset、过短 total length、超过缓冲区 total length、截断 options 和坏 checksum。
- `TASK.md` 将结构化 IPv4 parser 与边界校验移动到已完成记录，保留 diagnostic error、checksum helper 和真实 socket 数据路径等后续 TODO。

关键文件：

- `kernel-sim/src/kernel/core/net.rs`
- `kernel-sim/tests/smoke.rs`
- `TASK.md`
- `docs/ai-record.md`

测试结果：

```bash
cd kernel-sim
cargo fmt --check
cargo test
git diff --check
```

结果：`cargo fmt --check` 通过；完整 `cargo test` 通过，其中 `tests/elf.rs` 为 `3 passed`，`tests/smoke.rs` 为 `60 passed`，doc-tests 为 `0 passed`；`git diff --check` 通过。

未解决问题：

- `net.rs` 仍是 helper-only，尚未接入 socket syscall、`FLike::Socket`、loopback/虚拟网卡或真实包收发路径。
- `parse_ipv4_header()` 仍返回 `Option`，后续应改成可诊断错误类型以区分 too short、not IPv4、bad IHL、bad total length 和 bad checksum。
- checksum 与 TCP helper 仍需继续收紧为通用协议工具，包括更宽累加、verify helper 和明确 TCP segment 输入语义。

不要改的部分：

- 不要修改 `chaos/kernel/src/kernel.rs`。
- `kernel-sim` 相关修复应进入 `chaos/kernel-sim/`。

## 2026-06-24：kernel-sim sys_munmap 参数校验

目标：补齐 `TASK.md` 中记录的 `sys_munmap()` syscall 入口参数语义，防止零长度、未对齐地址、长度溢出、地址区间溢出和越过用户空间边界的请求进入地址空间修改路径。

已完成修改：

- `sys_munmap()` 在修改 `AddrSpace` 前拒绝 `len == 0` 和未页对齐地址。
- `sys_munmap()` 使用 `checked_add()` 计算 `len + PAGE_SZ - 1` 和 `addr + aligned_len`，避免整数回绕。
- `sys_munmap()` 拒绝 `end > KERN_BASE` 的用户地址范围，并在无当前 task 时返回 `esrch`。
- 新增 smoke 回归覆盖无当前 task、非法参数不应解除已有映射、合法非页整倍长度向上按页解除映射。
- `TASK.md` 标记 munmap 参数校验已完成，并保留 writeback 错误传播和 frame 回收 TODO。

关键文件：

- `kernel-sim/src/kernel/syscall/mm.rs`
- `kernel-sim/tests/smoke.rs`
- `TASK.md`
- `docs/ai-record.md`

测试结果：

```bash
cd kernel-sim
cargo fmt
cargo test --test smoke munmap
cargo test
```

结果：`cargo test --test smoke munmap` 通过 `3 passed`；完整 `cargo test` 通过 `45 passed`。

未解决问题：

- `AddrSpace::unmap_range()` 仍只返回解除的 PTE 页数，`sys_munmap()` 尚未传播 `MAP_SHARED` 文件页 flush 错误。
- `AddrSpace::unmap_range()` 删除 PTE 时仍未在最后一个引用释放时把 frame 归还 `FramePool`。

不要改的部分：

- 不要修改 `chaos/kernel/src/kernel.rs`。
- `kernel-sim` 相关修复应进入 `chaos/kernel-sim/`。

来源：当前 Codex 会话、`git diff` 和本轮本地测试结果。

## 2026-06-23：kernel-sim exec 文件来源重构

### 目标

把 `kernel-sim` 的 exec ELF 来源从专用 `Kernel.exec_files` 表迁移到统一路径文件节点，让普通文件安装/写入和 exec 读取共享同一份 `FileNode` 数据，并补齐失败路径回归。

### 已完成修改

- `Kernel` 的 exec 专用表改为 `file_nodes: RwLock<BTreeMap<String, Arc<FileNode>>>`。
- 新增 `FileKind` / `FileNode`，`FHandle` 改为只保存 fd-local 状态并共享底层文件节点数据。
- 新增 `Kernel::install_file()`、`install_directory()`、`write_file_at()` 和 `read_file_for_exec()`；`install_exec_file()` 保留为安装 executable regular file 的兼容 helper。
- `prepare_exec_image()` 改为通过 `read_file_for_exec()` 读取 ELF 快照，并区分缺失路径、目录、无执行权限和非法 ELF。
- `smoke.rs` 新增 exec 文件来源回归：同一路径写入新 ELF 后 exec 加载更新 payload；非 executable、目录、缺失路径、非法 ELF 失败时保留旧地址空间、线程上下文、`FD_CLOEXEC` fd 和 frame 计数。
- `TASK.md` 已更新当前状态，并把剩余文件 I/O 问题收敛到 syscall 层 `sys_open` / `sys_read` / `sys_write` 尚未完整接入统一路径文件表。

### 关键文件

- `kernel-sim/src/kernel/core/kernel_base.rs`
- `kernel-sim/src/kernel/core/kernel_ops.rs`
- `kernel-sim/src/kernel/fs/fd.rs`
- `kernel-sim/src/kernel/fs/pipe.rs`
- `kernel-sim/tests/smoke.rs`
- `TASK.md`

### 测试结果

```bash
cd kernel-sim
cargo fmt --check
cargo test --test smoke
cargo test
```

结果：`cargo fmt --check` 通过；`cargo test --test smoke` 通过 `38 passed`；完整 `cargo test` 通过 `tests/elf.rs` 的 `3 passed` 和 `tests/smoke.rs` 的 `38 passed`。

### 未解决问题

- `sys_open` / `sys_read` / `sys_write` 仍未完整从用户地址空间读取 path 并按 fd 搬运真实文件数据；当前统一 `FileNode` 主要服务内核 helper 和 exec 读取路径。
- ELF 解析仍未校验 `e_entry` 是否落在可执行 `PT_LOAD` 段内。
- exec 仍未处理 `PT_INTERP`、动态链接器、`PT_DYNAMIC` 和重定位。

### 不要改的部分

- 不要修改 `chaos/kernel/src/kernel.rs`。
- `kernel-sim` 相关修复应进入 `chaos/kernel-sim/`。

## 2026-06-23：kernel-sim exit/wait/reap 统一路径

目标：把 `kernel-sim` 中分散的退出和等待逻辑收敛到统一路径，保证 `exit` 不再被系统调用分发层当作普通成功返回处理，并让 `wait4` 按父子关系回收 zombie、写回 wait status。

已完成修改：

- 新增 `ExitReason`，统一保存正常退出码和信号终止原因，并通过 `wait_status()` 编码 wait status。
- `Kernel::do_exit_current()` / `exit_task()` 负责当前任务退出、SIGCHLD 通知、子进程重挂和调度切换。
- `dispatch_syscall()` 引入 `SyscallOutcome::{Return, NoReturn}`，普通 syscall 成功后递送 pending signal，`SYS_EXIT` 成功后跳过普通返回后置逻辑。
- `sys_wait4()` 改为复用 `Kernel::do_wait()`，只等待当前进程的子进程，成功时写回用户态 status 并 reap 目标 task。
- 新增/更新 smoke 回归，覆盖无 current task 的 exit 错误、wait4 写 status 并回收子进程、wait4 不回收无关 zombie、信号终止原因记录。

关键文件：

- `kernel-sim/src/kernel/core/kernel_ops.rs`
- `kernel-sim/src/kernel/proc/task.rs`
- `kernel-sim/src/kernel/syscall/dispatch.rs`
- `kernel-sim/src/kernel/syscall/mod.rs`
- `kernel-sim/src/kernel/syscall/proc.rs`
- `kernel-sim/tests/smoke.rs`
- `TASK.md`
- `AGENTS.md`
- `docs/ai-record.md`

测试结果：

```bash
cd kernel-sim
cargo fmt --check
cargo test --test smoke
cargo test
```

结果：`cargo fmt --check` 通过；`cargo test --test smoke` 通过 `34 passed`；完整 `cargo test` 通过 `37 passed`。

未解决问题：

- `kernel-sim` 的退出资源释放仍未完整建模用户地址空间页、内核栈、线程上下文、IPC/定时器/信号等真实生命周期。
- 线程退出语义仍是简化模型，尚未区分 `exit`、`exit_group`、单线程退出、`clear_child_tid` futex 唤醒和 robust futex owner 退出。
- `wait4` 仍未维护或写出真实 `rusage`。

不要改的部分：

- 不要修改 `chaos/kernel/src/kernel.rs`。
- `kernel-sim` 相关修复应进入 `chaos/kernel-sim/`。

## 2026-06-23：kernel-sim exit 资源释放 RAII 化清理

目标：在已拆分 exit/wait/reap 路径的基础上，降低手写字段清理列表的维护风险，让进程级退出资源释放更靠近 `ProcessState` 这个资源拥有者。

已完成修改：

- 新增 `ProcessState::release_exit_resources()`，集中释放进程级资源。
- 对 `debug_fds`、`files`、`ep_inst`、`sig_queue`、`sem_ctx`、`shm_ctx` 使用 `mem::take`，对 `sig_state` 使用 `mem::replace`，让旧资源离开 `Mutex` 后再 drop。
- `Task::release_process_exit_resources()` 保留为调用入口，但改为委托给 `ProcessState`。
- 保留退出时 `futex.wake_all()` 和 `AddrSpace::release_all_pages()` 这两个必须显式触发的语义动作。
- 已有 smoke 回归继续覆盖 exit 后 wait 前释放 fd、epoll、信号、IPC、futex waiter、用户页、线程上下文，并保留 zombie wait status。

关键文件：

- `kernel-sim/src/kernel/proc/task.rs`
- `kernel-sim/src/kernel/core/kernel_ops.rs`
- `kernel-sim/src/kernel/core/sync.rs`
- `kernel-sim/tests/smoke.rs`
- `TASK.md`

测试结果：

```bash
cd kernel-sim
cargo fmt --check
cargo test --test smoke
cargo test
```

结果：`cargo fmt --check` 通过；`cargo test --test smoke` 通过 `39 passed`；完整 `cargo test` 通过 `tests/elf.rs` 的 `3 passed` 和 `tests/smoke.rs` 的 `39 passed`。

未解决问题：

- 当前 `sys_exit()` 仍等价于进程级退出，尚未区分单线程 `exit`、`exit_group`、`clear_child_tid` futex 写零/唤醒、robust futex owner 退出和线程组 leader wait 语义。
- `kernel-sim` 尚未建模 per-process timer 集合，因此 exit 资源释放还没有取消 alarm / interval timer / POSIX timer。

不要改的部分：

- 不要修改 `chaos/kernel/src/kernel.rs`。
- `kernel-sim` 相关修复应进入 `chaos/kernel-sim/`。

## 2026-06-23：kernel-sim 进程/线程状态边界重构

目标：修复 `clone_thread` 后再由线程执行 `fork` 时的资源来源问题，把进程级状态和线程级状态从 `Task` 中拆清楚。

已完成修改：

- 新增 `ProcessState`，集中保存 fd 表、cwd、exec path、地址空间、进程信号 disposition、pending signal、epoll、futex、IPC、父子关系、pid/pgid 和线程列表等进程级状态。
- `Task` 保留线程级状态：调度实体、内核栈、线程上下文和 per-thread signal mask。
- `clone_thread()` 改为共享同一个 `Arc<ProcessState>`，新线程只复制调用线程的上下文、TLS、`clear_tid` 和 signal mask。
- `fork_task()` 改为从调用线程所属进程复制进程级状态，同时从调用线程复制 `ThdCtx`/TLS/`clear_tid`/signal mask。
- 相关 syscall 和 MM 路径迁移到 `task.process.*`，并补充 `fork_from_cloned_thread_uses_shared_process_state_and_thread_context` 回归测试。

关键文件：

- `kernel-sim/src/kernel/proc/task.rs`
- `kernel-sim/src/kernel/core/kernel_ops.rs`
- `kernel-sim/src/kernel/syscall/`
- `kernel-sim/tests/smoke.rs`
- `TASK.md`

测试结果：

```bash
cd kernel-sim
cargo fmt --check
cargo test --test smoke
cargo test
git diff --check
```

结果：`cargo fmt --check` 通过；`cargo test --test smoke` 通过 `31 passed`；完整 `cargo test` 通过 `31 smoke + 3 elf passed`；`git diff --check` 通过。

未解决问题：

- `ProcessState::debug_fds` 仍是 smoke 测试使用的调试字段，真实 fd 语义走 `ProcessState::files`；后续可以删除该辅助状态并把测试改成直接断言真实 fd 表。
- exit/wait 语义仍待补齐：`sys_exit()`、`sys_wait4()`、`Task::exit_proc()` 和 `TaskTable::reap()` 的状态 ABI、reparent、资源释放边界还没有完整统一。

不要改的部分：

- 不要修改 `chaos/kernel/src/kernel.rs`。
- `kernel-sim` 相关修复应进入 `chaos/kernel-sim/`。

## 2026-06-23：kernel-sim exec ELF loader 文件段复制

目标：修复 `TASK.md` 中 `kernel-sim` exec 回归测试仍缺少真实 ELF 文件段复制和 bss 清零覆盖的问题，移除 `default_exec_elf()` 占位执行镜像。

已完成修改：

- `Kernel` 新增测试可注册的 exec 镜像表，`install_exec_file(path, data)` 会按 `lookup_path(path)` 的结果保存 ELF bytes。
- `Kernel::prepare_exec_image()` 改为从注册路径读取 ELF bytes，不再使用 `default_exec_elf()`。
- exec loader 在映射每个 `PT_LOAD` 时临时加入写权限，将 `p_offset..p_offset+p_filesz` 的文件内容复制到 `p_vaddr`，然后恢复 ELF 段权限。
- 新增 smoke 回归 `do_exec_loads_registered_elf_segment_bytes_and_zeroes_bss`，覆盖跨页文件段复制、bss 零填充和 text 段最终不可写。
- 既有 exec 成功/失败/syscall 回归改为显式注册测试 ELF，避免未提供真实镜像时仍依赖占位数据源成功。

关键文件：

- `kernel-sim/src/kernel/core/kernel_base.rs`
- `kernel-sim/src/kernel/core/kernel_ops.rs`
- `kernel-sim/tests/smoke.rs`
- `TASK.md`

测试结果：

```bash
cd kernel-sim
cargo fmt --check
cargo test --test elf
cargo test --test smoke
cargo test
```

结果：`cargo fmt --check` 通过；`cargo test --test elf` 通过 `3 passed`；`cargo test --test smoke` 通过 `30 passed`；完整 `cargo test` 通过 `33 passed`。

未解决问题：

- `fs_misc.rs` 尚未校验 `e_entry` 是否落在用户态、已映射且可执行的 `PT_LOAD` 段中。
- `ET_DYN`/PIE、`PT_INTERP`、动态链接器、`PT_DYNAMIC` 和重定位仍未实现。
- `brk` 初始化仍只按映像末尾页对齐，后续还需确认 data/bss、页内偏移、空洞段和 mmap 基址语义。
- 多线程 exec 语义仍待补齐。

不要改的部分：

- 不要修改 `chaos/kernel/src/kernel.rs`。
- `kernel-sim` 相关修复应进入 `chaos/kernel-sim/`。

## 2026-06-23：kernel-sim ELF segment alignment 校验

目标：补齐 `TASK.md` 中记录的 ELF `PT_LOAD` 对齐规则，避免 `ElfLoadSegment::vm_region()` 用 `saturating_sub()` 容错畸形 program header。

已完成修改：

- `parse_elf_load_segments()` 读取 `p_align`，并拒绝非 2 的幂、`p_offset % p_align != p_vaddr % p_align` 的 `PT_LOAD` 段。
- 新增页内偏移一致性校验，要求 `p_offset % PAGE_SZ == p_vaddr % PAGE_SZ`。
- `ElfLoadSegment::vm_region()` 改用 `checked_sub()` 计算页对齐文件 offset，并拒绝非页对齐结果。
- 新增 `kernel-sim/tests/elf.rs`，覆盖非法 `p_align`、非法文件/虚拟地址同余关系，以及合法页内偏移映射结果。
- `TASK.md` 同步删除已完成的 ELF segment alignment TODO，并记录本轮验证结果。

关键文件：

- `kernel-sim/src/kernel/fs/fs_misc.rs`
- `kernel-sim/tests/elf.rs`
- `TASK.md`
- `docs/ai-record.md`

测试结果：

```bash
cd kernel-sim
cargo fmt --check
cargo test --test elf
cargo test --test smoke
cargo test
```

结果：`cargo fmt --check` 通过；`cargo test --test elf` 通过 `3 passed`；`cargo test --test smoke` 通过 `28 passed`；完整 `cargo test` 通过 `31 passed`。

未解决问题：

- `prepare_exec_image()` 仍使用 `default_exec_elf()` 占位 ELF，尚未根据 path 读取真实可执行文件。
- ELF `PT_LOAD` 文件内容复制和 bss 清零仍待接入真实 loader。
- ELF `e_entry` 尚未校验是否落在可执行 `PT_LOAD` 映射内。
- 多线程 exec 语义仍待补齐。

不要改的部分：

- 不要修改 `chaos/kernel/src/kernel.rs`。
- `kernel-sim` 相关修复应进入 `chaos/kernel-sim/`。

## 2026-06-23：kernel-sim exec 初始用户栈写入

目标：补齐 `ProcInit::push_at()` 的用户栈构造逻辑，使 `exec` 提交的新地址空间中包含可由用户态读取的 `argc`、`argv`、`envp` 和 auxv。

已完成修改：

- `ProcInit::push_at()` 改为接收 `AddrSpace` 和 `FramePool`，通过 `AddrSpace::write_user_bytes()` 写入用户栈内容，并在空间不足或写入失败时返回错误。
- 用户栈现在写入参数字符串、环境变量字符串、`argc`、`argv` 指针数组、`envp` 指针数组和 auxv 终止项；`Kernel::prepare_exec_image()` 先映射用户栈，再构造初始栈。
- `TaskTable::new_user_task()` 跟随新接口映射初始用户栈并写入启动栈。
- `do_exec_commits_new_address_space_context_and_cloexec` smoke 回归新增对栈对齐、`argv[0]`、`envp[0]`、`AT_PAGESZ` 和 `AT_ENTRY` 的读取校验。
- `TASK.md` 同步更新 exec 用户栈状态，删除已完成的用户栈 TODO。

关键文件：

- `kernel-sim/src/kernel/proc/process.rs`
- `kernel-sim/src/kernel/core/kernel_ops.rs`
- `kernel-sim/src/kernel/proc/task.rs`
- `kernel-sim/tests/smoke.rs`
- `TASK.md`

测试结果：

```bash
cd kernel-sim
cargo fmt --check
cargo test --test smoke
cargo test
```

结果：`cargo fmt --check` 通过；`cargo test --test smoke` 通过 `28 passed`；完整 `cargo test` 通过 `28 passed`。

未解决问题：

- `prepare_exec_image()` 仍使用 `default_exec_elf()` 占位 ELF，尚未根据 path 读取真实可执行文件。
- ELF `PT_LOAD` 文件内容复制和 bss 清零仍待接入真实 loader。
- 多线程 exec 语义仍待补齐。

不要改的部分：

- 不要修改 `chaos/kernel/src/kernel.rs`。
- `kernel-sim` 相关修复应进入 `chaos/kernel-sim/`。

## 2026-06-22：kernel-sim 地址空间 token 语义清理

目标：修复 `TASK.md` 中记录的 `next_exec_vm_token()` 占位问题，采用长期做法让 `vm_token` 归属于 `AddrSpace`，并删除未接入的 `AddrSpace::ref_count` 死字段。

已完成修改：

- `AddrSpace::new()` 统一分配模拟地址空间 token，并从 token 派生非零 `asid`。
- 删除 `Task.vm_token` 缓存字段，新增 `Task::vm_token()` 从共享 `AddrSpace` 读取 token。
- `fork_task()` 通过 `AddrSpace::fork_from()` 创建新地址空间，`clone_thread()` 继续共享同一 `Arc<Mutex<AddrSpace>>`。
- `prepare_exec_image()` 删除 `old_token + N_PROC` / `next_exec_vm_token()` 占位生成逻辑，exec 新映像直接创建新的 `AddrSpace`。
- 删除未使用的 `AddrSpace::ref_count`，避免和 `Arc<Mutex<AddrSpace>>` / `PgFrame` 引用计数混淆。
- 新增 smoke 回归，确认 cloned thread 在 exec 后通过共享地址空间观察到新的 token。

关键文件：

- `kernel-sim/src/kernel/mm/address_space.rs`
- `kernel-sim/src/kernel/proc/task.rs`
- `kernel-sim/src/kernel/core/kernel_ops.rs`
- `kernel-sim/src/kernel/syscall/dispatch.rs`
- `kernel-sim/tests/smoke.rs`
- `TASK.md`

测试结果：

```bash
cd kernel-sim
cargo fmt --check
cargo test --test smoke
cargo test
```

结果：`cargo fmt --check` 通过；`cargo test --test smoke` 通过 `28 passed`；完整 `cargo test` 通过 `28 passed`。

未解决问题：

- `page_table_root` / `vm_token` 仍是模拟 token，尚未建模真实 `satp`、页表根、ASID generation、ASID 复用和 TLB flush/shootdown。
- `prepare_exec_image()` 仍使用 `default_exec_elf()` 占位 ELF，尚未根据 path 读取真实可执行文件。
- ELF `PT_LOAD` 文件内容、bss 清零和 `argc/argv/envp/auxv` 初始用户栈写入仍未接入 loader。
- 多线程 exec 语义仍待补齐。

不要改的部分：

- 不要修改 `chaos/kernel/src/kernel.rs`。
- `kernel-sim` 相关修复应进入 `chaos/kernel-sim/`。
- 不要移动、复制或删除 `chaos/.git`。
- 后续 Chaos 的 Git 提交应在 `chaos/` 仓库内完成，不要在外层“操作系统”仓库中提交 `chaos/` 目录。

## 后续记录模板

```markdown
## YYYY-MM-DD：标题

### 目标

### 已完成修改

### 关键文件

### 测试结果

### 未解决问题

### 不要改的部分

### 来源
```

## 2026-06-20：kernel-sim sys_exec 用户参数搬运

目标：完成 `TASK.md` 中记录的 exec syscall 缺口，让 `sys_exec()` 从当前 task 地址空间读取用户态 `path`、`argv`、`envp`，并调用已有事务式 `Kernel::do_exec()`。

已完成修改：

- `AddrSpace` 的页表项新增模拟页内容，`map_region()` 创建零页内容，`fork_from()` 继续共享同一页内容，COW 写入时复制页内容。
- 新增 `AddrSpace::read_user_bytes()`、`read_user_usize()`、`write_user_bytes()`，按 VMA/PTE 权限检查后读写模拟用户内存。
- `sys_exec()` 删除旧的占位 ELF 校验，改为读取用户 C 字符串和空指针结尾的 `argv` / `envp` 指针数组，再调用 `Kernel::do_exec()`。
- 新增 `syscall_exec_reads_user_memory_and_commits_do_exec` 和 `syscall_exec_faults_on_unmapped_user_path_without_commit` 两个 smoke 回归。

关键文件：

- `kernel-sim/src/kernel/mm/address_space.rs`
- `kernel-sim/src/kernel/syscall/proc.rs`
- `kernel-sim/tests/smoke.rs`
- `TASK.md`

测试结果：

```bash
cd kernel-sim
cargo fmt --check
cargo test --test smoke
cargo test
```

结果：`cargo fmt --check` 通过；`cargo test --test smoke` 通过 `27 passed`；完整 `cargo test` 通过 `27 passed`。

未解决问题：

- `prepare_exec_image()` 仍使用 `default_exec_elf()` 占位 ELF，尚未根据 path 读取真实可执行文件。
- ELF `PT_LOAD` 文件内容、bss 清零和 `argc/argv/envp/auxv` 初始用户栈写入仍未接入 loader。
- 多线程 exec 语义仍待补齐。

不要改的部分：

- 不要修改 `chaos/kernel/src/kernel.rs`。
- `kernel-sim` 相关修复应进入 `chaos/kernel-sim/`。

## 2026-06-28：kernel-sim pipe-backed epoll 唤醒发布

目标：把 pipe readiness 接入 `epoll_wait()` 的阻塞唤醒路径，避免仅依赖轮询/yield，并将当前 `kernel-sim` 修改提交推送到 GitHub。

已完成修改：

- `EvBus::sub()` 改为返回可取消订阅 id，新增 `EvBus::unsub()`，供 epoll 注册/删除 readiness callback。
- `EpInst` 新增 `waiters` 队列和 source subscription 表，`mark_ready()` 可唤醒阻塞的 `epoll_wait()`。
- `PipeNode` 根据读写端状态计算 readiness，并通过 `EvBus` 注册 pipe -> epoll 的唤醒 callback；`poll()` 改为单次持锁计算，避免重复锁同一 mutex。
- `sys_epoll_ctl()` 在 ADD/MOD/DEL 时同步维护 pipe source subscription；`sys_epoll_wait()` 无 ready fd 时睡入 `EpInst.waiters`，由 pipe readiness 变化唤醒。
- `kernel-sim/tests/smoke.rs` 新增 `epoll_wait_wakes_when_pipe_becomes_readable` 回归，并串行化低层 current-task/Spin 相关测试，降低并行测试干扰。
- `TASK.md` 同步更新当前 pipe-backed epoll 状态和剩余 M8 TODO。

关键文件：

- `kernel-sim/src/kernel/core/current.rs`
- `kernel-sim/src/kernel/core/sync.rs`
- `kernel-sim/src/kernel/fs/epoll.rs`
- `kernel-sim/src/kernel/fs/pipe.rs`
- `kernel-sim/src/kernel/syscall/epoll.rs`
- `kernel-sim/tests/smoke.rs`
- `TASK.md`
- `docs/ai-record.md`

测试结果：

```bash
cd kernel-sim
cargo fmt --check
cargo test --test smoke
cargo test
git diff --check
```

结果：`cargo fmt --check` 通过；`cargo test --test smoke` 通过 `74 passed`；完整 `cargo test` 通过 `elf` 3 个和 `smoke` 74 个测试；`git diff --check` 通过。

未解决问题：

- `SyncQueue` 通用等待 helper 仍未统一接入 `EpInst` / readiness wakeup 路径。
- `EvBus::change()` 仍在状态更新过程中同步执行 callback，后续需要拆分为锁内收集、锁外分发。
- 当前 pipe readiness 已覆盖 `READABLE` / `WRITABLE` / `CLOSED` / `ERROR` 到 epoll 的映射，但其他文件对象、semaphore 统计和真实等待者计数仍未统一。

不要改的部分：

- 不要修改 `chaos/kernel/src/kernel.rs`。
- `kernel-sim` 相关修复应进入 `chaos/kernel-sim/`。
