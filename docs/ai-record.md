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

## 当前不要改的部分

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
