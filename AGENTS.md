# Chaos / kernel-qemu 工作约束

## 工作范围

- 默认只分析、修改和验证 `kernel-qemu/`；除非用户明确要求，不修改或回归 `kernel-sim/`、`chaos-tests/`。
- 不得修改 `kernel/src/kernel.rs`。
- 当前目标是完善 QEMU/RISC-V `no_std` 裸机内核，并通过 RISC-V target check、QEMU selftest 和 smoke 测试。

## 设计与语义

- M9 迁移以 `docs/kernel-sim-qemu-migration-design.md` 为准：将 `kernel-sim` 已稳定的进程、地址空间、ELF/exec、fd、exit/wait、timer、pipe/epoll 和同步等待语义迁移到裸机环境，不重新设计一套内核。
- `kernel-qemu` 的 syscall 应向 Linux 理论语义对齐，包括参数校验、返回值、错误码、资源生命周期以及阻塞/唤醒规则。若 `kernel-sim` 与 Linux 理论语义不一致，应记录差异并以 Linux 语义为最终收敛方向，不固化模拟器的历史偏差。
- RISC-V syscall 层只负责 ABI 适配：从 `a7`、`a0..a5` 解码参数并将返回值写回 `a0`；不得在 trap 层另行定义 syscall 行为。
- `kernel-qemu` 只承载启动、trap、页表、timer、SBI/UART、设备 I/O 和调度等裸机适配；可复用的纯逻辑才放入 `kernel-common/`，且不得依赖 `std`、host 线程/锁/时间或 host 文件系统。
- 若实现决策与迁移设计冲突，先更新设计文档或在 `TASK.md` 记录决定。

## 验证

按改动范围运行以下检查；除非用户明确要求，不运行 `kernel-sim` 或 `chaos-tests` 回归：

```bash
cargo check --manifest-path kernel-qemu/Cargo.toml --target riscv64gc-unknown-none-elf
cargo build --manifest-path kernel-qemu/Cargo.toml --release --features qemu-selftest
bash tools/qemu-smoke.sh
```

## AI 修改记录

- 使用 `// HUMAN` 和 `// AGENT` 区分人工与 AI 代码。每个由 AI 修改的函数、结构体等块结构前都要用 `// AGENT` 说明改动，不能只在文件开头统一标注。
- 保留 AI 对话日志作为提交材料；每次更新 GitHub 仓库后同步更新 `docs/ai-record.md`。

## 长任务交接

- 长任务或上下文不足时，先在当前已使用的 `TASK.md`、`NOTES.md` 或 issue/comment 中更新交接总结；没有现成位置时使用 `TASK.md`。
- 总结至少记录目标与成功标准、已完成修改、关键文件、测试命令及结果、未解决问题、风险和禁止修改的部分，并附当前 `git diff` / `git diff --stat`。
