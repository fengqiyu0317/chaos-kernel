# kernel-sim 到 QEMU 裸机内核的迁移设计

更新时间：2026-06-28

## 目标

本文档用于明确 M9 任务的迁移边界和第一阶段实现路线。核心目标是把 `kernel-sim` 已经稳定下来的内核语义迁移到 QEMU 裸机环境，而不是重新设计一套新内核。由于 `kernel-sim` 当前强依赖 host `std`、host 线程、host 锁和模拟地址空间，迁移不能通过简单改 target 完成；`kernel-qemu` 的职责是提供 RISC-V/QEMU 必需的启动、trap、页表、时钟和设备适配层，让 `kernel-sim` 的进程、内存、文件描述符、exec、wait、pipe/epoll 等语义逐步落到真实裸机运行时上。

第一阶段成功标准：

- `kernel-sim/` 仍可在宿主环境中通过 `cargo test` 和 `kernel-sim/tests/smoke.rs` 做语义回归。
- 新的 QEMU 内核路径可以用 `riscv64gc-unknown-none-elf` 构建。
- QEMU `virt` 启动后能通过 SBI/UART 输出启动日志。
- 能处理 timer trap，并用真实中断推进内核逻辑 tick。
- 能启动一个内嵌 init 用户程序，至少打通 `write`、`exit`、`getpid`、`read` 中的最小子集。
- 用户程序 `write` 输出后 `exit`，内核可以关机或进入明确的 idle 状态。

## 非目标

- 不修改 `chaos/kernel/src/kernel.rs`。
- 不删除或替换 `kernel-sim/`。
- 不把 `chaos-tests` 直接当作 QEMU 移植的判定标准，除非后续明确接入该测试体系。
- 不要求第一阶段实现完整文件系统、网络、virtio-blk、TTY、完整 signal、完整 epoll 或真实 Linux 兼容 ABI。
- 不以“从零写一个更像 rCore 的内核”为目标；新增裸机代码必须服务于承载和迁移 `kernel-sim` 语义。
- 不在第一阶段追求把 `kernel-sim` 所有 `std` 依赖一次性抽象干净。

## 当前边界

`kernel-sim` 当前是 userspace 模拟器。它依赖：

- `std::sync::{Arc, Mutex, RwLock, Condvar}`。
- `std::thread` 和 host thread 的 park/unpark。
- `thread_local!` 保存宿主线程本地状态。
- `std::time::{Duration, Instant}`。
- `KernelRuntimeTicker` 后台宿主线程推进逻辑 tick。
- `AddrSpace` 中的模拟用户页内容，例如以宿主堆内存保存页面数据。
- `cargo test` 作为主要验证入口。

这些能力在裸机 `no_std` 环境中不存在。因此迁移时需要保留 `kernel-sim` 作为语义源和回归基准，同时为 QEMU 新建独立运行时底座。这个底座不是新内核的业务语义来源，而是把真实 RISC-V trap、页表、timer、设备 I/O 映射到 `kernel-sim` 现有语义所需接口上的适配层。

## 建议目录

建议新增独立目录，避免混入既有 `kernel/` 和 `kernel-sim/`：

```text
chaos/
├── kernel-sim/             # 保留：host userspace 模拟器和 cargo test
├── kernel-qemu/            # 新增：RISC-V QEMU no_std 内核壳
│   ├── Cargo.toml
│   ├── .cargo/config.toml
│   ├── linker-qemu.ld
│   ├── src/
│   │   ├── entry.S
│   │   ├── main.rs
│   │   ├── arch/riscv64/
│   │   ├── trap/
│   │   ├── mm/
│   │   ├── proc/
│   │   ├── syscall/
│   │   ├── fs/
│   │   └── drivers/
│   └── tests/
└── tools/
    └── qemu-smoke.sh       # 可选：自动化 QEMU smoke
```

如果后续出现足够稳定的可共享代码，再考虑增加共享 crate：

```text
chaos/kernel-common/
```

`kernel-common` 只应放不依赖 `std`、宿主线程、宿主锁、host 文件系统的代码，例如 syscall 常量、ELF 解析结构、地址对齐 helper、纯数据结构和部分错误码定义。不要为了过早复用而把 `kernel-sim` 的 host runtime 抽进共享层。

## 分层设计

分层时先按“从 `kernel-sim` 迁移什么语义”划线，再决定 QEMU 侧需要补哪些硬件适配。每一层都应标清：

- `kernel-sim` 中的语义源文件或现有测试。
- QEMU 侧必须替换的 host 依赖。
- 可以直接抽取到 `kernel-common` 的纯逻辑。
- 暂时只能重新实现的裸机适配代码。

