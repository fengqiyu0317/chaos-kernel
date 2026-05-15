# kernel-sim 调度现状

<!-- AGENT -->

本文档总结 `kernel-sim` 当前和调度相关的结构、`schedule_tick()` 的实际行为，以及后续如果要实现真正调度器需要补齐的内容。

## 当前结论

`kernel-sim` 目前有一些调度器零件，但还没有组成真正的调度器。

`schedule_tick(cpu)` 现在真正稳定生效的部分只有推进逻辑时钟：

```rust
dtk(cpu);
```

其余代码会计算“是否需要重新调度”“抢占目标”“内核中运行时间”等信息，但这些结果都只保存在局部变量里，函数返回后就丢弃，不会改变当前 CPU 上运行的任务。

## 已有结构

### 当前 CPU 任务

`Kernel` 里有每个 CPU 当前任务槽：

```rust
pub cpus: Mutex<[Option<Arc<Task>>; MAX_CPU]>,
```

并提供：

```rust
cur_task(cpu)
set_cur(cpu, task)
```

这说明模拟器具备“某个 CPU 当前运行哪个任务”的基本表示。

问题是 `proc_init()` 当前只创建 root task，没有把 root 设置为 CPU 0 的当前任务。因此如果没有其他路径调用 `set_cur()`，`cur_task(cpu)` 可能为空。

### 任务表

`TaskTable` 是任务注册表，维护所有任务：

```rust
pub struct TaskTable {
    pub map: RwLock<BTreeMap<usize, Arc<Task>>>,
    pub seq: AtomicUsize,
    pub root: Mutex<Option<Arc<Task>>>,
}
```

它支持创建和查找任务，例如：

```rust
spawn()
spawn_root()
fork_task()
clone_thread()
active_tasks()
zombie_tasks()
reap()
```

但它不是 run queue。`active_tasks()` 只是返回 `status == None` 的任务 id 列表，并不表示这些任务都处于 runnable 状态。

### 任务状态

当前任务状态非常粗略：

```rust
pub status: Option<i32>
```

目前基本含义是：

```text
None       = 没有退出
Some(code) = 已退出
```

它不能区分：

```text
Running
Runnable
Sleeping
Blocked
Zombie
```

所以调度器无法可靠判断某个任务是否真的可以被选中运行。

### 线程上下文

`Task` 有线程上下文：

```rust
pub thd_ctx: Mutex<Option<ThdCtx>>
```

并提供：

```rust
begin_run()
end_run()
```

这可以保存和恢复模拟上下文，但当前没有和 `schedule_tick()` 接起来。也就是说，tick 到来时不会根据时间片切换上下文。

### RunQueue

`proc/sched.rs` 里已经定义了：

```rust
SchedulePolicy
RunQueue
```

`RunQueue` 支持：

```rust
enqueue()
dequeue()
pick_next()
yield_current()
update_vruntime()
```

但 `Kernel` 结构里没有 `RunQueue` 字段，`schedule_tick()` 也没有使用 `RunQueue`。目前它更像是独立写好的调度器雏形，没有接入主执行路径。

## schedule_tick 当前行为

当前实现大致做了这些事：

1. 调用 `dtk(cpu)` 推进 `CLK` / `CLK_ALL`。
2. 尝试获取当前 CPU 的任务。
3. 根据子任务数量计算一个伪时间片剩余量。
4. 如果时间片为 0，则计算是否需要抢占以及抢占目标。
5. 计算一个伪“内核中运行时间”。

其中只有第 1 步有真实副作用。

后续计算存在几个问题：

- `_remaining_slice` 每次调用都会重新计算，没有保存在任务或调度策略中，所以不会随 tick 递减。
- `_remaining_slice` 当前通常是 9 或 7，基本不会等于 0。
- `_needs_resched` 和 `_preempt_target` 是局部变量，没有写回任何状态。
- 没有调用 `set_cur()`，所以当前任务不会变化。
- 没有把当前任务重新入队，也没有从 run queue 选下一个任务。
- 没有根据任务阻塞、退出、信号、等待等状态做过滤。

## 目前缺失的关键环节

要让单 CPU 调度真正生效，至少需要补齐：

```text
Kernel.run_queue
TaskState
任务创建后入队
任务退出后出队
任务阻塞时从 runnable 集合移除
任务唤醒时重新入队
每个任务或调度实体的 time_slice_remaining
schedule_tick 扣减时间片
时间片耗尽后 pick_next
通过 set_cur(cpu, next_task) 切换当前任务
```

如果继续支持多 CPU，还需要每 CPU run queue 或全局 run queue 的并发策略。但单 CPU 调度不依赖多 CPU，可以先只实现 CPU 0。

## 建议改造顺序

建议不要直接把 `schedule_tick()` 里的局部变量改成写回状态，而是按下面顺序改：

1. 增加明确的任务状态枚举，例如 `TaskState::{Runnable, Running, Sleeping, Zombie}`。
2. 在 `Kernel` 中接入一个 `RunQueue`，初期可以只支持 CPU 0。
3. 修改 `proc_init()`，创建 root 后设置 `set_cur(0, Some(root))` 或将 root 入队后调度。
4. 修改 `fork_task()` / `new_user_task()` 等创建路径，让新任务进入 runnable 状态并入队。
5. 在 `exit_proc()` / `reap()` 等路径中让任务退出 runnable 集合。
6. 给调度实体保存实际剩余时间片，而不是在 `schedule_tick()` 中临时计算。
7. 修改 `schedule_tick()`，让它推进时间、扣当前任务时间片、必要时切换到下一个 runnable 任务。
8. 再处理 futex、epoll、pipe 等等待路径中的阻塞和唤醒语义。

## 当前建议

在没有补齐上述状态和 run queue 接入前，`schedule_tick()` 最好继续只承担稳定推进逻辑时间的职责。现有的调度计算可以保留为占位，但它容易误导读代码的人，以为调度已经生效。

如果要先降低误导，可以把 `schedule_tick()` 简化为：

```rust
pub fn schedule_tick(&self, cpu: usize) {
    // AGENT: kernel-sim currently uses ticks as deterministic logical time.
    // Scheduling decisions are not wired into a run queue yet.
    dtk(cpu);
}
```

真正调度器应在 `RunQueue`、`TaskState`、任务创建/退出/阻塞/唤醒路径都串起来之后再启用。
