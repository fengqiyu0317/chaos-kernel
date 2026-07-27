# kernel-sim 到 QEMU 裸机内核的迁移设计

更新时间：2026-07-23

## 目标

本文档用于明确 M9 任务的迁移边界和第一阶段实现路线。核心目标是把 `kernel-sim` 已经稳定下来的内核语义迁移到 QEMU 裸机环境，而不是重新设计一套新内核。迁移策略采用 source-first：每个子系统先直接迁入 `kernel-sim` 的现有代码作为基线，保留结构体、函数名、控制流和错误语义，再在这个基线上替换 host `std`、host 线程、host 锁和模拟地址空间等裸机不可用依赖。`kernel-qemu` 的职责是提供 RISC-V/QEMU 必需的启动、trap、页表、时钟和设备适配层，让迁入的 `kernel-sim` 语义逐步落到真实裸机运行时上。

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
- 不为了提前获得干净的 QEMU 原生结构而先写一套空骨架替代 `kernel-sim` 现有实现。

## 当前边界

`kernel-sim` 当前是 userspace 模拟器。它依赖：

- `std::sync::{Arc, Mutex, RwLock, Condvar}`。
- `std::thread` 和 host thread 的 park/unpark。
- `thread_local!` 保存宿主线程本地状态。
- `std::time::{Duration, Instant}`。
- `KernelRuntimeTicker` 后台宿主线程推进逻辑 tick。
- `AddrSpace` 中的模拟用户页内容，例如以宿主堆内存保存页面数据。
- `cargo test` 作为主要验证入口。

这些能力在裸机 `no_std` 环境中不存在。因此迁移时需要保留 `kernel-sim` 作为语义源和回归基准，同时先把 `kernel-sim` 代码迁入 QEMU 侧形成可审查的移植基线，再逐步替换裸机不可用依赖。QEMU 底座不是新内核的业务语义来源，而是把真实 RISC-V trap、页表、timer、设备 I/O 映射到迁入的 `kernel-sim` 语义所需接口上的适配层。

## 迁移策略

迁移执行顺序以“先迁入、再替换”为准：

1. 每个子系统先从 `kernel-sim/src/kernel/` 复制对应源码到 `kernel-qemu/src/` 的同名或明确标注的迁移模块中。复制后应尽量保留原有类型名、函数名、错误返回、TODO 和关键控制流，避免先写只有相似名字的新实现。
2. 复制完成后做 host 依赖清单，逐项标出哪些依赖必须替换，例如 `std::sync`、host thread、`thread_local!`、`Instant`、host 文件对象、模拟页面 `Arc<Mutex<Vec<u8>>>`。
3. 先让迁入代码在 QEMU crate 中可审查，再分批接入编译。暂时不能编译或暂时不能从 `main.rs` 注册的批次可以先隔离在未注册模块或显式 `cfg` 下，但隔离不是废弃；这些已迁入文件就是后续修改的主要位置。
4. 修改迁入代码时应尽可能在原有结构体、函数和控制流内部替换实现，不因为当前接不上裸机入口就另写一套“更干净”的 QEMU 空骨架。缺少 heap、sync、frame allocator、Sv39、usercopy 等承载时，先补窄适配层或 TODO，再回到已迁入代码中替换对应 host 依赖。
5. 每个阶段允许出现“代码已迁入但尚未可编译 / 尚未接入运行路径”的中间状态；只要记录清楚未接入原因、缺失前置依赖和下一步替换点即可。目标是改完整后自然接上，而不是用并行新实现绕开迁入代码。
6. QEMU 新写代码只承担硬件适配和 ABI 适配，例如启动、trap、CSR、SBI/UART、页表写入、timer 中断、用户指针翻译。不得在这些适配层重新定义 syscall、fd、进程、等待或地址空间业务语义。
7. 每次迁移都要能回答：源文件来自 `kernel-sim` 哪里，迁入后改了哪些 host 依赖，对应的 host 回归测试或 smoke 语义是什么，QEMU 侧当前只验证到哪一步。

### 功能代码块级迁移工作流

后续实现改为按用户指定的 `kernel-sim` 功能代码块逐步推进。这里的“代码块”可以是一个结构体、函数、impl 方法组，或一个明确的 syscall / MM / fd 子路径。除非用户明确扩大范围，每次只处理当前指定代码块，不顺手重构相邻 TODO 或提前迁移整个子系统。

每个代码块按下面顺序处理：