### 1. 裸机启动适配层

这一层不对应 `kernel-sim` 的业务语义，只负责让后续迁移代码能在 QEMU 上运行。`kernel-qemu` 第一阶段需要自带最小启动骨架：

- `#![no_std]` 和 `#![no_main]`。
- `entry.S` 设置启动栈并跳入 `rust_main(hartid, dtb_pa)`。
- `linker-qemu.ld` 把内核放到 QEMU `virt` 常见入口地址。
- panic handler 通过 SBI/UART 打印 panic 信息。
- SBI console putchar 和 shutdown。
- 清零 `.bss`，初始化内核堆，初始化日志输出。

建议 smoke 命令形式：

```bash
qemu-system-riscv64 \
  -machine virt \
  -nographic \
  -bios default \
  -kernel target/riscv64gc-unknown-none-elf/release/kernel-qemu
```

具体路径以后以实际构建产物为准。

### 2. trap/syscall ABI 适配层

`kernel-sim` 的 syscall 当前由测试或模拟运行时直接进入 Rust 函数；QEMU 侧必须建立真实 trap 入口，但 syscall 的行为语义应尽量向 `kernel-sim/src/kernel/syscall/` 靠拢，而不是在 trap 层重新定义一套规则。

- 设置 `stvec`。
- 定义 RISC-V trap frame，保存和恢复通用寄存器。
- 处理 user `ecall`。
- 处理 timer interrupt。
- 处理 page fault、非法指令等异常，并在早期以清晰日志失败。
- syscall 返回前写回 `a0`。
- user `ecall` 返回前推进 `sepc`。
- timer interrupt 驱动调度 tick，例如调用裸机路径下的 tick 处理。

`kernel-sim` 当前 syscall 编号更接近 x86_64/Linux 风格。QEMU RISC-V 用户程序通常使用 `a7` 传 syscall number，参数位于 `a0` 到 `a5`。因此需要新增 arch-specific syscall 映射层，把 RISC-V ABI 转成 `kernel-sim` 语义入口能理解的调用形式：

```text
RISC-V trap frame
  -> syscall number/argument mapper
  -> migrated kernel-sim syscall semantic layer
  -> arch return value writer
```

第一阶段只映射最小集合：`write`、`exit`、`getpid`、`read`。

### 3. 内存语义迁移层

迁移目标不是丢掉 `kernel-sim` 的地址空间语义，而是把它从“宿主堆模拟页面”换成“真实物理页 + Sv39 页表”。QEMU 裸机内核不能继续使用 `AddrSpace` 中的宿主堆页面模型，因此需要替换底层存储和翻译机制：

- 从 linker symbols、设备树或固定 QEMU `virt` 内存布局中确定可用物理内存范围。
- 初始化 frame allocator。
- 建立 Sv39 kernel page table。
- 映射 kernel text、rodata、data、bss、boot stack、内核堆。
- 建立 trampoline/trap 上下文映射。
- 为用户进程建立独立用户页表。
- `copy_from_user` / `copy_to_user` 通过页表翻译和权限检查访问用户内存。

优先迁移或保留的 `kernel-sim` 语义包括：

- VMA / `AddrSpace` 的区域管理、权限检查和映射生命周期。
- `fork` / COW 的可观察行为。
- `mmap` / `munmap` / `brk` 的返回值、错误处理和 frame 回收语义。
- `exec` 装载 ELF、建立用户栈、替换地址空间的事务性语义。

可复用的部分应限于：

- ELF `PT_LOAD` 解析思路。
- 用户栈 `argc/argv/envp/auxv` 的布局思路。
- 地址对齐、页权限标志等纯逻辑。

必须替换的部分：

- `Arc<Mutex<Vec<u8>>>` 这类模拟页面内容。
- host `FramePool` 中只服务模拟器的资源统计逻辑。
- 任何依赖 host lock 来保护真实页表状态的路径。

### 4. 调度、同步和等待语义迁移层

这一层要迁移 `kernel-sim` 中已经形成的 `TaskRunState`、wait token、timer target、futex、pipe/epoll 唤醒等可观察语义；QEMU 侧替换的是 host thread 承载方式，而不是重新发明等待规则。`kernel-sim` 的等待模型建立在 host thread 上，而 QEMU 裸机路径需要 task 调度模型：

