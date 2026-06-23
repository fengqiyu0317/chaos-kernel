# rCore-Tutorial & Chaos 学习记录

---

## Chaos 项目

### 概述
- **仓库**: `chaos/` — 基于 rCore 的内核调试与重写作业
- **当前目标目录**: `kernel-sim/`
- **任务**: 修 bug → 通过全部测试 → 重写提升代码质量

### 测试
```bash
cargo test --test basic     # 基础测试
cargo test --test advanced  # 进阶测试
cargo test --test pressure  # 压力测试
cargo test --test basic -- group_01  # 按组运行
```

### 内核结构
- `kernel/src/kernel.rs` — 含故意植入的 bug，覆盖锁、内存管理、调度、文件系统、IPC、信号等
- 使用 `std::` 而非 `no_std`，编译为 userspace 模拟
- 常数定义：`PAGE_SZ=4096`, `N_PROC=256`, `N_FRAMES=65536` 等

### 目录结构
```
chaos/
├── kernel/src/kernel.rs  # 待调试/重写的内核
├── chaos-tests/tests/    # 测试用例（basic/advanced/pressure）
├── modules/              # 内核模块
├── rboot/                # 启动相关
├── tests/                # 集成测试
├── tools/                # 工具
└── user/                 # 用户程序
```

### 注意
- 一定不要修改 `chaos/kernel/src/kernel.rs`
- 对 `kernel-sim` 相关问题，修改目标是 `chaos/kernel-sim/`，不要改 `chaos/kernel/`
- 修改后要保留 AI 对话日志作为提交材料
- 代码中需要标注 `// HUMAN` 和 `// AGENT` 区分人写/AI 生成，注意 `// AGENT` 不能只写在一个文件开头，而是在每个修改的函数或结构体等块结构前写上 `// AGENT` 以及修改的内容

### 长任务交接
- 如果任务很长、对话上下文即将不足，或继续在同一对话中追加会降低稳定性，Codex 必须先更新 handoff summary / 当前状态总结。
- 总结应写入 `TASK.md`、`NOTES.md`，或对应的 issue/comment；优先使用当前任务已经在维护的文件，没有则创建或补充 `TASK.md`。
- 每次 github 更新后应当把进度更新到 `docs/ai-record.md` 中。
- handoff summary 至少包括：
  - 目标：当前要完成什么，成功标准是什么
  - 已完成修改：已经改了哪些行为、接口或实现
  - 关键文件：涉及的源码、测试、配置、文档路径
  - 测试结果：已运行命令、通过/失败情况、关键失败日志
  - 未解决问题：剩余 bug、风险、下一步排查方向
  - 不要改的部分：例如 `chaos/kernel/src/kernel.rs` 等明确禁止修改的位置
- 写完总结后，应把当前 `git diff` / `git diff --stat`、最新测试命令与结果一起交给新对话继续，不要在一个超长对话里无期限追加。

---

## 环境与工具备忘

### Rust 2024 edition 注意
- `extern "C"` → `unsafe extern "C"`
- bin 中需 `use user_lib::func;`（`#[macro_use]` 只对宏生效）

### 测试仓库
- `rcore_tutorial_tests`：独立仓库，Python 自动化测试
- WSL2 下需用清华/ustc 镜像源
- 旧 nightly 需降级 spin 到 0.7

### QEMU virt 平台
- mtime 频率 10MHz（`RISCV_ACLINT_DEFAULT_TIMEBASE_FREQ`）
- `TICKS_PER_SEC=100` → 10ms 中断间隔（改 1000 可见明显并发）