1. 标出源代码块，例如 `kernel-sim/src/kernel/mm/alloc.rs::FramePool`、`AddrSpace::resize_brk()` 或 `sys_brk()`。
2. 说明该块当前在 `kernel-sim` 中提供的语义、直接调用者和依赖的下游入口。
3. 列出该块不能直接进入 QEMU 的 host 依赖，例如 `std`、host lock、host thread、模拟页 `Arc<Mutex<Vec<u8>>>`、模拟地址偏移或测试 helper。
4. 只在当前代码块或它必须接触的最小适配边界内修改；如果发现需要先补 `heap`、`sync`、`FramePool`、Sv39 或 usercopy 前置能力，先记录阻塞点和下一块建议，不临时写一套平行实现绕过。
5. 修改后记录验证边界：至少说明 `kernel-qemu` 是否能 build / smoke；如果触及 `kernel-sim` 共享语义，再运行对应 host 回归。
6. 交接记录要写清“本轮只处理了哪个代码块”和“明确未处理哪些相邻功能”，避免后续误以为整个子系统已经完成。

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
│   │   ├── mm/             # 先迁入 kernel-sim/src/kernel/mm，再替换真实 frame/Sv39
│   │   ├── proc/           # 先迁入 kernel-sim/src/kernel/proc，再替换 host task/thread 承载
│   │   ├── syscall/        # 先迁入 kernel-sim/src/kernel/syscall，再接 RISC-V ABI 映射
│   │   ├── fs/             # 先迁入 kernel-sim/src/kernel/fs，再替换 host fd 后端
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

分层时先按“从 `kernel-sim` 迁移什么语义”划线，再决定 QEMU 侧需要补哪些硬件适配。每一层都应先迁入对应 `kernel-sim` 源码，再在此基础上替换裸机不可用依赖。每一层都应标清：

- `kernel-sim` 中的语义源文件或现有测试。
- 迁入到 `kernel-qemu` 后保留了哪些原有类型、函数和错误语义。
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

#### M3 可执行步骤

这一阶段的第一目标是把 `kernel-sim` 的地址空间实现迁入 `kernel-qemu`，然后在迁入代码上把“宿主堆模拟页面”替换为“真实物理页 + Sv39 页表”。不要先写一个只有同名接口的 QEMU 原生空骨架，也不要一开始迁移完整文件系统、file-backed `mmap` 或完整 COW。下面顺序是总体依赖路线；实际执行仍按用户指定的功能代码块逐块推进。每一步都应尽量保持可构建、可 smoke；确实暂时不能编译或不能接入 `main.rs` 的复制批次要明确隔离、继续在原迁入文件上替换依赖，并在前置 heap/sync/frame/Sv39/usercopy 补齐后再接入。

1. 直接迁入 `kernel-sim` MM 源码：
   - 以 `kernel-sim/src/kernel/mm/mod.rs`、`address_space.rs`、`alloc.rs`、`bits.rs`、`memory.rs` 为源。
   - 迁入到 `kernel-qemu/src/mm/` 的对应文件中，必要时拆出 `sv39.rs`、`usercopy.rs` 作为裸机适配文件。
   - 保留 `AddrSpace`、`VmRegion`、`VmMap`、`FramePool`、`map_region()`、`unmap_range()`、`protect()`、`read_user_bytes()`、`write_user_bytes()` 等语义入口；`kernel-sim` 的 `PageTableEntry` 映射所有权语义在 QEMU 侧由 `SharedPage` 承载。它按值保存映射级 `cow` 状态，克隆时只共享内部 `PgFrame`；`ShmSegment` 中的 wrapper 始终保持非 COW。后续在这些入口内部替换实现。
   - 在 `kernel-qemu/src/main.rs` 注册 `mod mm;` 的时机以可构建为准；不能立即构建的迁入文件必须有明确 TODO 和下一步替换清单，但仍优先在这些迁入文件里完成 `std`、host lock、模拟页面和 host frame 依赖替换。

2. 对迁入代码做 host 依赖清单：
   - `std::collections` 可迁到 `alloc::collections`。
   - `Vec`、`String`、`Box`、`BTreeMap` 等可迁到 `alloc`。
   - QEMU 全局堆只作为 `alloc` crate 的早期承载入口；实现时应尽量复用或改造已迁入 `kernel-qemu/src/mm/alloc.rs` 中的 `heap_init()`、`heap_grow()`、`KStk` 等边界，不另写一套与迁入 `FramePool` 平行的 MM 语义。
   - `Arc<Mutex<Vec<u8>>>` 只能作为原语义参照，必须替换为真实 frame/PPN、页表项和必要 backing metadata。
   - `std::sync::Mutex`、host `FramePool`、host 文件对象和测试 helper 不能原样进入最终 QEMU 路径。
   - 对每个替换点记录“保留的 `kernel-sim` 语义”和“QEMU 侧替换的承载机制”。