- `std::thread::park()` / `unpark()` 替换为修改 `TaskRunState` 和 run queue。
- `Condvar` 替换为内核 wait queue。
- `thread_local!` current task 替换为 CPU-local 或 per-hart current task。
- `Instant` / `Duration` 替换为 timer interrupt 和内核 tick/deadline。
- `KernelRuntimeTicker` 不进入裸机路径，逻辑时间由真实 timer interrupt 推进。
- 自旋锁需要明确是否关中断，必要时提供 irqsave/irqrestore 版本。

第一阶段可以先实现单核、不可抢占或弱抢占模型，目标是让 `kernel-sim` 的阻塞/唤醒状态转换能在 QEMU 上被观察到。后续再补公平性、多核、抢占、中断嵌套和锁顺序约束。

### 5. 进程模型迁移层

这一层的语义源应来自 `kernel-sim/src/kernel/proc/` 和现有 smoke 测试。第一阶段只需要把最小 task 路径跑通：

- idle task。
- init user task。
- run queue。
- 当前 task 指针。
- trap 返回用户态。
- `exit` 将当前 task 置为 exited。

后续按 `kernel-sim` 现有能力逐步迁移：

- `fork` / `clone`。
- `exec`。
- `wait4`。
- futex。
- pipe / epoll wait。
- signal。

迁移顺序应从 trap 返回用户态、`exit`、`wait4`、`fork/clone` 的可观察行为开始；每迁移一个行为，都应能指出对应的 `kernel-sim` 语义源和回归测试。

### 6. fd、文件和设备语义迁移层

这一层不应先做完整文件系统，而应先迁移 `kernel-sim` 已有的 fd table、open-file-description、pipe 和 epoll ready/wait 语义。第一阶段建议只实现最小字符设备作为 fd 后端：

- fd `1` / `2` 写到 SBI console 或 UART。
- fd `0` 可以先返回 EOF 或阻塞占位，按测试需求决定。
- `/bin/init` 先以内嵌 ELF 或 initramfs 提供。

短期文件系统路线：

- 内存 `FileNode` 或 initramfs 支撑 init。
- 迁移 `kernel-sim` 的 fd/open-file-description 共享 offset、cloexec、dup/dup2/fcntl 等语义。
- 后续把 pipe readiness、epoll ready list、waiter 唤醒路径接到 QEMU 调度等待队列。

长期路线：

- virtio-blk。
- 目录和路径解析。
- 设备文件和 TTY。
- 更完整的权限和 credential 检查。

## 阶段计划

### Milestone 0：建立迁移清单和语义基线

产物：

- 本设计文档。
- `TASK.md` 保留 M9 TODO。
- 不改 `kernel-sim` 行为。
- 列出第一批迁移对象及对应源码/测试：syscall 最小集、`Task`/`ProcessState`、`AddrSpace`/ELF/user stack、fd table、timer tick。

验证：

```bash
cd kernel-sim
cargo test
```

成功标准：

- `kernel-sim` 原有测试继续通过。
- 每个第一批迁移对象都有明确的 `kernel-sim` 语义源，不把 QEMU 侧实现当作新的语义源。

### Milestone 1：QEMU 最小承载层

产物：

- `kernel-qemu/` 新 crate。
- `entry.S`、`linker-qemu.ld`、panic handler、SBI console、SBI shutdown。
- `tools/qemu-smoke.sh` 或等价命令记录。

成功标准：

- QEMU 启动后打印固定 banner。
- panic 能输出信息。
- shutdown 可结束 QEMU。
- 该阶段只建立承载环境，不引入与 `kernel-sim` 冲突的进程、文件或内存语义。

### Milestone 2：trap/timer 接入迁移接口

产物：

- RISC-V trap frame。
- `stvec` 初始化。
- timer interrupt 处理。
- tick 计数日志或可测试计数器。
- RISC-V syscall 参数解码后进入“待迁移 syscall 语义入口”，而不是直接在 trap 层写死业务逻辑。

成功标准：

- QEMU 中可观察到 timer interrupt 生效。
- tick 不依赖 host 后台线程。
- timer tick 能对接后续 `kernel-sim` 等待/超时语义所需的 deadline 或 wakeup 接口。

### Milestone 3：真实页表承载 `kernel-sim` 地址空间语义

产物：

- frame allocator。
- Sv39 kernel page table。
- 内核地址空间映射。
- 基础 `copy_from_user` / `copy_to_user`。
- `AddrSpace` 语义适配方案：把模拟页面内容替换为真实 frame 和页表映射，同时保留 VMA 权限、映射生命周期、错误返回和回收语义。

成功标准：

