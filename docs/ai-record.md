# Chaos AI 工作日志

更新时间：2026-06-19

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