3. 在迁入的 `FramePool` 基础上替换物理页分配：
   - 保留 `alloc_page()` / `dealloc_page()` / `free_count()` 或等价语义入口，不另起一套无法对照的 API。
   - 从 linker symbol `ekernel` 得到内核镜像结束地址。
   - 第一版可以固定 QEMU `virt` 内存布局，配合 `tools/qemu-smoke.sh` 显式使用 `-m 128M`。
   - RAM 范围先按 `0x8000_0000..0x8800_0000` 处理，空闲页起点为 `align_up(ekernel, PAGE_SIZE)`。
   - 分配器最终返回页对齐物理地址或 PPN；原 `Mutex<Vec<bool>>` 只能作为迁移起点，不能作为裸机最终实现。

4. 在迁入的 `SharedPage` / `AddrSpace` 基础上接入 Sv39：
   - 新增或迁入后改造 PTE flags：`V/R/W/X/U/G/A/D`。
   - 在保留 `map_region()`、`unmap_range()`、`protect()` 等入口的前提下，把内部页记录替换为 Sv39 page table walk、map、unmap、translate。
   - 先采用恒等映射 `VA == PA`，降低开启分页后的地址切换风险。
   - 根据 linker symbols 映射 kernel text、rodata、data、bss、boot stack 和 frame allocator metadata。
   - 页表页本身从 frame allocator 分配，并清零后再作为下级页表使用。
   - 后续 TODO 分两阶段收敛低地址映射：先把整段 RAM 恒等映射收缩到低链接内核镜像和必要启动资源，并强制动态物理页只走高半区 direct map；再在高半区 VMA 链接、分页后高地址跳转、`sp` / `gp` / `stvec` / 上下文和 trap handler 函数指针迁移、linker 符号物理地址换算全部完成后，取消内核根页表的低地址 RAM 恒等映射。

5. 开启分页并保持早期 smoke 可观察：
   - 在 `kernel-qemu/src/csr.rs` 增加 `satp` 写入和 `sfence.vma` helper。
   - 在 `rust_main()` 中按顺序执行：清 BSS、初始化 trap/timer、初始化迁入后改造的 frame allocator、建立 kernel page table、写入 Sv39 `satp`、执行 `sfence.vma`、再次打印分页已开启日志。
   - `tools/qemu-smoke.sh` 增加对分页后日志的匹配，防止只验证到分页前。
   - 成功标准是开启分页后仍能输出 boot、timer tick 和 shutdown 日志。

6. 迁入并改造用户拷贝语义：
   - 以 `kernel-sim` 的 `read_user_bytes()`、`write_user_bytes()`、`readable_user_prefix_len()`、`writable_user_prefix_len()` 为语义源。
   - QEMU 侧实现 `copy_from_user()` / `copy_to_user()` 时仍遵守这些入口的错误返回、短前缀检查和跨页行为。
   - 具体访问必须逐页翻译，检查 `U/R/W` 权限，并正确处理跨页 buffer。
   - 非法地址、未映射页或权限不符应返回可映射到 `EFAULT` 的错误，而不是直接 panic。
   - 将 `kernel-qemu/src/semantics.rs` 中直接解引用用户指针的 `write` 临时实现改为调用迁入后改造的用户拷贝入口。

7. 保留 `kernel-sim` 的 VMA 生命周期语义：
   - QEMU 版页表项只记录真实 frame/PPN、权限、present/COW 状态和必要 backing metadata，不能保存 `Arc<Mutex<Vec<u8>>>` 页面内容。
   - `map_region()`、`unmap_range()`、`protect()`、`release_all_pages()` 的外部语义应与迁入前保持可对照。
   - `unmap_range()` 必须释放真实 frame，并保留 `kernel-sim` 已经稳定的语义：无效范围先校验，失败时避免半更新，成功时返回已解除映射页数或等价诊断信息。

8. 对齐 `brk` 和用户栈的最小语义：
   - `brk` 第一版可以保持 `kernel-sim` 现有页粒度模型：增长时映射匿名页，收缩时走 `unmap_range()` 回收 frame。
   - 需要在 TODO 中明确：byte-granular program break、`start_brk/min_brk`、lazy allocation 仍不是第一版目标。
   - 用户栈先用匿名页映射，并复用 `kernel-sim` 已验证过的 `argc/argv/envp/auxv` 布局思路；真正进入用户态放到 Milestone 4。