- 内核能在分页开启后继续输出。
- 非法用户地址能被拒绝或触发可诊断 fault。
- 能说明 `kernel-sim` 中哪些地址空间行为已迁移，哪些仍停留在 host 模拟器。

### Milestone 4：迁移第一个用户进程路径

产物：

- 内嵌 init ELF。
- 用户地址空间。
- 用户栈。
- `sret` 进入用户态。
- user `ecall` 返回内核。
- 复用或对齐 `kernel-sim` 的 ELF `PT_LOAD`、用户栈初始化、pid/task 初始化和 `exec` 地址空间替换语义。

成功标准：

- init 可以执行到 `ecall`。
- 内核能识别 syscall number 和参数。
- init 的建立过程能映射回 `kernel-sim` 中 `proc_init` / `exec` / 用户栈相关语义。

### Milestone 5：迁移最小 syscall 语义

产物：

- RISC-V syscall number 映射层。
- `write` 到 SBI/UART。
- `exit` 结束 init。
- `getpid` 返回固定或真实 pid。
- `read` 的 EOF 或最小输入语义。
- 每个 syscall 都标出对应的 `kernel-sim` syscall 语义源；QEMU 侧只替换用户指针访问、fd 后端和返回寄存器写回。

成功标准：

- init 输出一行文本。
- init 调用 `exit` 后内核关机或进入 idle。
- `write`、`exit`、`getpid`、`read` 的返回值和错误处理不自行发明，能与 `kernel-sim` 的最小语义对照。

### Milestone 6：双路径语义回归体系

产物：

- 保留 `kernel-sim` 的 `cargo test`。
- 新增 QEMU smoke 脚本。
- 文档记录每次 QEMU 命令、输出摘要和限制。
- 为每个已迁移行为建立“host smoke 测试 / QEMU smoke 输出”的对照表。

成功标准：

- host 语义回归和 QEMU smoke 可以分别运行。
- 两条路径失败时能判断是模拟器语义问题还是裸机 runtime 问题。
- 新增 QEMU 行为时，必须先说明对应的 `kernel-sim` 语义是否已经存在；不存在时进入 TODO，而不是顺手设计一套新语义。

## 共享代码策略

优先复用 `kernel-sim` 的语义和可抽取代码，谨慎复用具体实现。

可以考虑共享：

- ELF header / program header 解析中的纯解析逻辑。
- 页大小、权限 bit、对齐 helper。
- syscall 错误码和部分常量。
- 用户栈布局算法。
- fd table 的抽象接口设计。

暂时不要共享：

- 任何直接使用 `std` 的模块。
- host lock、host thread、host time 相关代码。
- `KernelRuntimeTicker`。
- 基于 `Arc<Mutex<Vec<u8>>>` 的模拟地址空间。
- 测试专用 helper。

当某段逻辑需要共享时，先满足三个条件：

- 可以在 `#![no_std]` 下编译，最多依赖 `alloc`。
- 不假设 host thread 或 host filesystem。
- 在 `kernel-sim` 和 `kernel-qemu` 两侧都有明确调用点和测试价值。

## 测试策略

host 端继续运行：

```bash
cd kernel-sim
cargo test
```

QEMU 端新增 smoke，初始检查：

- 启动 banner。
- timer trap 日志或计数。
- init 输出。
- init exit 后 shutdown。

后续可以把 QEMU 输出写入日志文件并用脚本匹配关键行，但第一阶段不要把测试脚本做得比内核本身复杂。

## 风险和决策

- 最大风险是过早复用 `kernel-sim` 代码，导致 `std` 依赖渗入裸机路径。
- 第二个风险是把 syscall 语义和 RISC-V ABI 混在一起，导致后续难以同时维护 host 模拟器和 QEMU 内核。
- 第三个风险是先做文件系统、网络或完整 epoll，绕过了更基础的 trap、页表、调度和用户态返回路径。
- 初期应接受功能少，但每个里程碑都必须能启动、能观察、能回归。

## 禁止修改范围

- 不修改 `chaos/kernel/src/kernel.rs`。
- 不删除 `kernel-sim/` 的现有测试。
- 不把 `kernel-sim` 的 host 测试路径改成依赖 QEMU。
- 不把旧 `kernel/` 当作 M9 迁移的直接修改目标，除非后续任务明确改变边界。

## 交接记录要求

每完成一个 M9 里程碑，应同步更新 `TASK.md` 或 `docs/ai-record.md`，至少记录：

- 目标。
- 已完成修改。
- 关键文件。
- QEMU 命令。
- host 测试结果。
- QEMU smoke 结果。
- 未解决问题。
- 禁止修改范围。
