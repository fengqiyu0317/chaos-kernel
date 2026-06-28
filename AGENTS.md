# rCore-Tutorial & Chaos 学习记录

---

## Chaos 项目

### 概述
- **仓库**: `chaos/` — 基于 rCore 的内核调试与重写作业
- **当前目标目录**: `kernel-sim/`
- **任务**: 修 bug → 通过全部测试 → 重写提升代码质量
- **M9 迁移目标**: 以 `docs/kernel-sim-qemu-migration-design.md` 为准，把 `kernel-sim` 已稳定的内核语义迁移到 QEMU 裸机环境，而不是重新设计一套新内核

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
├── kernel-sim/           # 当前主要修改目标：host userspace 内核模拟器
├── kernel-qemu/          # M9 新增目标：RISC-V QEMU no_std 承载层
├── kernel-common/        # 可选共享 crate：仅放 no_std/alloc 纯逻辑
├── docs/                 # 设计文档、AI 记录和交接材料
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

### M9：kernel-sim 到 QEMU 裸机迁移

- 迁移设计以 `docs/kernel-sim-qemu-migration-design.md` 为准；如果设计文档和临时想法冲突，先更新设计文档或在 `TASK.md` 记录决策。
- 核心目标是迁移 `kernel-sim` 的现有语义：进程、地址空间、ELF/exec、fd/open-file-description、exit/wait、timer、pipe/epoll、同步等待等。
- `kernel-qemu/` 只负责提供 QEMU/RISC-V 必需的启动、trap、页表、timer、SBI/UART、设备 I/O 和调度承载层；不要把它当作新的业务语义来源。
- 不要以“从零写一个更像 rCore 的内核”为目标。新增裸机代码必须能说明服务于哪个 `kernel-sim` 语义迁移点。
- 不删除或替换 `kernel-sim/`；host 端 `cargo test` 和 `kernel-sim/tests/smoke.rs` 仍是语义回归基准。
- 不把 `chaos-tests` 直接当作 QEMU 迁移判定标准，除非任务明确要求接入该测试体系。
- 不把旧 `kernel/` 当作 M9 迁移的直接修改目标，除非用户明确改变边界。
- 每一层迁移前先标清：
  - `kernel-sim` 中的语义源文件或现有测试。
  - QEMU 侧必须替换的 host 依赖。
  - 可以抽到 `kernel-common/` 的 no_std/alloc 纯逻辑。
  - 必须留在 `kernel-qemu/` 的裸机适配代码。
- `kernel-common/` 只能放不依赖 `std`、host 线程、host 锁、host 文件系统的代码，例如 syscall 常量、ELF 解析结构、地址对齐 helper、纯数据结构和部分错误码定义。
- 暂时不要共享 `KernelRuntimeTicker`、host lock、host thread、host time、基于 `Arc<Mutex<Vec<u8>>>` 的模拟地址空间或测试专用 helper。
- RISC-V syscall 层只做 ABI 适配：从 `a7` / `a0..a5` 解码到迁移后的 `kernel-sim` syscall 语义入口，返回值写回 `a0`；不要在 trap 层重新定义 syscall 行为。
- timer tick 在 QEMU 中由真实 timer interrupt 推进，不再依赖 host 后台线程。
- fd/文件层先迁移 `kernel-sim` 的 fd table、open-file-description、dup/dup2/fcntl、cloexec、pipe readiness 和 epoll ready/wait 语义；不要先扩展完整文件系统、网络或 virtio-blk。
- 每完成一个 M9 里程碑，需要记录 host 语义回归和 QEMU smoke 结果；新增 QEMU 行为时必须说明对应的 `kernel-sim` 语义是否已经存在，不存在则进入 TODO。

### 长任务交接
- 如果任务很长、对话上下文即将不足，或继续在同一对话中追加会降低稳定性，Codex 必须先更新 handoff summary / 当前状态总结。
- 总结应写入 `TASK.md`、`NOTES.md`，或对应的 issue/comment；优先使用当前任务已经在维护的文件，没有则创建或补充 `TASK.md`。
- 每次更新到 github 仓库后应当把进度更新到 `docs/ai-record.md` 中。
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