9. 暂缓 file-backed `mmap` 和完整 COW，但保留迁入位置：
   - 当前 `kernel-sim` 的 file-backed `mmap` 依赖 host 文件对象和 `Arc<Mutex<Vec<u8>>>` backing，M3 不完成最终裸机替换。
   - M3 迁入相关结构和 backing metadata 位置，实际文件页读入、`MAP_SHARED` 写回和 fd 权限检查放到文件层迁移时完成。
   - COW 第一阶段迁入现有语义并设计 frame refcount 和只读 PTE fault 路径；完整 `fork` 行为可以在用户进程和调度路径具备后继续迁移。

10. 每一步验证和记录：
   - `cd kernel-qemu && cargo fmt --check && cargo build --release`。
   - `bash tools/qemu-smoke.sh`。
   - `git diff --check -- kernel-qemu tools/qemu-smoke.sh docs/kernel-sim-qemu-migration-design.md TASK.md docs/ai-record.md`。
   - 如本阶段改动影响共享语义说明，还要运行 `cd kernel-sim && cargo test`，确保 host 端语义基准未被破坏。
   - 每完成一个小闭环，把“已迁移的 kernel-sim 语义、仍停留在 host 模拟器的语义、QEMU smoke 结果”记录到 `TASK.md` 或 `docs/ai-record.md`。

### 4. 调度、同步和等待语义迁移层

这一层先迁入 `kernel-sim/src/kernel/proc/`、`kernel-sim/src/kernel/core/sync.rs` 以及相关等待/唤醒代码，再替换 host thread 承载。要保留 `TaskRunState`、wait token、timer target、futex、pipe/epoll 唤醒等可观察语义；QEMU 侧替换的是运行方式，而不是重新发明等待规则。`kernel-sim` 的等待模型建立在 host thread 上，而 QEMU 裸机路径需要 task 调度模型：

- `std::thread::park()` / `unpark()` 替换为修改 `TaskRunState` 和 run queue。
- `Condvar` 替换为内核 wait queue。
- `thread_local!` current task 替换为 CPU-local 或 per-hart current task。
- `Instant` / `Duration` 替换为 timer interrupt 和内核 tick/deadline。
- `KernelRuntimeTicker` 不进入裸机路径，逻辑时间由真实 timer interrupt 推进。
- 自旋锁需要明确是否关中断，必要时提供 irqsave/irqrestore 版本。

第一阶段可以先实现单核、不可抢占或弱抢占模型，目标是让迁入的 `kernel-sim` 阻塞/唤醒状态转换能在 QEMU 上被观察到。后续再补公平性、多核、抢占、中断嵌套和锁顺序约束。

### 5. 进程模型迁移层

这一层先直接迁入 `kernel-sim/src/kernel/proc/` 的进程、task、wait、resource、signal 相关代码，再在迁入代码上替换 host runtime。第一阶段只需要把最小 task 路径跑通：

- idle task。
- init user task。
- run queue。
- 当前 task 指针。
- trap 返回用户态。
- `exit` 只终止当前 task；仅最后一个线程退出时提交进程级 zombie。

QEMU 侧的上下文所有权按下列边界固定，不再保留从 simulator
复制而来的 16 槽寄存器镜像：

- 每个 task 的完整用户态 `TrapFrame` 固定位于 `kernel_stack_top - size_of::<TrapFrame>()`，由 `trap.S` 原地保存和恢复，并作为用户寄存器的唯一事实来源。
- 第一阶段的 signal-frame stack 直接由 `Task::sig_frames` 持有，信号屏蔽字只由 `Task::sig_mask` 持有，不再为单一字段保留 `ThdCtx` 包装。单线程退出核心路径已经接入；尚未接入 syscall 的 `clear_child_tid` 不提前保留，待 `clone` / `set_tid_address` ABI 接入后在线程资源和地址空间释放前补写零与 futex wake。
- `fork` / `clone` 从调用者的完整 live `TrapFrame` 派生子任务现场；`exec` / `sigreturn` 通过 syscall outcome 由持有 live frame 的 trap 边界原子替换现场，避免从 `Task` 再创建第二个可变引用。
- checkpoint 中的 `SavedTrapFrame` 只是序列化 DTO，restore 时转成运行时 `TrapFrame` 并直接安装到新 task 的内核栈顶。
- task 切换使用仅保存内核态 `ra` / `sp` / `s0..s11` 的 `KernelContext` 和 `__switch`；不将内核切换现场混入用户 `TrapFrame`。
- 单 hart 阶段的 `KernelContext` 由 `Task` 内稳定地址的 `UnsafeCell` 持有，只有 CPU0 调度路径可读写；`Arc<Task>` 保证切换端点在挂起期间地址和生命周期稳定。任何 `MutexGuard`、`RwLockGuard` 或其他临界区借用都不得跨越 `__switch`，引入多 hart 前必须把这条独占约束替换为 per-hart 所有权协议。
- `Kernel` 为每个 hart 保留 `Processor`，其中 `current` 是当前 task 的唯一 CPU 归属标记，`idle_context` 保存 boot/idle stack 的内核上下文；第一阶段只允许 CPU0 进入真实调度循环。

当前 CPU0 的页表切换边界固定如下：

- idle、`__switch`、`task_bootstrap`、Rust trap handler 和 task→idle handoff 始终运行在 kernel satp 下；`KernelContext.ra` 不保存用户地址，也不在用户页表下解释。
- Sv39 低半区顶部两页分别保留为 supervisor-only `TRAMPOLINE` 和 `TRAP_CONTEXT`，普通 `VmMap` / usercopy / ELF / mmap 范围以 `TRAP_CONTEXT` 为 exclusive `USER_TOP`，不得覆盖这两个架构页。
- linker 把 user trap entry/return 限制在一个物理页；内核根与每个用户根把该页映射到相同 `TRAMPOLINE` 虚拟地址，只允许 trampoline 在切换 `satp` 后继续取指。
- 每次 CPU0 返回某个任务用户态前，将固定 `TRAP_CONTEXT` 重新绑定到该任务内核栈顶 TrapFrame 所在物理页；该映射无 `PTE_U`，不进入用户 VMA、COW、checkpoint 或 resident-page 语义。用户 trap 先通过该别名保存完整现场，再切回 kernel satp 和 high-half 内核栈。
- `exec`、`sigreturn` 或重新调度后必须在每次用户返回边界刷新 user satp、kernel satp、内核 TrapFrame 指针和 trampoline 地址，不能复用旧地址空间的运行时元数据。
- 固定单个 `TRAP_CONTEXT` 别名依赖“只有 CPU0 执行真实用户任务”的现阶段约束；引入多 hart 前必须改为 per-hart trap context 槽位或其他不会并发重绑定同一用户根的协议。
- CPU0 scheduler 在 boot stack 上关中断选取 runnable task，先发布 `Running` 和 `current`，再从 `idle_context` 切换到 task context；task 阻塞、时间片用尽或退出时先发布状态，然后切回 idle context。无 runnable task 时必须清空 `current`，打开中断并在 idle stack 上执行 `wfi`。运行路径不保留“scheduler 尚未初始化时只修改 `current` / run queue”的兼容分支；任何需要换出当前 task 的操作都必须发生在已初始化的 idle-context 调度器中，否则属于内核生命周期错误。
- 正在运行的退出 task 只先标记 zombie 并保留内核栈；只有 `__switch` 已经回到 idle stack 后，scheduler 才能释放该栈。
- `Task::done()` 只读取线程局部的 `TaskRunState::Zombie`。`Process` 用同一生命周期锁维护 `ProcessPhase::{Running, Exiting, Zombie}` 和线程 TID 集合，使最后线程判断与禁止退出期 clone 成为一个原子状态转换。
- RISC-V `exit(93)` 进入单线程退出；`exit_group(94)` 和默认终止信号进入线程组退出。非最后线程不得释放 fd、地址空间或 futex waiters，也不得重定向子进程、发布 `CHILD_QUIT` 或发送 `SIGCHLD`。
- `wait4` / reap 只能观察 `ProcessPhase::Zombie`；`Exiting` 表示资源清理尚未完成，不能提前暴露给父进程。

#### Signal 第二阶段待办：把现场迁移到用户栈

提交 `da7e18f` 已完成第一阶段的 RISC-V signal syscall ABI、真实用户地址空间 copy-in/copy-out、独立用户态 sigreturn trampoline 和 U-mode handler round-trip。第二阶段应在这个基线上把内核 `Task::sig_frames` shadow stack 替换为用户栈上的 Linux RISC-V `rt_sigframe`；在下列任务全部完成并通过回归前，必须继续保留 `Task::sig_frames`：

1. 明确用户 ABI：
   - 若目标是运行 musl，直接匹配 Linux RISC-V 的 `siginfo_t + ucontext_t` 布局，不把 Rust `TrapFrame` 当成 ABI。
   - frame 至少保存原信号屏蔽字、原 `sepc` 和 `x1..x31`，所有字段使用固定宽度和显式 offset/size。
   - frame 起始地址保持 16 字节对齐，并为 `siginfo`、`ucontext` 定义可测试的固定 offset。
2. 让 `Task::enter_signal_handler()` 成为可失败的用户栈写入：
   - 从被中断现场的用户 `sp` 使用 checked subtraction 预留 frame，再向下按 16 字节对齐。
   - 使用 `AddrSpace::write_user_bytes()` 写入编码后的 frame；用户栈溢出、未映射或不可写不能 panic，也不能留下半提交的 handler 现场。
   - 写入成功后设置 `ra=USER_SIGTRAMP`、`sp=frame_sp`、`a0=signo`、`a1=SIGINFO_ADDR`、`a2=UCONTEXT_ADDR`。
3. 让 `rt_sigreturn` 只信任用户 frame 中允许恢复的状态：
   - 从 syscall caller frame 的当前用户 `sp` 定位 `rt_sigframe`，使用 `AddrSpace::read_user_bytes()` copyin 并验证完整 frame。
   - 只恢复用户 GPR、`sepc` 和信号屏蔽字；恢复 mask 后强制清除 `SIGKILL`、`SIGSTOP`。
   - `sstatus` 由内核重建并保证 `SPP=0`；不得从用户 frame 恢复 `kernel_satp`、`user_satp`、`kernel_frame`、内核栈地址或其他 trap/trampoline 运行时字段。
4. 定义坏 frame 的不可返回失败路径：
   - 错误地址、错误大小/对齐、非法用户 `sepc`/`sp` 或不可接受的恢复状态应终止进程并产生 `SIGSEGV`。
   - 不允许 sigreturn 失败后从 trampoline 的 `ecall` 继续执行。
5. 增加真实 U-mode 验收：
   - 单次 handler `ret -> USER_SIGTRAMP -> rt_sigreturn -> 恢复被中断现场`。
   - 嵌套信号在用户栈形成连续 frame，并按 LIFO 顺序恢复。
   - mask 恢复、`SIGKILL`/`SIGSTOP` 强制清除、坏 frame 触发 `SIGSEGV`。
   - 若以 musl 为目标，增加由其 ABI 布局构造/解释 frame 的兼容测试。
6. 最后删除内核 shadow stack：
   - 删除 `Task::sig_frames`、内核 `SigFrame`、构造时的 `Vec::new()`、fork/clone 显式复制、exec `clear()`、exit `mem::take()` 和依赖这些内部状态的旧测试。
   - 改为验证 fork 通过地址空间 COW 自然继承正在执行的用户 signal frame，exec 更换地址空间后旧 frame 自然消失，exit 只需释放地址空间。
   - 删除前审查 checkpoint/restore，确认它不会继续假设存在内核 signal-frame stack。

后续按 `kernel-sim` 现有能力逐步迁移：

- `fork` / `clone`。
- `exec`。
- `wait4`。
- futex。
- pipe / epoll wait。
- signal。

迁移顺序应从 trap 返回用户态、`exit`、`wait4`、`fork/clone` 的可观察行为开始；每迁移一个行为，都应能指出对应的 `kernel-sim` 源文件、迁入后的改动点和回归测试。

### 6. fd、文件和设备语义迁移层

这一层先迁入 `kernel-sim/src/kernel/fs/` 中已有的 fd table、open-file-description、pipe 和 epoll ready/wait 语义，再把 host 文件对象和等待后端替换为 QEMU 可用承载。第一阶段建议只实现最小字符设备作为 fd 后端：

- fd `1` / `2` 写到 SBI console 或 UART。
- fd `0` 可以先返回 EOF 或阻塞占位，按测试需求决定。
- `/bin/init` 先以内嵌 ELF 或 initramfs 提供。

短期文件系统路线：

- 内存 `FileNode` 或 initramfs 支撑 init。
- 迁移 `kernel-sim` 的 fd/open-file-description 共享 offset、cloexec、dup/dup2/fcntl 等语义。
- 后续把 pipe readiness、epoll ready list、waiter 唤醒路径接到 QEMU 调度等待队列。
- 第一阶段对象 VFS 已建立 `Kernel -> Vfs -> Mount -> FsInstance -> FileStorage/root/nodes` 所有权；已解析路径使用 `PathRef { mount, node }`，挂载表按 parent mount 与 mountpoint inode 管理 stacking，不再把 source 设备名拼入全局路径键。
- 当前 `FsInstance::lookup()` 内部仍是完整相对路径键表；后续目录迁移应在不改变调用者 `PathRef` 边界的前提下替换为逐 dentry 分量遍历。source/device 发现、superblock/重启恢复、mount flags、busy/lazy detach 和 mount namespace 仍属于后续阶段。

长期路线：

- virtio-blk。
- 目录和路径解析。
- 设备文件和 TTY。
- 更完整的权限和 credential 检查。

### 7. CRIU-like checkpoint / restore 长期层

该能力定义为 guest 内核中的进程级 checkpoint / restore，而不是 QEMU `savevm` / `loadvm` 这类整机虚拟机快照。它应保存和恢复迁移后的 `kernel-sim` task/process 语义，QEMU 侧只负责提供真实 frame、Sv39 页表、trap frame、usercopy 和设备后端承载。

该层不应早于 M9 核心迁移推进。前置条件包括：

- 用户地址空间已经由真实物理页和 Sv39 页表承载。
- QEMU 侧已经能启动用户 init，并能通过 trap frame / `sret` 返回用户态。
- QEMU 侧一等 `Process` / `Task`、run queue、当前 task、`exit` / `wait4` 基础路径已经迁入；其中 `Process` 承载 `kernel-sim::ProcessState` 的进程级语义，`Task` 只承载线程执行状态。
- fd table、open-file-description、基础 `read` / `write` 后端和用户缓冲区复制已经稳定。
- timer / wait 后端已经能由真实 timer interrupt 推进，阻塞与超时边界可观察。

第一版建议限制为单进程、单线程、syscall 安全点或显式 quiescent point checkpoint，不尝试序列化任意内核栈或持锁临界区。保存内容包括：

- 用户 trap frame、通用寄存器、`sepc`、用户 `sp` 和必要 CSR 返回状态。
- VMA 列表、权限、匿名页内容、brk、用户栈和必要 mapping metadata。
- 基础 fd entry、`FD_CLOEXEC`、open-file-description offset / flags，以及可序列化的内存文件或字符设备状态。
- 必要的 timer deadline 或 alarm 状态；无法稳定恢复的等待状态先拒绝 checkpoint。

明确后置的范围：

- 多线程进程、线程组 leader / 非 leader wait 语义。
- pid namespace、原 pid 复用、父子关系完整重建。
- futex / epoll / pipe 等阻塞中的等待现场恢复。
- socket、TTY、virtio-blk 文件系统、namespace、cgroup、seccomp、ptrace、credential / capability 完整状态。
- 跨内核版本或跨 image 格式版本的兼容恢复。

实现顺序仍应遵守 source-first：

1. 先在 `kernel-sim` 定义 checkpoint / restore 的可观察语义和 smoke 回归。
2. 抽取 image header、section tag、错误码、对齐 helper 等纯数据结构到 `kernel-common/` 或迁移模块，保持 `no_std` / `alloc` 可用。
3. 在 `kernel-qemu` 中新增 checkpoint 模块时，从已迁入的 `Task`、`Process`、`AddrSpace`、fd table 和 timer state 读取状态，不绕过这些语义源另写平行状态。
4. restore 先允许创建新 pid 和新地址空间，重放用户页、VMA、trap frame 和基础 fd 状态后放回 run queue。
5. 验收同时保留 `kernel-sim` smoke 和 QEMU smoke：QEMU 侧至少覆盖 init 触发 checkpoint、修改用户内存或 fd offset 后 restore、恢复态继续执行并输出预期日志。

## 阶段计划

### Milestone 0：建立迁移清单和语义基线

产物：

- 本设计文档。
- `TASK.md` 保留 M9 TODO。
- 不改 `kernel-sim` 行为。
- 列出第一批直接迁入对象及对应源码/测试：syscall 最小集、`kernel-sim` 的 `Task`/`ProcessState` 语义及 QEMU 侧 `Task`/`Process` 承载、`AddrSpace`/ELF/user stack、fd table、timer tick。
- 为每个对象记录复制目标路径、暂时不能编译的 host 依赖、第一轮替换方案。

验证：

```bash
cd kernel-sim
cargo test
```

成功标准：

- `kernel-sim` 原有测试继续通过。
- 每个第一批迁移对象都有明确的 `kernel-sim` 语义源、QEMU 复制目标和替换清单，不把 QEMU 侧实现当作新的语义源。

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

- 直接迁入的 `kernel-sim/src/kernel/mm/` 代码。
- 在迁入 `FramePool` / `AddrSpace` 基础上改造出的 frame allocator。
- 在迁入页表入口基础上改造出的 Sv39 kernel page table。
- 内核地址空间映射。
- 从 `read_user_bytes()` / `write_user_bytes()` 语义改造出的基础 `copy_from_user` / `copy_to_user`。
- `AddrSpace` 语义适配方案：把模拟页面内容替换为真实 frame 和页表映射，同时保留 VMA 权限、映射生命周期、错误返回和回收语义。

成功标准：

- 内核能在分页开启后继续输出。
- 非法用户地址能被拒绝或触发可诊断 fault。
- 能说明 `kernel-sim` 中哪些地址空间代码已直接迁入，哪些 host 依赖已替换，哪些仍停留在 host 模拟器。

### Milestone 4：迁移第一个用户进程路径

产物：

- 内嵌 init ELF。
- 用户地址空间。
- 用户栈。
- `sret` 进入用户态。
- user `ecall` 返回内核。
- 直接迁入并改造 `kernel-sim` 的 ELF `PT_LOAD`、用户栈初始化、pid/task 初始化和 `exec` 地址空间替换语义。

成功标准：

- init 可以执行到 `ecall`。
- 内核能识别 syscall number 和参数。
- init 的建立过程能映射回 `kernel-sim` 中 `proc_init` / `exec` / 用户栈相关语义。

### Milestone 5：迁移最小 syscall 语义

产物：

- RISC-V syscall number 映射层。
- 直接迁入 `kernel-sim/src/kernel/syscall/` 中对应 syscall 的语义入口。
- 在迁入入口基础上把 `write` 接到 SBI/UART。
- 在迁入入口基础上把 `exit` 接到 init task 退出或关机路径。
- 在迁入入口基础上让 `getpid` 返回固定或真实 pid。
- 在迁入入口基础上实现 `read` 的 EOF 或最小输入语义。
- 每个 syscall 都标出对应的 `kernel-sim` syscall 语义源、迁入文件和 QEMU 替换点；QEMU 侧只替换用户指针访问、fd 后端和返回寄存器写回。

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

### Milestone 7：进程级 checkpoint / restore

该里程碑属于 M9 核心迁移之后的长期目标，不作为第一阶段 QEMU 裸机迁移成功标准。

产物：

- `kernel-sim` 中的 checkpoint / restore 语义入口和 smoke 回归。
- 可审查的 checkpoint image 格式：header、section、版本号、地址空间段、寄存器段、fd 段和 timer 段。
- `kernel-qemu` 中从迁移后的 `Task` / `Process` / `AddrSpace` / fd table 导出 image 的路径。
- `kernel-qemu` 中 restore 到新 task / 新地址空间 / 新 run queue entry 的路径。
- 明确拒绝不支持状态的错误返回，例如多线程、正在阻塞的 futex / epoll、不可序列化设备 fd。

成功标准：

- `kernel-sim` smoke 能证明 checkpoint 后修改用户内存、brk 或 fd offset，再 restore 可回到 checkpoint 时状态。
- QEMU smoke 能证明 init 触发 checkpoint，随后改变用户态可观察状态，再 restore 并继续从恢复后的 PC/SP 执行。
- 所有 QEMU 新行为都能映射回 `kernel-sim` 中的 checkpoint / restore 语义源，而不是依赖 QEMU 整机快照。

## 共享代码策略

默认先复制 `kernel-sim` 的具体实现作为迁移起点，再把其中可以长期共用的纯逻辑抽到 `kernel-common`。共享 crate 不是第一步；第一步是让 `kernel-qemu` 中能看到和审查从 `kernel-sim` 迁入的真实代码。

可以考虑共享：

- ELF header / program header 解析中的纯解析逻辑。
- 页大小、权限 bit、对齐 helper。
- syscall 错误码和部分常量。
- 用户栈布局算法。
- fd table 的抽象接口设计。

迁入到 `kernel-qemu` 后必须替换，暂时不要抽到 `kernel-common`：

- 任何直接使用 `std` 的模块。
- host lock、host thread、host time 相关代码。
- `KernelRuntimeTicker`。
- 基于 `Arc<Mutex<Vec<u8>>>` 的模拟地址空间。
- 测试专用 helper。

当某段逻辑需要从已迁入代码中抽成共享层时，先满足三个条件：

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

- 最大风险是绕过 `kernel-sim` 代码，直接写一套看起来相似但语义逐渐偏离的新 QEMU 实现。
- 第二个风险是直接复制后不做 host 依赖清单，导致 `std`、host thread、host lock、host 文件对象或模拟页面模型渗入最终裸机路径。
- 第三个风险是把 syscall 语义和 RISC-V ABI 混在一起，导致后续难以同时维护 host 模拟器和 QEMU 内核。
- 第四个风险是先做文件系统、网络或完整 epoll，绕过了更基础的 trap、页表、调度和用户态返回路径。
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
