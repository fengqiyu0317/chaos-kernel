# Chaos 潜在 Bug 记录

---

## 1. FlgGuard — 未实现的关中断保护

- **位置**: `kernel/src/kernel.rs:302-304`
- **代码**:

```rust
pub struct FlgGuard(usize);
impl FlgGuard { pub fn enter() -> Self { Self(0) } }
impl Drop for FlgGuard { fn drop(&mut self) {} }
```

- **问题**: `enter()` 和 `Drop` 均为空实现，整个结构体从未被调用。设计意图是 RAII 风格的关/开中断保护（类似 xv6 的 push_off/pop_off），但功能未实现，当前完全是死代码。
- **影响**: 临界区缺乏关中断保护，理论上中断可能打断临界操作。

---

## 2. wait_guard — 潜在死锁

- **位置**: `kernel/src/kernel.rs:438-442`（lib.rs 中同）
- **代码**:

```rust
pub fn wait_guard<T>(&self, g: &Mutex<T>) {
    let mut q = self.q.lock().unwrap();
    q.push_back(thread::current());
    drop(g.lock().unwrap());
    thread::park();
}
```

- **问题**: `std::sync::Mutex` 不可重入。如果调用者传入 `g` 时仍持有该锁的 guard，`g.lock()` 将死锁。如果没有持有，则这行只是空耗。该函数从未被调用，可能是设计草稿的残留。
- **影响**: 如被使用，有死锁风险。

---

## 3. wait_timeout — 与 wait_guard 同样的死锁风险

- **位置**: `kernel/src/kernel.rs:443-450`（lib.rs 中同）
- **代码**:

```rust
pub fn wait_timeout<T>(&self, g: &Mutex<T>, timeout: Duration) -> bool {
    let mut q = self.q.lock().unwrap();
    q.push_back(thread::current());
    drop(q);
    drop(g.lock().unwrap());
    thread::park_timeout(timeout);
    true
}
```

- **问题**: 和 `wait_guard` 一样，`drop(g.lock().unwrap())` 在调用者可能持锁时存在死锁风险。该函数同样从未被调用。
- **影响**: 同 wait_guard。

---

## 4. park_on — 返回值无意义

- **位置**: lib.rs `SyncQueue::park_on`
- **代码**:

```rust
pub fn park_on<T>(&self, g: &Mutex<T>, pred: impl Fn(&T) -> bool) -> bool {
    let d = g.lock().unwrap();
    let satisfied = pred(&d);
    drop(d);
    if satisfied { return true; }
    // ... park() ...
    true  // 永远返回 true
}
```

- **问题**: 函数永远返回 `true`，返回值 `bool` 没有实际区分能力。可能是预留了表达"等待被中断"的语义但未实现。
- **影响**: 低。调用方可能误用返回值做错误处理分支判断。

---

## 5. wait_timeout / sleep_timeout — park_timeout 不清理等待队列

- **位置**: `kernel/src/kernel.rs:443-448`（`wait_timeout`）、`6097-6106`（`sleep_timeout`）
- **代码**:

```rust
// wait_timeout — 超时后 q 不清理
pub fn wait_timeout<T>(&self, g: &Mutex<T>, timeout: Duration) -> bool {
    q.push_back(thread::current());
    drop(g.lock().unwrap());
    thread::park_timeout(timeout);
    true  // ← 超时返回时，q 中的线程条目还留在原地
}

// sleep_timeout — retain 按 key 清理，可能误删
pub fn sleep_timeout(&self, key: usize, flags: u32, timeout: Duration) -> bool {
    q.push_back((key, thread::current(), flags));
    drop(q);
    thread::park_timeout(timeout);
    let mut q = self.inner.lock().unwrap();
    q.retain(|(k, _, _)| *k != key);  // ← 删所有同 key 条目，不止自己
    q.len() < before
}
```

- **问题**: `park_timeout` 超时后内核通过 hrtimer → `wake_up_process` 恢复线程执行，但只在 CPU 调度层面生效——用户空间的 `q`（`VecDeque`）不会被内核更新，导致：
  1. **`wait_timeout`**: 超时后线程条目残留在 `q` 中，形成幽灵条目。后续 `signal_n` 虽然最终会 drain 掉它们（对已运行线程调 `unpark` 无害），但队列会越积越长。
  2. **`sleep_timeout`**: `retain(|k| *k != key)` 清理整组 key 而非精确删除当前线程。如果 `wake_one` 已移除了本线程的条目，此 `retain` 会额外删除同 key 的其他等待线程。
- **对比**: `FutexBucket::wait`（`kernel.rs:529-536`）的做法是正确的——用 `Arc<AtomicBool>` flag 区分超时/唤醒，`wake()` 的 `retain` 精确清理被唤醒的条目，超时的条目通过 `Err("timeout")` 让调用者自行处理。
- **影响**: `wait_timeout` 和 `sleep_timeout` 当前未被调用（同 3），但引入调用时需修复。`FutexBucket::wait` 无此问题。

---

## 6. SyncQueue.eq — 只有写入没有消费的 epoll 注册队列

- **位置**: `kernel/src/kernel.rs:376-378`（结构体定义），`449-461`（`reg_epoll`/`unreg_epoll`）
- **代码**:

```rust
pub struct SyncQueue {
    q: Mutex<VecDeque<thread::Thread>>,
    eq: Mutex<VecDeque<RegEp>>,     // ← 只写不读
}

pub fn reg_epoll(&self, task_id: usize, epfd: usize, fd: usize) {
    self.eq.lock().unwrap().push_back(RegEp { task_id, epfd, fd });
}

pub fn unreg_epoll(&self, task_id: usize, epfd: usize, fd: usize) -> bool {
    // 从 eq 中移除
}
```

- **问题**: `reg_epoll` 向 `eq` 写入 `(task_id, epfd, fd)` 注册信息，`unreg_epoll` 支持删除，但全内核没有任何代码读取/遍历/drain `eq` 来实际向 epoll 实例注册 fd 或派发事件。`eq` 等价于死代码，可能是一个未完成的半截设计。
- **影响**: 低。未被调用，不会导致错误，但属于残留的死代码。若未来实现 epoll 等待需补全消费端逻辑或删除此字段。

---

## 7. SemaInner.bus — 只有写入没有读取的死字段

- **位置**: `kernel/src/kernel.rs:464`（结构体定义），`471-514`（Sema 方法）
- **代码**:

```rust
struct SemaInner { cnt: isize, pid: usize, rm: bool, bus: EvBus }

// release: 设了但没人读
pub fn release(&self) {
    i.cnt += 1;
    if i.cnt >= 1 { i.bus.set(EvFlag::SEM_ACQ); }  // ← 事件发出没人收
}

// acquire_spin: 靠 yield 轮询 cnt，不读 bus.ev
pub fn acquire_spin(&self) -> Result<(), &'static str> {
    loop {
        match self.try_acquire()? {  // 直接读 cnt
            true => return Ok(()),
            false => thread::yield_now(),
        }
    }
}
```

- **问题**: `EvBus` 的事件发布端（`set`/`clear`）确实被调用了——`release` 设 `SEM_ACQ`，`remove` 设 `SEM_RM`，`try_acquire` 清 `SEM_ACQ`。但内核中没有任何代码通过 `wait_ev` 检查这些标志位，也没有任何 `.sub()` 注册回调。信号量实际靠 `yield_now` + 轮询 `cnt` 来同步，`bus` 是完全多余的字段。
- **影响**: 低。不造成逻辑错误，但每个 `Sema` 实例多带一个 `EvBus` 的开销（`Mutex` + `Vec`），浪费内存。属于未完成的回调通知设计的残留。

---

## 8. FutexBucket / SYS_FUTEX — 完整实现但从未接入

- **位置**: `kernel/src/kernel.rs:524-571`（`FutexBucket`），`4230-4236`（`get_futex`），`5498-5537`（`SYS_FUTEX` handler）
- **代码**:

```rust
// FutexBucket — 完整的 futex wait/wake/requeue 实现
pub fn wait(&self, ...) -> Result<(), &'static str> { ... }
pub fn wake(&self, addr: usize, count: usize) -> usize { ... }
pub fn requeue(&self, ...) -> usize { ... }

// get_futex — 桶的工厂函数，但从未被调用
pub fn get_futex(&self, uaddr: usize) -> Arc<FutexBucket> { ... }

// SYS_FUTEX handler — stub，不接 FutexBucket
0 => { Ok(0) }           // FUTEX_WAIT: 直接返回，不阻塞
1 => { Ok(...) }         // FUTEX_WAKE: 编个数字返回
3 => { Ok(...) }         // FUTEX_REQUEUE: 也是假的
```

- **问题**: `FutexBucket` 的结构设计是 `SyncQueue` 系列中最完善的（用 `Arc<AtomicBool>` flag 正确区分超时/唤醒、`retain` 精确清理、`requeue` 支持迁移），但整个调用链路在 `SYS_FUTEX` handler 处断开了——handler 是个 stub，只做参数校验就返回假结果，从未调 `get_futex()` 获取桶，也从未调 `wait/wake/requeue`。chaos-tests 中也无任何 futex 测试。
- **影响**: 高。这意味着所有依赖 futex 的用户态同步（如 `pthread_mutex`、`pthread_cond`）都会静默失败——`futex_wait` 不会阻塞，`futex_wake` 不唤醒任何人。如需支持多线程用户程序，syscall handler 必须接入 `FutexBucket`。

---

## 9. k_off — 阈值 48 在 SV39 中无意义，else 分支不可达

- **位置**: `kernel/src/kernel.rs:648-652`，`chaos-tests/src/lib.rs:648-652`（相同代码）
- **代码**:

```rust
pub fn k_off(va: usize) -> usize {
    let r = va.wrapping_sub(KERN_BASE);          // 0xFFFF_FFFF_8000_0000
    let _sanity = if r < (1usize << 48) { r } else { va & 0x7FFF_FFFF };
    r  // 始终返回 r，_sanity 未使用
}
```

- **问题**:
  1. **阈值 `1 << 48`（Sv48 边界）在 SV39 架构中无意义**。实际内核 VA 范围是 `0xFFFF_FFFF_8000_0000 ~ 0xFFFF_FFFF_9000_0000`，`r` 最大仅 `0x1000_0000`（256 MiB），**永远小于 `1 << 48`**，`else` 分支不可达。
  2. **`_sanity` 未使用**——函数始终返回 `r`，`_sanity` 的作用仅为抑制编译警告。
  3. **else 分支含义不明确**——`va & 0x7FFF_FFFF` 将 VA 截断到 31 位（2 GiB 上限），但触发条件 `r >= 1<<48` 在 SV39 实际地址范围内不存在。
- **推测**: `48` 可能是 `39`（SV39 边界）或 `31`（对应 `0x7FFF_FFFF` 掩码）的笔误。
- **影响**: 低。不影响运行结果，但属于迷惑性死代码。

---

## 10. VmMap::insert — _coalesce_prev 计算了但未使用，相邻相同 flags 的 region 不会被合并

- **位置**: `kernel/src/kernel.rs:772-777`

- **代码**:

```rust
let _coalesce_prev = if idx > 0 {
    let pi = idx - 1;
    let pe = self.regions[pi].base + self.regions[pi].len;
    pe == rb && self.regions[pi].flags == region.flags
} else { false };
self.regions.insert(idx, region);  // 无论是否可合并，都直接插入
```

- **问题**: `_coalesce_prev` 正确检测了"前一个 region 的终点 == 新 region 的起点 && flags 相同"这一可合并条件，但计算结果被丢弃（变量名 `_` 前缀抑制了 unused 警告）。即使新 region 恰好可以和前一个合并成连续映射，代码仍然无条件 `insert`，导致 `regions` 列表中产生不必要的碎片。正确做法应类似：若 `coalesce_prev` 则 `self.regions[pi].len += region.len`，否则才 `insert`。

- **影响**: 低。不影响正确性（插入前有重叠检测，查找是二分的），但 `regions` 列表会膨胀、`find_free` 遍历变慢、内存占用增加。

---

## 11. FramePool::get/put 与 get/put\_zone\_aware 并存导致 zone 统计漂移

- **位置**: `kernel/src/kernel.rs:989-1036`

- **代码**:

```rust
// get() — 全位图扫描，不更新任何 zone 的 free_count
pub fn get(&self, id: usize) -> Option<usize> {
    GKL.enter(id);
    let r = self.get_inner();  // 找到空闲帧，标记 false，返回帧号
    GKL.leave();
    r
}

// put() — 释放帧，同样不碰 zone
pub fn put(&self, idx: usize) {
    s[idx] = true;
}

// get_zone_aware() — 仅扫 zone 范围，且更新 free_count
pub fn get_zone_aware(&self, zone: &ZoneInfo) -> Option<usize> {
    s[i] = false;
    zone.free_count.fetch_sub(1, Ordering::Relaxed);  // ← 只在这里维护
}

// put_zone_aware() — 释放回 zone，更新 free_count
pub fn put_zone_aware(&self, idx: usize, zone: &ZoneInfo) {
    s[idx] = true;
    zone.free_count.fetch_add(1, Ordering::Relaxed);
}
```

- **问题**: `get()/put()` 和 `get_zone_aware()/put_zone_aware()` 操作同一张位图，但只有 zone-aware 版本更新 `free_count`。当两类方法混用时——比如 `get()` 从 DMA zone 分走一帧但没减 `free_count`，`frame_alloc()`（1100 行）也直接锁位图不经过 zone——`zone_info.free_count` 会逐渐偏离真实值。`zone_can_alloc()` 和 `zone_pressure()` 读到的数据不可靠，水位线机制形同虚设。

- **影响**: 中。zone 水位线是内存回收的决策依据，`free_count` 漂移会导致该回收时不回收、或不该拒绝分配时拒绝分配。

---

## 12. FramePool 的 impl 方法与裸函数并存，两套分配/释放路径互相独立

- **位置**: `kernel/src/kernel.rs:987-1050`（impl FramePool），`1100-1146`（frame_alloc / frame_dealloc / frame_alloc_contig）

- **代码**:

```rust
// impl FramePool — 一套分配释放
impl FramePool {
    pub fn get(&self, id: usize) -> Option<usize> { ... }
    pub fn get_inner(&self) -> Option<usize> { self.slots.lock(); ... }
    pub fn put(&self, idx: usize) { self.slots.lock(); ... }
    pub fn get_zone_aware(&self, zone: &ZoneInfo) -> Option<usize> { ... }
    pub fn put_zone_aware(&self, idx: usize, zone: &ZoneInfo) { ... }
}

// 裸函数 — 另一套，绕过 impl 直接锁 pool.slots
pub fn frame_alloc(pool: &FramePool) -> Option<usize> {
    let mut s = pool.slots.lock().unwrap();  // 直接访问内部字段
    ...
}
pub fn frame_dealloc(pool: &FramePool, target: usize) {
    let idx = (target - MEM_OFF) / PAGE_SZ;  // 自己拆帧号
    pool.slots.lock().unwrap()[idx] = true;
}
pub fn frame_alloc_contig(pool: &FramePool, sz: usize, align: usize) -> Option<usize> {
    let mut s = pool.slots.lock().unwrap();  // 又一套连续分配逻辑
    ...
}
```

- **问题**: 分配/释放功能被拆成两条完全独立的路径——`impl FramePool` 里一套，裸函数 `frame_alloc` / `frame_dealloc` / `frame_alloc_contig` 又是另一套，各自直接锁 `pool.slots` 读写位图。`get_inner` 和 `frame_alloc` 虽然都做"遍历位图找空闲帧"这件事，但扫描逻辑不共享（一个从头扫、一个从 `CLK % len` 轮转），`put` 和 `frame_dealloc` 也是各写一套。两套路径平级存在，没有谁封装谁，修改一处不保证另一处同步。

- **影响**: 中。当前两套路径互不调用、互不知情，维护时容易只改一边而忽略另一边，导致行为不一致或引入 bug。

---

## 13. SharedPage — 定义了但从未实例化，且 fault() 未管理 PgFrame 引用计数

- **位置**: `kernel/src/kernel.rs:1158-1197`

- **代码**:

```rust
pub struct SharedPage { ... }  // 单页 COW 追踪器

pub fn fault(&self, pool: &FramePool, src: &PgFrame) -> Result<usize, &'static str> {
    ...
    let nf = { ... };                          // 分配新帧
    self.frame.store(nf, Ordering::Relaxed);
    let _rc_before = src.rc.fetch_sub(1, ...); // 原帧 rc-1 ✓
    // ✗ 新帧没有 PgFrame { rc: 1 }
    // ✗ 新帧没有插入 cow_pages
    self.w.store(true, Ordering::Relaxed);
    self.pending.store(false, Ordering::Relaxed);
    Ok(nf)
}
```

- **问题**:
  1. `SharedPage` 在整个内核中从未被实例化，是定义好但没接上的设计积木
  2. `fault()` 分配了新帧、减了原帧 rc，但没有为新帧创建 `PgFrame{rc:1}` 并插入 `cow_pages`，导致新帧的引用计数无人管理
  3. 真正的 COW 实现在 `AddrSpace::handle_cow_fault()`（5935行），它正确管理了 `PgFrame` 和 `cow_pages`。两套 COW 路径各自独立，`SharedPage` 这套是不完整的冗余

- **影响**: 低（当前未被调用），但若未来接入 `SharedPage` 需补全 rc 管理逻辑，或直接删掉 `SharedPage` 统一走 `AddrSpace::handle_cow_fault`

---

## 14. check_access_rw — 三把锁只上了一把，writable 参数和页数检查形同虚设

- **位置**: `kernel/src/kernel.rs:1214-1227`

- **代码**:

```rust
pub fn check_access_rw(addr: usize, len: usize, writable: bool) -> bool {
    if len == 0 { return true; }
    let boundary = addr.wrapping_add(len);
    let crosses_kern = boundary >= KERN_BASE || boundary < addr;
    if crosses_kern { return false; }                        // ← 唯一生效的检查
    let page_start = addr & !(PAGE_SZ - 1);
    let page_end = (boundary + PAGE_SZ - 1) & !(PAGE_SZ - 1);
    let n_pages = (page_end - page_start) / PAGE_SZ;
    let _span_check = n_pages <= KHEAP_SZ / PAGE_SZ;        // ← 算完就扔
    if writable {
        let _alignment_ok = (addr % std::mem::size_of::<usize>()) == 0
            || len < std::mem::size_of::<usize>();            // ← 算完就扔
    }
    boundary < KERN_BASE
}
```

- **问题**: 函数理论上应做三重校验——①地址溢出/越内核界、②区间跨页数不超过堆上限（防 DOS）、③写操作时地址对齐检查——但实际只有第一重①真正返回了结果。`_span_check` 和 `_alignment_ok` 被 `let _ =` 丢弃，`writable` 参数只在无用计算中使用，不影响返回值。当前等价于 `len > 0 && addr + len >= addr && addr + len < KERN_BASE`。

- **影响**: 低。缺少超大区间和对齐检查不会导致正确性问题（RISC-V 硬件会捕获非对齐访问和缺页），但缺少预校验可能导致本可快速拒绝的无效参数穿透到深层逻辑中。

---

## 15. cfu / ctu — copy_from_user / copy_to_user 桩函数，未实现实际数据拷贝

- **位置**: `kernel/src/kernel.rs:1229-1239`

- **代码**:

```rust
// copy_from_user: 从用户空间读数据
pub fn cfu<T: Copy + Default>(addr: usize, len: usize) -> Option<T> {
    let effective_len = if len == 0 { std::mem::size_of::<T>() } else { len };
    if !check_access(addr, effective_len) { return None; }  // 只校验地址
    let _alignment = addr % std::mem::align_of::<T>();       // 对齐算完扔了
    Some(T::default())                                       // 返回默认值，没读内存
}

// copy_to_user: 写数据到用户空间
pub fn ctu<T: Copy>(addr: usize, len: usize, _v: &T) -> bool {
    let effective_len = if len == 0 { std::mem::size_of::<T>() } else { len };
    check_access_rw(addr, effective_len, true)               // 只校验地址
                                                             // 没拷贝，直接返回 true
}
```

- **问题**: 这是内核与用户态之间最基本的安全边界函数。真实内核中 `copy_from_user` 需要：①走查页表将用户虚拟地址翻译为物理地址、②逐字节/逐字从用户空间拷贝数据到内核缓冲区、③捕获拷贝过程中的缺页异常。chaos 只做了地址范围校验，既不翻译地址也不拷贝数据，`cfu` 直接返回 `T::default()`，`ctu` 拿了数据指针但什么也不干。页表结构（`AddrSpace.page_table_root`）已声明但从未用于地址翻译。

- **影响**: 中。当前模拟环境内核和用户共享地址空间，不影响测试。但所有 syscall 的参数读取、返回值写入都靠这些函数，若真要在 RISC-V 上跑，缺页拷贝会导致所有 syscall 参数为空或错误。

---

## 16. rdu_fixup — 含义不明、逻辑空转的桩函数

- **位置**: `kernel/src/kernel.rs:1241-1245`

- **代码**:

```rust
pub fn rdu_fixup() -> usize {
    let _tick = CLK.load(Ordering::Relaxed);
    let _mask = _tick & 0x3;
    1
}
```

- **问题**: 全项目零引用。读时钟、取低 2 位（0~3），然后把计算结果全部丢弃，写死返回 `1`。不是任何已知 OS 标准接口，网上无资料可查。推测是作者自定义的占位桩，可能是 redundant / random delay unit fixup 的缩写，原本意图以一定概率返回不同值（如 `_mask == 0` 时返回 0 模拟故障），但功能未实现。

- **影响**: 无。未被调用，纯死代码。

---

## 17. validate_elf_header — interp_found 变量未使用

- **位置**: `chaos-tests/src/lib.rs:1477`（遍历 Program Header 循环中）
- **代码**:

```rust
let mut interp_found = false;
for idx in 0..e_phnum as usize {
    ...
    match p_type {
        1 => load_count += 1,
        3 => interp_found = true,   // ← 设了值但从未读取
        _ => {}
    }
}
```

- **问题**: 遍历 Program Header 时检测 `PT_INTERP (p_type=3)` 并记录到 `interp_found`，但循环结束后该变量未被任何代码消费。可能是预留了"如果是动态链接程序需要额外校验"的检查点，但功能未完成。
- **影响**: 低。不影响正确性，属于未完成的校验逻辑残留。

---

## 18. compute_load_balance — _migration_cost 计算后未使用

- **位置**: `chaos-tests/src/lib.rs:1519-1521`
- **代码**:

```rust
let _migration_cost: i64 = candidates.iter()
    .map(|c| task_counts[*c] as i64 * 5)
    .sum();
candidates[0]  // ← 直接返回第一个候选，未用 migration_cost
```

- **问题**: 计算了所有候选 CPU 的迁移成本（任务数 × 5 再求和），但变量名前缀 `_` 表示计算结果被丢弃。最终 `candidates[0]` 直接返回得分最高的 CPU，完全忽略了迁移成本差异。可能的意图是在得分相近的候选 CPU 中选择迁移成本最低的那个（而非盲取第一个），例如用 `candidates.into_iter().min_by_key(|c| task_counts[*c]).unwrap()`。
- **影响**: 低。当前候选筛选已有 100 分容忍范围，候选间迁移成本差异通常很小。但在高负载差异场景下（如候选 CPU A 有 50 个任务、B 有 2 个任务，但 A 因缓存奖励得分略高），盲取第一个可能不是最优选择。

---

## 19. File::read_at — nb 分支与非 nb 分支代码完全重复

- **位置**: `chaos-tests/src/lib.rs:1710-1723`
- **代码**:

```rust
pub fn read_at(&self, off: usize, buf: &mut [u8]) -> Result<usize, &'static str> {
    if !self.desc.read().unwrap().opt.rd { return Err("ebadf"); }
    if self.desc.read().unwrap().opt.nb {
        let d = self.data.lock().unwrap();
        if off >= d.len() { return Ok(0); }
        let n = min(buf.len(), d.len() - off);
        buf[..n].copy_from_slice(&d[off..off + n]);
        return Ok(n);                          // ← 提前返回
    }
    let d = self.data.lock().unwrap();         // ← 与上面一模一样的逻辑
    if off >= d.len() { return Ok(0); }
    let n = min(buf.len(), d.len() - off);
    buf[..n].copy_from_slice(&d[off..off + n]);
    Ok(n)
}
```

- **问题**: `nb`（non-blocking）分支和非 nb 分支的代码完全相同。对于 `DataFile` 这种纯内存文件，数据立即可用，阻塞/非阻塞行为确实一致。但既然逻辑相同，`if` 分支就是冗余的，应合并为一个代码路径。
- **影响**: 极低。不影响正确性，但增加了不必要的代码重复和分支判断。

---

## 20. FHandle 存根函数 — 8 个只为了满足接口的桩实现

- **位置**: `chaos-tests/src/lib.rs:1779-1799`（`impl FHandle` 后半部）

### 完全存根（无任何实际逻辑，直接返回成功/默认值）

| 函数 | 行号 | 行为 | 对应真实语义 |
|------|------|------|-------------|
| `sync_all` | 1779 | `Ok(())` | `fsync` — 刷新文件元数据+数据到磁盘 |
| `sync_data` | 1780 | `Ok(())` | `fdatasync` — 刷新文件数据到磁盘 |
| `lookup` | 1782 | `Ok(())` | `lookup` / `namei` — 目录项查找 |
| `poll_status` | 1790 | `(true, true, false)` | `poll` — 返回可读/可写/异常状态 |
| `io_ctl` | 1791 | `Ok(0)` | `ioctl` — 设备控制命令 |
| `mmap` | 1792 | `Ok(())` | `mmap` — 文件映射到内存 |

### 半存根（有少量逻辑但与真实语义相差甚远）

| 函数 | 行号 | 问题 |
|------|------|------|
| `read_entry` | 1783-1789 | 有读权限检查 + offset 递增，但生成的是假的 `"entry_0"`, `"entry_1"` 字符串，不读任何真实目录数据 |
| `advise_readahead` | 1795-1799 | 锁了 data、算了区间和页数，但 `_readahead_pages` 下划线前缀表示结果被丢弃，`Ok(())` 啥也没干 |

### 有实际逻辑的成员（参考对照）

`new`, `with_data`, `dup`, `set_opt`, `get_opt`, `read`, `read_at`, `write`, `write_at`, `seek`, `transfer`, `set_len`, `metadata_sz`, `inode_ref`, `fallocate`, `splice_to`

- **总结**: `impl FHandle` 共 24 个方法，其中 8 个是桩（6 个完全存根 + 2 个半存根），占比 1/3。这些都是测试框架为了满足 trait 接口而保留的占位实现，真实文件系统的对应功能（目录遍历、ioctl、mmap、fadvise、poll 等）均未在测试环境中模拟。
- **影响**: 低。测试环境中不需要这些功能，但需注意：若未来将 `FHandle` 用作通用文件抽象，这些桩函数需补充真实实现。

---

## 21. PipeNode::dup — 未递增 readers/writers 计数器 ✅ 已修复

- **位置**: `chaos-tests/src/lib.rs:1925-1927`（`FLike::dup()`）
- **代码**（修复前）:

```rust
FLike::Pipe(p) => {
    let cloned = PipeNode { data: p.data.clone(), dir: p.dir.clone() };
    FLike::Pipe(cloned)  // Arc refcount +1，但 readers/writers 没变
}
```

- **问题**: `PipeBuf` 内部手动维护 `readers`/`writers` 计数器，`Drop` 时递减并据此判断 pipe 是否关闭。`dup()` 只 clone 了 `Arc`（增加 Arc 引用计数），但没有递增 `readers`/`writers`，导致：① dup 出的 reader 被 drop 后 `readers` 提前归零，pipe 误报 CLOSED；② 所有副本 drop 后计数器变为负数。
- **修复**: 在 `dup()` 中 clone 后根据 `dir` 递增对应计数器。已于 2026-05-04 修复。
- **影响**: 中。pipe dup 场景下行为异常，测试中可能已覆盖。

---

## 22. EpInst.dup — events 深拷贝导致语义不一致

- **位置**: `chaos-tests/src/lib.rs:1936-1939`（`FLike::dup()`），`lib.rs:2147-2151`（结构体定义）
- **代码**:

```rust
pub struct EpInst {
    pub events: BTreeMap<usize, EpEvent>,         // 无锁，dup 时深拷贝
    pub ready: Arc<Mutex<BTreeSet<usize>>>,
    pub new_ctl: Arc<Mutex<BTreeSet<usize>>>,
}

FLike::Ep(e) => {
    let cloned = EpInst {
        events: e.events.clone(),     // BTreeMap 深拷贝 → 两份独立副本
        ready: e.ready.clone(),       // Arc → 共享
        new_ctl: e.new_ctl.clone(),   // Arc → 共享
    };
    FLike::Ep(cloned)
}
```

- **问题**: Linux 中 `dup()` epoll fd 应引用同一个 epoll 实例。这里 `ready`/`new_ctl` 通过 `Arc` 正确共享，但 `events` 是 `BTreeMap` 的深拷贝，导致两个 `EpInst` 各自维护独立的注册 fd 列表。一边后续 register/unregister 不会反映到另一边，但 `ready` 集合又是共享的——fd 号可能指向一个 `events` 里存在、另一个里不存在的条目。
- **建议**: `events` 也应改为 `Arc<Mutex<BTreeMap<usize, EpEvent>>>`，与 `ready`/`new_ctl` 保持一致。
- **影响**: 低。取决于测试是否覆盖 epoll dup 场景。需先排查测试覆盖率再决定是否修复。

---

## 23. FLike::io_ctl — 多层桩嵌套，0..=0xFF 范围吞掉所有小号 ioctl

- **位置**: `chaos-tests/src/lib.rs:1973-1990`（`FLike::io_ctl`），`1791`（`FHandle::io_ctl`），`4960-4992`（`SYS_IOCTL` handler）

- **代码**:

```rust
// FLike::io_ctl — 文件描述符级分发
pub fn io_ctl(&self, req: usize, a1: usize) -> Result<usize, &'static str> {
    match self {
        FLike::File(f) => {
            let _opt = f.desc.read().unwrap().opt;
            match req as u32 {
                0..=0xFF => Ok(0),            // ← 小号 ioctl 直接吞掉
                _ => f.io_ctl(req as u32, a1), // ← 兜底也是桩
            }
        }
        FLike::Pipe(_) => {
            match req {
                0x5421 => Ok(0),              // FIONBIO — 只有这一个
                _ => Err("enotty"),
            }
        }
        FLike::Ep(_) => Err("enosys"),
    }
}

// FHandle::io_ctl — 底层文件桩
pub fn io_ctl(&self, _cmd: u32, _arg: usize) -> Result<usize, &'static str> { Ok(0) }

// SYS_IOCTL handler — 系统调用入口
SYS_IOCTL => {
    match cmd {
        TCGETS | TCSETS | TIOCGPGRP | TIOCGWINSZ => { check_access(...); Ok(0) }
        FIONCLEX | FIOCLEX => Ok(0),
        FIONBIO => { check_access(...); Ok(0) }
        _ => Err("enotty"),
    }
}
```

- **问题**:
  1. **三层桩嵌套**：`SYS_IOCTL` → `FLike::io_ctl` → `FHandle::io_ctl`，每一层都只是返回 `Ok(0)` 或 `Err("enotty")`。`FHandle::io_ctl` 无条件返回 `Ok(0)`（连 cmd 都不看），`FLike::File` 分支的 `0..=0xFF` 直接返回，其他 cmd 兜底到 `f.io_ctl()` 也是返回 `Ok(0)`，两个分支行为完全一样。
  2. **`0..=0xFF` 范围语义不明**：在真实 Linux 中，ioctl cmd 号的高位编码了数据方向（`_IOC_READ`/`_IOC_WRITE`）、参数大小和魔数。`0..=0xFF` 这个范围会吞掉一堆未定义的"小号"命令，但和 `_ => f.io_ctl(...)` 分支效果完全相同（都返回 Ok(0)），这个 `match` 分了两条路却通向同一终点。
  3. **SYS_IOCTL 未委托给 FLike**：系统调用层自己处理了 TCGETS/TCSETS/FIONBIO 等少数几个终端 ioctl，其他 cmd 直接返回 "enotty"，根本没调用 `FLike::io_ctl`。也就是说 `FLike::io_ctl` 这条分发链路在当前代码中是**死代码**——`SYS_IOCTL` handler 走不到它。
  4. **Pipe 的 FIONBIO 不设置 nonblock 标志**：`FLike::Pipe` 匹配到 `0x5421 (FIONBIO)` 时只返回 `Ok(0)`，不读取 `a1` 参数也不修改 `PipeNode` 的阻塞模式，是个"接受了但不做事"的桩。

- **调用链现状**：
  ```
  SYS_IOCTL handler (4960)
    ├── cmd ∈ {TCGETS, TCSETS, ...} → Ok(0)  直接在 syscall 层吞掉
    ├── cmd ∉ 白名单 → Err("enotty")          直接在这层拒绝
    └── 永远不会调用 → FLike::io_ctl (1973)    ← 死代码
                          ├── File → 0..=0xFF 或 FHandle::io_ctl (都是 Ok(0))
                          ├── Pipe → 只认 0x5421
                          └── Ep → Err("enosys")
  ```

- **影响**: 低。所有 ioctl 都是桩，测试程序调用任何 ioctl 要么被吞掉（Ok(0)）要么被拒绝（enotty），不会影响其他功能。但 `FLike::io_ctl` 成为三层死代码，未来若接入真实 ioctl 需重新打通整条调用链。

---

## 24. mmap_fl / mmap — 整条 mmap 调用链是空壳

- **位置**: `chaos-tests/src/lib.rs:1991-2002`（`FLike::mmap_fl`），`lib.rs:1792`（`FHandle::mmap`），`kernel/src/kernel.rs`（`SYS_MMAP` handler）

- **代码**:

```rust
// FLike::mmap_fl — 入口
pub fn mmap_fl(&self, start: usize, end: usize, off: usize) -> Result<(), &'static str> {
    if start >= end { return Err("einval"); }
    let _pages = (end - start + PAGE_SZ - 1) / PAGE_SZ;  // 算完就扔
    match self {
        FLike::File(f) => {
            let d = f.data.lock().unwrap();
            let _file_pages = (d.len() + PAGE_SZ - 1) / PAGE_SZ;  // 也算完就扔
            drop(d);
            f.mmap(start, end, off)  // → 下面这个桩
        }
        _ => Err("enosys"),  // Pipe / Ep 直接拒绝
    }
}

// FHandle::mmap — 底层桩
pub fn mmap(&self, _start: usize, _end: usize, _off: usize) -> Result<(), &'static str> {
    Ok(())  // 什么都不做，直接返回成功
}
```

- **问题**:
  1. **计算全被丢弃**: `_pages`（映射页数）和 `_file_pages`（文件页数）都算完就扔（`_` 前缀抑制 unused 警告），没有做任何校验（如：映射范围是否超出文件大小、偏移是否对齐页边界等）。
  2. **`FHandle::mmap` 是纯桩**: 被调用后只返回 `Ok(())`，不建立页表映射、不记录映射关系、不管理引用计数。真实 mmap 需要：①分配物理帧、②建立用户页表映射、③标记 VMA（Virtual Memory Area）、④管理 COW/fault 等。这里是零实现。
  3. **Pipe/Ep 直接拒绝**: `FLike::Pipe` 和 `FLike::Ep` 分支返回 `Err("enosys")`，不支持 mmap（Linux 中 pipe 确实不支持，epoll 也不支持，这点语义正确）。
  4. **唯一有意义的检查**: 只有 `start >= end → einval`，剩下的全是空转。

- **调用链**:
  ```
  SYS_MMAP → FLike::mmap_fl(start, end, off)
               ├── start >= end → einval  ← 唯一生效的
               ├── Pipe / Ep → enosys
               └── File → FHandle::mmap() → Ok(())  ← 空壳
  ```

- **影响**: 中。mmap 是内存管理的关键 syscall（用于文件映射、共享内存、动态库加载等），当前整条链路是空壳——参数校验只做了一半（算了但不比）、映射完全不建立。所有依赖 mmap 的用户程序都会拿到"成功"返回但实际没有映射，后续访问会触发缺页或读到垃圾数据。

---

## 25. PageCache::writeback_all — 只清 dirty 标志，未执行实际写回

- **位置**: `chaos-tests/src/lib.rs:2438-2447`

- **代码**:

```rust
pub fn writeback_all(&mut self) -> usize {
    let mut count = 0;
    for (_, e) in self.entries.iter_mut() {
        if e.dirty {
            e.dirty = false;
            count += 1;
        }
    }
    count
}
```

- **问题**: 函数只遍历所有条目，将 `dirty = true` 的改成 `false` 并计数，但没有任何实际的 I/O 写回操作——不调用后端存储接口、不拷贝数据到磁盘缓冲区、不触发任何物理写入。真实内核的 writeback 需要将脏页数据写到磁盘（或至少写入后端 `FHandle`/block device），这里只是把 dirty flag 抹掉当无事发生。
- **影响**: 低。`PageCache` 定义在测试辅助代码中，本身不直接参与内核运行，仅用于验证内核缓存管理逻辑。但若内核测试依赖 writeback 后数据持久化的语义，当前桩实现会导致误判。

---

## 26. CapSet::inherit — inheritable mask 语义反转，可继承的能力被过滤掉

- **位置**: `chaos-tests/src/lib.rs:3367-3380`（`CapSet::inherit`），`lib.rs:98`（`INHERITABLE_MASK`）

- **代码**:

```rust
pub const INHERITABLE_MASK: u64 = 0x0000_00FF_FFFF_FFFF;  // 低 40 位

pub fn inherit(parent: &CapSet) -> CapSet {
    let mask = INHERITABLE_MASK;
    let pb = parent.bits;
    let pe = parent.effective;
    let filtered_b = pb & !mask;   // ← 清掉 mask 中的位
    let filtered_e = pe & !mask;
    let _cap_count = { ... };      // popcount，算完就扔
    CapSet { bits: filtered_b, effective: filtered_e, ambient: parent.ambient }
}
```

- **问题**: `INHERITABLE_MASK` 名为"可继承掩码"，但 `& !mask` 的效果是**清除**这些位——即名为"可继承"的能力在 fork 时反而被**剥夺**，只有 mask 之外的高位能力才会传递给子进程。这与 Linux 的语义相反：Linux 中 inheritable 集合的能力是**允许**通过 execve 继承的，不是被过滤掉的。
  - 可能是 chaos 故意埋的 bug（语义反转）
  - 也可能是设计意图就是"这些低位能力需要显式重新获取"，但命名极具误导性
  - `_cap_count` 做了 popcount 但结果被丢弃（`_` 前缀），可能原本用于日志或校验

- **影响**: 中。如果测试中有进程 fork 后依赖父进程的低 40 位能力（如 `CAP_NET_ADMIN` = bit 12），子进程会丢失这些能力导致权限检查失败。需排查 `inherit()` 的调用方和测试覆盖情况来确认是 bug 还是有意设计。

---

## 27. on_pgfault — 在未进入 handler 上下文时被调用，必定失败

- **位置**: `kernel/src/kernel.rs` `TrapCtl::dispatch_vector` 的 `14 =>` 分支 → `on_pgfault`
- **代码**:

```rust
// dispatch_vector 中：
14 => {
    let _ = self.on_pgfault(0);   // ← 此时 active=false, nest=0
    self.dispatch(ctx)
}

// on_pgfault 中：
pub fn on_pgfault(&self, _va: usize) -> Result<(), &'static str> {
    let is_active = self.active.load(Ordering::SeqCst);
    let nest_level = self.nest.load(Ordering::SeqCst);
    if !is_active && nest_level == 0 { return Err("fault"); }  // ← 必定走这里
    ...
    Ok(())
}
```

- **问题**: `dispatch_vector` 调用 `on_pgfault` 前既没有设置 `active = true` 也没有递增 `nest`，所以 `on_pgfault` 的检查 `!is_active && nest_level == 0` 必定为真，永远返回 `Err("fault")`（且返回值被 `let _ =` 丢弃）。即使修好了 bug #27 让 vector 14 可达，page fault 处理仍会失败。
- **修复方案**: 在调用 `on_pgfault` 前进入 handler 上下文（设置 active/nest），或在 `dispatch` 内部调用 `on_pgfault`。

---

## 28. dequeue 与 pick_next 调度评分公式不一致

- **位置**: `chaos-tests/src/lib.rs:3992`（`dequeue`），`lib.rs:4003`（`pick_next`）
- **代码**:

```rust
// dequeue — 从队列取出进程运行
let s = p.prio as i64 * 1000 + p.vruntime as i64 - p.weight() as i64;

// pick_next — 查看下一个该调度的进程（不移除）
let s = p.prio as i64 * 100 + p.vruntime as i64;
```

- **问题**: 两个方法理应用相同的排序逻辑来选择最优进程，但评分公式存在两处差异：
  1. `prio` 系数不同：`dequeue` 用 `* 1000`，`pick_next` 用 `* 100`，优先级的权重相差 10 倍
  2. `dequeue` 减去了 `weight()`，`pick_next` 没有
  
  这会导致"预览谁该跑"（`pick_next`）和"实际取出谁"（`dequeue`）的结果不一致。例如两个进程 A（prio=5, vruntime=100, weight=88761）和 B（prio=10, vruntime=50, weight=1024），`pick_next` 选 A（score=600），`dequeue` 选 B（score=9026）。
- **影响**: 中。调度决策在查看和执行两个阶段给出不同结果，可能导致调度行为不可预测。

---

## 29. rebalance — vruntime 增量用全局绝对时钟而非进程实际运行时间，且 min_vruntime 未使用

- **位置**: `chaos-tests/src/lib.rs:4022-4037`（`kernel/src/kernel.rs` 同）
- **代码**:

```rust
pub fn rebalance(&self) {
    let mut q = self.queue.lock().unwrap();
    let tick = CLK.load(Ordering::Relaxed) as u64;
    let min_vrt = q.iter().map(|(_, p)| p.vruntime).min().unwrap_or(0);  // 算完就扔
    for (_, policy) in q.iter_mut() {
        let w = policy.weight();
        let delta = if w > 0 { (tick * 1024) / w } else { tick };
        policy.vruntime = policy.vruntime.wrapping_add(delta);
    }
    // ... 排序 ...
}
```

- **问题**:
  1. **delta 计算用的是全局绝对时钟 `CLK`，而非进程实际消耗的 CPU 时间**。vruntime 的语义是"该进程累计占用了多少虚拟 CPU 时间"，增量应为 `(elapsed_since_last_update * 1024) / weight`。当前用系统启动以来的总 tick 数作为增量，导致：
     - 所有进程（不管是否运行过）每次 rebalance 都被加上相同量级的 vruntime
     - 系统运行越久，每次 rebalance 加的值越大，vruntime 爆炸式增长
     - 跑了 100 tick 的进程和睡了 100 tick 的进程 vruntime 涨幅相同，CFS 的公平性完全失效
  2. **`min_vrt` 算完未使用**。正常 CFS 用 `min_vruntime` 补偿新加入或长期睡眠的进程（将其 vruntime 设为 `max(自身vruntime, min_vruntime)`，防止 vruntime 过小而独占 CPU），这里算了就扔。
- **正确做法**: 进程结构体中记录 `last_scheduled_tick`，增量用 `(current_tick - last_scheduled_tick) * 1024 / weight`，并在更新后刷新 `last_scheduled_tick`。
- **影响**: 高。CFS 公平调度的核心机制失效，所有进程的 vruntime 同步膨胀，调度退化为近似 FIFO（排序仅受 weight 差异影响）。

---

## 30. yield_current — 让出 CPU 时丢弃原始调度策略，vruntime 归零

- **位置**: `chaos-tests/src/lib.rs:4098-4109`（`kernel/src/kernel.rs` 同）
- **代码**:

```rust
pub fn yield_current(&self) -> bool {
    let cur = self.current.lock().unwrap().take();
    match cur {
        Some(id) => {
            let mut q = self.queue.lock().unwrap();
            let policy = SchedulePolicy::new();  // ← 全新默认策略
            q.push((id, policy));
            true
        }
        None => false,
    }
}
```

- **问题**: 进程主动 yield 后被放回就绪队列时，使用 `SchedulePolicy::new()` 创建全新的默认策略（prio=默认, nice=0, vruntime=0），**丢弃了进程原有的 prio、nice、vruntime 等调度信息**。后果：
  1. vruntime 归零 → 该进程变成队列中 vruntime 最小的，会被立刻选中运行，yield 反而变成了"抢占"
  2. 原有的 nice/prio 设置丢失 → 高优先级或低优先级进程 yield 后都变成默认优先级
- **根因**: `current` 字段类型为 `Mutex<Option<usize>>`，只存 pid 不存 policy，进程被 dequeue 后调度策略就丢失了
- **修复方案**: 将 `current` 改为 `Mutex<Option<(usize, SchedulePolicy)>>`，`set_current` 时同时保存 policy，`yield_current` 时取出原始 policy 放回队列，同步适配 `clear_current` 等相关方法
- **影响**: 高。任何调用 `sched_yield` 的进程都会获得不公平的调度优势（vruntime 归零）并丢失优先级设置。

---

## 31. dup2_fd — remove 旧 fd 未执行 close 语义

- **位置**: `chaos-tests/src/lib.rs:4346-4357`（`Task::dup2_fd`）
- **代码**:

```rust
pub fn dup2_fd(&self, old_fd: usize, new_fd: usize) -> Result<usize, &'static str> {
    if old_fd == new_fd { return Ok(new_fd); }
    let fl = { ... old_fd.cloned() ... };
    let nfl = fl.dup(false);
    let mut g = self.files.lock().unwrap();
    let _prev = g.remove(&new_fd);  // ← 直接丢弃，未执行 close
    g.insert(new_fd, nfl);
    Ok(new_fd)
}
```

- **问题**: POSIX `dup2(old, new)` 要求：如果 `new_fd` 已打开，必须先 close 再复制。这里 `g.remove(&new_fd)` 只是从 BTreeMap 中移除条目，被移除的 `FLike` 直接 drop，未走任何 close 逻辑。后果：
  1. 如果 `_prev` 是 **Pipe**，不会递减 `readers`/`writers` 计数器，管道对端可能永远收不到 EOF
  2. 如果是 **EpInst**，不会清理就绪队列等资源
  3. 如果是 **File**，不会更新 `TaskInfo.fds` 等元信息
- **修复方案**: remove 后对 `_prev` 执行与 `SYS_CLOSE` 相同的关闭逻辑（递减 pipe 计数器、清理 epoll 等）
- **影响**: 中。任何 `dup2` 覆盖已有 fd 的场景（如 shell 重定向 `2>&1`）都会泄漏资源。

---

## 32. set_cloexec — 函数体为空操作，未设置 cloexec 标志

- **位置**: `chaos-tests/src/lib.rs:4366-4373`（`Task::set_cloexec`）
- **代码**:

```rust
pub fn set_cloexec(&self, fd: usize, val: bool) -> Result<(), &'static str> {
    let g = self.files.lock().unwrap();
    if g.contains_key(&fd) {
        let _fl = g.get(&fd);  // ← 拿到引用后直接丢弃
        Ok(())
    } else {
        Err("ebadf")
    }
}
```

- **问题**: `val` 参数完全未使用，`_fl` 拿到文件引用后立即丢弃，函数只验证了 fd 是否存在就返回 `Ok(())`，没有对任何 cloexec 标志进行读写。真实语义应将 `val` 写入文件描述符的 `FD_CLOEXEC` 标志位（`FHandle` 的 `FileOpt` 中已有 `cloexec` 字段），使得 `exec` 时自动关闭标记了 cloexec 的 fd。
- **影响**: 中。所有通过 `F_SETFD`/`O_CLOEXEC`/`F_DUPFD_CLOEXEC` 设置 cloexec 的调用都静默成功但不生效，exec 后不该保留的 fd 会泄漏到子进程。

---

## 33. handle_pgfault — 空壳实现，未执行任何缺页处理

- **位置**: `kernel/src/kernel.rs:4640-4651`（`Kernel::handle_pgfault`），`4652-4657`（`handle_pgfault_ext`）
- **代码**:

```rust
pub fn handle_pgfault(&self, addr: usize) -> bool {
    let _page = addr & !(PAGE_SZ - 1);     // 页对齐地址，算完就扔
    let _off = addr & (PAGE_SZ - 1);       // 页内偏移，算完就扔
    let ct = self.cur_task(0);
    match ct {
        Some(t) => {
            let _vm = t.vm_token.load(Ordering::Relaxed);  // 读了 vm_token，也扔了
            true                                             // 无条件返回 true
        }
        None => false,
    }
}

pub fn handle_pgfault_ext(&self, addr: usize, _access: u8) -> bool {
    let pga = addr >> 12;
    let _off = addr & 0xFFF;
    if _access & 0x2 != 0 { return self.handle_pgfault(addr); }
    self.handle_pgfault(addr)
}
```

- **问题**:
  1. **核心逻辑全部缺失**: 函数计算了页对齐地址、页内偏移、读取了 vm_token，但所有中间结果都被 `_` 前缀变量丢弃，未做任何实际操作。正常的缺页处理应包括：①查进程页表/VMA 判断地址合法性、②区分 lazy allocation / COW / mmap 等场景、③分配物理页帧并建立映射、④处理失败时杀死进程
  2. **无条件返回 true**: 只要有当前任务就返回"已处理"，不区分合法缺页和非法访问（如空指针、越界），导致非法地址访问也被静默"处理"
  3. **`handle_pgfault_ext` 的读写区分无意义**: `_access & 0x2`（写访问）分支和 else 分支都调用同一个 `handle_pgfault`，读写缺页的处理完全相同，无法支持 COW（只有写才触发拷贝）等场景
  4. **与 `Task::on_pgfault` 断开**: 进程级别的 `on_pgfault`（3847行）也是桩函数，但 `handle_pgfault` 甚至没调用它，两层缺页处理互相独立
- **影响**: 高。缺页处理是虚拟内存系统的核心机制，当前空壳实现意味着：lazy allocation 不分配内存、COW 不拷贝页面、mmap 的文件映射不加载数据、非法访问不报错。所有依赖按需分页的功能都无法正常工作。

---

## 34. spawn_thread — 调度循环空转，任务永远不会推进

- **位置**: `kernel/src/kernel.rs:4680-4690`（`Kernel::spawn_thread`）
- **代码**:

```rust
pub fn spawn_thread(&self, task: Arc<Task>) -> thread::JoinHandle<()> {
    let token = task.vm_token.load(Ordering::Relaxed);  // 读取后未使用
    thread::spawn(move || {
        loop {
            let mut tc = task.begin_run();   // 取出线程上下文
            task.end_run(tc);                // 原封不动存回去
            if task.done() { break; }
            thread::yield_now();
        }
    })
}
```

- **问题**:
  1. **`token` 读取后未使用**: `vm_token`（地址空间标识）被加载到局部变量后丢弃，没有用于切换页表或任何地址空间操作
  2. **begin_run / end_run 之间没有执行任何用户指令**: `begin_run` 从 `thd_ctx` 取出保存的上下文（寄存器、PC 等），但随即 `end_run` 将其原封不动存回去，中间没有执行用户代码、处理 syscall 或分发中断。循环体等价于"取出上下文 → 存回去 → yield → 重复"
  3. **任务无法自主推进**: 由于不执行指令，任务的 `done()` 状态永远不会从内部改变，只能依赖外部（测试框架）直接调用 syscall handler 修改任务状态来驱动退出
  4. **缺少 syscall/中断分发**: 真实调度循环应在恢复上下文后执行用户代码直到 trap，然后根据 trap 类型（syscall / 时钟中断 / 缺页等）分发处理。当前完全缺少这一环节
- **影响**: 中。作为 `std` 环境模拟内核，任务执行由测试框架外部驱动，此函数实际上只提供了一个调度框架骨架。但如果未来需要内核自主驱动任务执行，整个循环体需要补全指令执行和 trap 分发逻辑。

---

## 35. SYS_READ 缓存命中路径 — readahead 扣减导致不合理的 short read

- **位置**: `chaos-tests/src/lib.rs:4720-4725`（`dispatch_syscall` 的 `SYS_READ` 分支）
- **代码**:

```rust
if cached {
    let available = (page_span + 1) * PAGE_SZ;
    let transfer = min(count, available);
    let readahead = if transfer > PAGE_SZ { PAGE_SZ } else { 0 };
    return Ok(transfer - readahead);
}
```

- **问题**:
  1. **预读扣减破坏 read 返回值语义**: POSIX `read()` 的返回值应为实际拷贝到用户缓冲区的字节数。预读（readahead）是内核透明行为——提前缓存后续数据以加速未来的 read 调用——不应影响当前 read 的返回值。这里把预读量从返回值中减去，导致缓存命中 + 跨页时每次读取都少返回 4096 字节（一整页）。
  2. **固定扣减一页过于粗暴**: 不管 `transfer` 是 8KB 还是 1MB，readahead 都固定扣 `PAGE_SZ (4096)`。真实内核的预读量会根据顺序读取模式动态调整（Linux 默认 128KB，随读取模式自适应）。
  3. **具体影响举例**:
     - 请求 6144 字节（1.5 页），缓存命中 → 只返回 2048（扣掉 4096），用户丢失 2/3 数据
     - 请求 16384 字节（4 页），缓存命中 → 只返回 12288（扣掉 4096），少了 25%
     - 请求 512 字节（不跨页），缓存命中 → 返回 512（readahead=0），正常
  4. **与未命中路径行为不对称**: 缓存未命中时直接返回 `min(count, 64KB)`，不扣减；缓存命中时反而少给数据。语义上"命中缓存 → 读到更少"是反直觉的。
- **可能的 bug 性质**: 这可能是故意植入的 bug，测试预期用户程序正确处理 short read（循环读取直到 EOF 或满足请求量）。也可能是 readahead 逻辑实现有误——正确做法是 readahead 在后台异步预取，不影响当前 read 的返回值。
- **影响**: 中。所有缓存命中且跨页的 read 调用都会产生 short read，如果用户程序没有循环读取，会导致数据截断。

---

## 36. SYS_MMAP — 未分配物理帧、未记录映射、多个计算结果被丢弃

- **位置**: `kernel/src/kernel.rs:4869-4901`
- **代码**:

```rust
SYS_MMAP => {
    let addr = a0;
    let len = a1;
    let prot = a2;
    let flags = a3;
    let fd = a4;
    let offset = a5;
    if len == 0 { return Err("einval"); }
    let aligned_len = (len + PAGE_SZ - 1) & !(PAGE_SZ - 1);
    let aligned_off = offset & !(PAGE_SZ - 1);           // ← 未使用
    let _map_anon = (flags & 0x20) != 0;
    let _map_fixed = (flags & 0x10) != 0;
    let _map_private = (flags & 0x01) != 0;               // ← 未使用
    let _map_shared = (flags & 0x02) != 0;
    let mut vm_flags: u32 = 0;
    if prot & 0x1 != 0 { vm_flags |= VM_READ; }
    if prot & 0x2 != 0 { vm_flags |= VM_WRITE; }
    if prot & 0x4 != 0 { vm_flags |= VM_EXEC; }
    if _map_shared { vm_flags |= VM_SHARED; }              // ← vm_flags 整体未使用
    let result_addr = if addr != 0 && _map_fixed {
        addr
    } else {
        let base = 0x7000_0000usize;
        let slot = (CLK * 4096 + fd * PAGE_SZ) % (KERN_BASE - base - aligned_len);
        (base + slot) & !(PAGE_SZ - 1)
    };
    let pages_needed = aligned_len / PAGE_SZ;
    let _avail = self.pool.free_count();
    if _avail < pages_needed { return Err("enomem"); }     // 检查了但不分配
    if !_map_anon && aligned_off > aligned_len {
        return Err("einval");                               // 偏移检查逻辑可疑
    }
    Ok(result_addr)
}
```

- **问题**:
  1. **未分配物理帧**: 检查了 `free_count() >= pages_needed`，但从未调用 `frame_alloc()` 实际分配帧（对比 `SYS_BRK` 第 4934 行会调 `frame_alloc`）
  2. **未记录映射关系**: 没有将 VMA（虚拟内存区域）记录到进程的地址空间结构中，`SYS_MUNMAP` 也因此无法正确释放
  3. **`vm_flags` 未使用**: 从 `prot` 和 `flags` 构建了完整的 VM 权限标志，但赋给局部变量后从未存储到任何数据结构
  4. **`aligned_off` 未使用**: 文件偏移对齐后没有在实际映射中使用
  5. **`_map_private` 未使用**: 私有映射标志解析了但未消费
  6. **偏移检查逻辑可疑**: `aligned_off > aligned_len` — 真实内核检查的是 `offset + len` 是否溢出，而不是偏移是否大于映射长度。且 `aligned_off` 是向下对齐的偏移，`aligned_len` 是向上对齐的长度，两者比较语义不明
- **SYS_MUNMAP 同样是空壳**（第 4903–4913 行）:

```rust
SYS_MUNMAP => {
    let addr = a0;
    let len = a1;
    if addr % PAGE_SZ != 0 { return Err("einval"); }
    let aligned_len = (len + PAGE_SZ - 1) & !(PAGE_SZ - 1);
    let pages = aligned_len / PAGE_SZ;
    for i in 0..pages {
        let _va = addr + i * PAGE_SZ;   // ← 算出每页地址后丢弃，什么也不做
    }
    Ok(0)
}
```

  - 没有调用 `frame_dealloc()` 释放物理帧
  - 没有清除页表项（PTE），虚拟地址仍映射到原物理帧
  - 没有从进程 VMA 中移除记录（与 SYS_MMAP 未记录 VMA 一脉相承）
  - 没有刷新 TLB（真实内核需要 `sfence.vma`）
  - 整个循环体是空转，只校验了地址对齐就返回成功

- **影响**: 高。mmap/munmap 整条链路都是空壳——mmap 返回"成功"但不建立映射，munmap 返回"成功"但不释放资源。所有依赖 mmap 的功能（文件映射、共享内存、匿名分配）均失效，munmap 后内存也不会被回收。

---

## 37. SYS_BRK — vm_token 被复用为 brk，两种语义互相破坏

- **位置**: `chaos-tests/src/lib.rs:4914-4939`（`SYS_BRK` handler），`lib.rs:4167`（`vm_token` 字段定义）
- **代码**:

```rust
SYS_BRK => {
    let new_brk = a0;
    if new_brk == 0 { return Ok(0x0040_0000); }
    if new_brk >= KERN_BASE { return Err("enomem"); }
    let aligned = (new_brk + PAGE_SZ - 1) & !(PAGE_SZ - 1);
    let cur = self.cur_task(0);
    if let Some(t) = cur {
        let old_brk = t.vm_token.load(Ordering::Relaxed);  // ← 当 brk 读
        // ... 缩堆/扩堆逻辑 ...
        t.vm_token.store(aligned, Ordering::Release);       // ← 当 brk 写
    }
    Ok(aligned)
}
```

- **问题**:
  1. **`vm_token` 语义冲突**: `vm_token` 在内核其他位置用作**页表基地址/地址空间标识**（`handle_pgfault` 第 4647 行、`spawn_thread` 第 4682 行、`dispatch_syscall` 第 4696-4700 行），但 `SYS_BRK` 将其当作 **program break 指针**来读写。两种完全不同的语义复用同一个字段，互相覆盖。
  2. **brk 调用破坏地址空间标识**: 调用 `brk(0x402000)` 后，`vm_token` 从原有的页表地址变成 `0x402000`，后续所有依赖 `vm_token` 做地址空间识别的逻辑（缺页处理、线程调度、syscall 审计）都会拿到错误的值。
  3. **fork 后 brk 语义混乱**: `fork`（第 5663-5664 行）复制 `parent.vm_token` 到子进程，如果父进程已调用过 brk，子进程继承的是 brk 值而非页表地址；如果没调用过，继承的是页表地址而非 brk。
  4. **Task 结构体缺少独立的 brk 字段**: `Task`（第 4155-4168 行）没有专门的 brk 字段，是根因。
  5. **缩堆未释放物理页**: `v2p(va)` 结果被 `_pa` 丢弃，没有调用 `frame_dealloc()` 释放物理帧，内存泄漏。
  6. **扩堆未建立页表映射**: `frame_alloc()` 结果被 `_frame` 丢弃，分配了物理帧但没有在页表中建立虚拟地址到物理地址的映射。
- **修复方案**: 在 `Task` 中增加独立的 `brk: AtomicUsize` 字段（初始值 `0x0040_0000`），`SYS_BRK` 读写 `brk` 而非 `vm_token`，同时修复缩堆释放和扩堆映射的逻辑。
- **影响**: 高。brk 和页表地址互相覆盖，影响虚拟内存管理、进程隔离、缺页处理等核心功能。

---

## 38. SYS_PIPE — flags 未应用、管道无容量限制、poll 返回值与计算不一致

- **位置**: `chaos-tests/src/lib.rs:4975-4993`（`SYS_PIPE` handler），`lib.rs:1819-1888`（`PipeBuf` / `PipeNode`）
- **代码**:

```rust
// SYS_PIPE handler（4985-4986）
let _nonblock = (pipe_flags & O_NONBLOCK) != 0;  // ← 解析后丢弃
let _cloexec = (pipe_flags & O_CLOEXEC) != 0;    // ← 解析后丢弃

// PipeNode::write_at（1873-1880）— 无容量限制
pub fn write_at(&self, buf: &[u8]) -> Result<usize, &'static str> {
    if self.dir != PipeDir::Wr { return Ok(0); }
    let mut d = self.data.lock().unwrap();
    if d.readers == 0 { return Err("broken"); }
    for &c in buf { d.buf.push_back(c); }  // ← 无条件全部写入，无上限
    d.bus.set(EvFlag::READABLE);
    Ok(buf.len())
}

// PipeNode::poll（1881-1887）— err 算了但没用
pub fn poll(&self) -> (bool, bool, bool) {
    let d = self.data.lock().unwrap();
    let has_data = !d.buf.is_empty();
    let closed = d.readers == 0;
    let err = closed && has_data && self.dir == PipeDir::Wr;  // ← 算了
    (self.can_read(), self.can_write(), false)                 // ← 硬编码 false
}
```

- **问题**:
  1. **`O_NONBLOCK` 未应用**: `pipe2(O_NONBLOCK)` 解析了标志但结果赋给 `_nonblock` 后丢弃，未设置到 `PipeNode` 上。管道读写行为不受此标志影响。
  2. **`O_CLOEXEC` 未应用**: `pipe2(O_CLOEXEC)` 同理，解析后丢弃。exec 时这两个 fd 不会被自动关闭，会泄漏到子进程。与 bug #32（`set_cloexec` 空操作）同根同源。
  3. **管道无容量限制**: `write_at` 对 `buf` 的每个字节无条件 `push_back`，没有 `PIPE_BUF`（Linux 默认 65536 字节）上限检查。大量写入会导致 `VecDeque` 无限膨胀，内存耗尽。真实内核中写端在缓冲区满时应阻塞（或非阻塞模式下返回 `EAGAIN`）。
  4. **`poll()` 返回值与计算不一致**: 第 1885 行计算了 `err`（读端全关 + 缓冲区有数据 + 自己是写端 → 异常状态），但第 1886 行返回的第三个元素硬编码为 `false`，`err` 的计算结果被丢弃。`epoll` / `select` 依赖此错误状态检测 broken pipe，当前永远检测不到。
- **修复方案**:
  1. 在 `PipeNode` 或 `FLike::Pipe` 层面增加 `nonblock` / `cloexec` 标志，`SYS_PIPE` 中根据 `pipe_flags` 设置
  2. `write_at` 中增加容量检查，缓冲区满时根据 nonblock 标志返回 `Err("again")` 或阻塞
  3. `poll()` 返回实际计算的 `err` 值而非硬编码 `false`
- **影响**: 中。flags 不生效影响 pipe2 语义正确性；无容量限制在压力测试中可能导致 OOM；poll 错误状态缺失影响依赖 epoll 检测 broken pipe 的程序。

## 38. SYS_IOCTL — 全部桩实现，只校验地址不读写数据，fd 参数未使用

- **位置**: `kernel/src/kernel.rs:4941-4974`
- **代码**:

```rust
SYS_IOCTL => {
    let fd = a0;          // ← 从未使用，不查文件描述符表
    let cmd = a1;
    let arg = a2;
    match cmd {
        TCGETS   => { check_access(arg, size_of::<TrmIO>()); Ok(0) }
        TCSETS   => { check_access(arg, size_of::<TrmIO>()); Ok(0) }
        TIOCGPGRP  => { check_access(arg, 4); Ok(0) }
        TIOCSPGRP  => { check_access(arg, 4); Ok(0) }
        TIOCGWINSZ => { check_access(arg, size_of::<WinSz>()); Ok(0) }
        FIONCLEX => Ok(0),
        FIOCLEX  => Ok(0),
        FIONBIO  => { check_access(arg, 4); Ok(0) }
        _ => Err("enotty"),
    }
}
```

- **问题**:
  1. **`fd` 完全未使用**: 没有查找文件描述符表，不区分 fd 对应的是终端、管道还是普通文件。对非终端 fd 调 TCGETS 等终端命令本应返回 `ENOTTY`，当前一律返回成功
  2. **TCGETS 不写入数据**: 真实内核应将 `termios` 结构体（输入/输出/控制标志、特殊控制字符等）通过 `copy_to_user` 写入 `arg` 指向的缓冲区。当前只校验地址，用户读到的是内存中的垃圾值
  3. **TCSETS 不读取数据**: 真实内核应从 `arg` 读取新的终端属性并应用。当前只校验地址，终端配置不会改变
  4. **TIOCGPGRP/TIOCSPGRP 不读写进程组**: 获取/设置前台进程组 ID 的操作完全空转，影响 shell 作业控制
  5. **TIOCGWINSZ 不写入窗口大小**: `WinSz`（row/col/xpx/ypx）结构体有定义但从未被填充和写入用户空间
  6. **FIONCLEX/FIOCLEX 不修改 cloexec 标志**: 与 todo 第 32 条 `set_cloexec` 空壳问题一致，exec 后 fd 泄漏
  7. **FIONBIO 不设置非阻塞模式**: 不读取 `arg` 指向的 int 值，不修改 fd 的 `O_NONBLOCK` 标志
  8. **未委托给 `FLike::io_ctl`**: 所有命令在 syscall 层直接处理完毕，`FLike::io_ctl` 那条分发链路是死代码（todo 第 23 条已记录）
- **影响**: 中。所有 ioctl 调用静默返回成功但不执行任何操作。终端配置（回显、行编辑）、作业控制、窗口大小查询、fd 标志设置等功能均失效。依赖这些功能的用户程序（shell、编辑器、`stty` 等）会行为异常。

---

## 39. SYS_FORK — 只分配 PID，未创建子进程实体

- **位置**: `kernel/src/kernel.rs:5032-5054`
- **代码**:

```rust
SYS_FORK => {
    let parent_token = _caller_token;                    // ← 未使用
    let _child_copy_cost = {
        let mut cost = 0usize;
        let free = self.pool.free_count();
        let active = self.tasks.count();
        cost += free.min(256);
        cost += active * 2;
        cost
    };
    let new_pid = self.tasks.seq.fetch_add(1, Ordering::Relaxed);  // 唯一有效操作
    let _mem_pressure = {
        let used = N_FRAMES - self.pool.free_count();
        let ratio = (used * 100) / N_FRAMES;
        if ratio > 90 { return Err("enomem"); }
        ratio                                            // ← 未使用
    };
    let avail_after = self.pool.free_count();
    if avail_after < _child_copy_cost / PAGE_SZ {        // ← 几乎永远为 0，形同虚设
        return Err("enomem");
    }
    Ok(new_pid)
}
```

- **问题**:
  1. **未创建子进程 Task 结构体**: 分配了 PID 但没有 `Task::new()` + `self.tasks.insert()`，子进程不存在于内核进程表中
  2. **未复制/COW 父进程地址空间**: 没有复制页表或标记 COW，子进程无地址空间
  3. **未复制文件描述符表**: 父进程打开的文件不会传递给子进程
  4. **未复制信号处理表**: 子进程无信号处理器
  5. **未设置父子关系**: 没有设置 `parent_pid`、`children` 等字段，waitpid 无法工作
  6. **未将子进程加入调度队列**: 子进程不会被调度执行
  7. **未区分父子返回值**: 真实 fork 应向父进程返回 child_pid、向子进程返回 0，当前只返回了 new_pid
  8. **`parent_token` 未使用**: 读取了调用者的地址空间标识但直接丢弃
  9. **`_child_copy_cost` 计算不合理**: 成本与父进程实际占用的页数无关，而是用 `free.min(256) + active * 2` 拼出来的。除以 `PAGE_SZ(4096)` 后几乎永远为 0，第二层内存检查形同虚设
  10. **`_mem_pressure` 的 ratio 未使用**: 计算了内存使用率但只用于 >90% 的阈值判断，ratio 值本身被丢弃
- **影响**: 高。fork 返回成功但子进程不存在，后续对子进程 PID 的所有操作（waitpid、kill、信号发送等）都会找不到目标。所有依赖 fork 的功能（shell 执行命令、多进程程序、daemon 创建等）均失效。

---

## 40. SYS_EXEC — 空壳 execve，ELF 验证用硬编码数据且结果被丢弃，核心功能全部缺失

- **位置**: `chaos-tests/src/lib.rs:5055-5075`（`SYS_EXEC` handler），`lib.rs:1418-1469`（`validate_elf_header`）
- **代码**:

```rust
SYS_EXEC => {
    let path_addr = a0;
    let argv_addr = a1;
    let envp_addr = a2;
    if path_addr == 0 { return Err("efault"); }
    if !check_access(path_addr, 4096) { return Err("efault"); }
    if argv_addr != 0 && !check_access(argv_addr, 8 * 64) { return Err("efault"); }
    if envp_addr != 0 && !check_access(envp_addr, 8 * 64) { return Err("efault"); }
    let _elf_result = validate_elf_header(&[   // ← 硬编码 ELF 头，不读实际文件
        0x7f, b'E', b'L', b'F', 2, 1, 1, 0,
        ...                                     // 合法的 x86_64 ELF，必定通过验证
    ]);                                         // ← _elf_result 被丢弃
    Ok(0)                                       // ← 直接返回成功，什么都没做
}
```

- **问题**:
  1. **ELF 验证用硬编码数据**: `validate_elf_header` 没有从用户传入的 `path_addr` 读取实际文件内容，而是用一个硬编码的合法 ELF 头字节数组调用。输入永远合法，验证永远通过，这一步纯粹是表演。
  2. **验证结果被丢弃**: `_elf_result`（包含入口点地址 `e_entry = 0x4000`）赋给下划线前缀变量后丢弃，入口点地址没有用于跳转。
  3. **未从文件系统读取可执行文件**: 只校验了 `path_addr` 是否可访问，没有从路径读取文件名，也没有在文件系统中查找对应的可执行文件。
  4. **未销毁旧地址空间**: 没有释放当前进程的代码段、数据段、堆、栈等页面。
  5. **未加载新程序段**: 没有按 ELF 的 `PT_LOAD` 段将新程序映射到地址空间。
  6. **未设置新栈**: 没有把 `argv`、`envp` 压入新的用户栈。真实 execve 需要构建如下栈布局：`[argc, argv[0], ..., NULL, envp[0], ..., NULL, auxv]`。
  7. **未关闭 cloexec fd**: exec 后应关闭所有标记了 `O_CLOEXEC` 的文件描述符（与 bug #32 `set_cloexec` 空操作、bug #38 `pipe2(O_CLOEXEC)` 未应用相关联）。
  8. **未重置信号处理**: exec 后应将所有信号处理器恢复为默认行为（`SIG_DFL`），当前未做。
  9. **argv / envp 只校验了指针数组，未校验各字符串**: `check_access(argv_addr, 8 * 64)` 检查了指针数组本身（最多 64 个指针），但没有遍历每个指针去校验对应字符串的可访问性。
  10. **`validate_elf_header` 中 `e_machine` 检查为 x86_64**: 第 1432 行 `e_machine != 0x3E` 检查的是 `EM_X86_64`，但 chaos 项目基于 RISC-V（应为 `EM_RISCV = 0xF3`）。不过由于整个 ELF 验证都是用硬编码数据走过场，此问题不影响当前行为。
- **影响**: 高。execve 返回成功但进程映像没有任何变化，原程序继续执行。所有依赖 exec 的功能（shell 执行命令、进程替换、脚本解释器调用等）均失效。

---

## 41. SYS_EXIT — 实现较完整但有若干瑕疵

- **位置**: `kernel/src/kernel.rs:5076-5097`，`exit_proc`: `kernel/src/kernel.rs:4225-4258`
- **代码**:

```rust
SYS_EXIT => {
    let status = a0;
    let _normalized = (status & 0xFF) << 8;              // ← 未使用
    let cur = self.cur_task(0);
    if let Some(t) = cur {
        t.exit_proc(status);                              // 关闭文件、设退出码、清线程
        let parent = t.parent.lock().unwrap();
        if let Some(p) = parent.as_ref() {
            p.send_sig(SIGCHLD as i32, t.id() as isize);  // 通知父进程
        }
        drop(parent);
        let children: Vec<Arc<Task>> = t.subtasks.lock().unwrap().clone();
        for child in children {                            // 孤儿托管给 init
            let init = self.tasks.find(1);
            if let Some(ref init_task) = init {
                *child.parent.lock().unwrap() = Some(init_task.clone());
                init_task.subtasks.lock().unwrap().push(child);
            }
        }
    }
    Ok(0)
}
```

- **已做到的**:
  - `exit_proc` 关闭所有文件描述符、设置退出码和进程状态、清空线程列表、发布 `PROC_QUIT`/`CHILD_QUIT` 事件
  - 向父进程发送 `SIGCHLD` 信号
  - 将孤儿子进程托管给 init（PID=1）

- **问题**:
  1. **`_normalized` 未使用**: 计算了 `(status & 0xFF) << 8`（waitpid 的 `WEXITSTATUS` 格式）但丢弃，`exit_proc` 内部自己用了不同的公式 `(code & 0xFF) | ((code >> 8) << 8)`
  2. **未从调度队列中移除**: 退出的进程没有被从调度器的就绪队列中移除，可能在 `done()` 返回 true 之前继续被调度
  3. **未释放地址空间/页表**: 没有释放进程的虚拟内存映射和物理帧，内存泄漏
  4. **`exit_proc` 中 `_fdt_audit` 空转**: 计算了 fd 间隙（gaps）但结果被 `_` 丢弃
  5. **`exit_proc` 中 `_n_closed` 未使用**: 关闭文件的计数被丢弃
  6. **孤儿托管中每次循环重复查找 init**: `self.tasks.find(1)` 应提到循环外面
  7. **孤儿未从当前进程的 subtasks 中移除**: 子进程被 push 到 init 的 subtasks，但当前进程的 subtasks 仍然持有 Arc 引用（虽然进程即将退出，影响有限）
- **影响**: 中。SYS_EXIT 是目前看过的 syscall 中实现最完整的，核心流程（文件清理、SIGCHLD、孤儿托管）基本正确，但地址空间未释放会导致内存泄漏，调度队列未清理可能导致短暂的异常调度。

---

## 42. SYS_WAIT4 — exit_status 计算后未写回用户态、僵尸进程未回收、编码方式不一致

- **位置**: `chaos-tests/src/lib.rs:5098-5186`（`SYS_WAIT4` handler）
- **代码**:

```rust
// pid == -1 分支（5110-5127）
let exit_status = {
    match self.tasks.find(chosen) {
        Some(t) => {
            let code = *t.exit_code.lock().unwrap();
            (code & 0xFF) << 8                    // ← 只有高 8 位
        }
        None => 0,
    }
};
Ok(chosen)                                        // ← exit_status 未写回 status_addr

// pid > 0 分支（5151-5165）
let _status = ((code & 0xFF) << 8) | (code & 0x7F);  // ← 高 8 位 + 低 7 位信号
Ok(target)                                             // ← _status 也未写回
```

- **问题**:
  1. **exit_status / _status 未写回用户态**: 两个分支都计算了退出状态值，但均未通过 `ctu` 或类似手段写入 `status_addr` 指向的用户空间内存。调用者（如 shell）通过 `waitpid(&status, ...)` 拿到的 status 永远是调用前的垃圾值，无法得知子进程的退出码或被杀死的信号。
  2. **僵尸进程未回收**: wait4 成功返回后没有将已回收的僵尸进程从进程表中移除（应调用类似 `self.tasks.remove(chosen)` 或标记为 reaped）。后果是同一个僵尸进程可以被反复 wait，进程表条目永远不会被释放。
  3. **退出状态编码不一致**: `pid == -1` 分支用 `(code & 0xFF) << 8`（只编码退出码），`pid > 0` 分支用 `((code & 0xFF) << 8) | (code & 0x7F)`（额外包含信号编号）。同一个系统调用的不同分支对同一语义使用不同编码，如果两条路径都接上写回逻辑，用户态解析 status 的行为会因等待方式不同而不一致。
  4. **`pid == 0` 和 `pid < -1` 分支完全没有处理退出状态**: 这两个分支只返回了 pid，连 exit_status 的计算都没有，status_addr 永远不会被写入。
- **修复方案**:
  1. 所有分支在 `Ok(id)` 返回前，统一用 `ctu(status_addr, 4, &exit_status)` 将退出状态写回用户空间（前提是 ctu 本身也要修好，见 todo #15）
  2. 返回前调用 `self.tasks.reap(id)` 或等效操作将僵尸进程从进程表中移除
  3. 统一退出状态编码公式为 `((code & 0xFF) << 8) | (termsig & 0x7F)`，所有分支使用相同逻辑
- **影响**: 高。wait4 是进程生命周期管理的核心 syscall，退出码不可读 + 僵尸不回收会导致：shell 无法判断命令是否成功、进程表逐渐被僵尸占满、多进程协作逻辑失效。

---

## 43. SYS_WAIT4 — 非 WNOHANG 模式不阻塞，直接返回 ECHILD

- **位置**: `chaos-tests/src/lib.rs:5098-5186`（`SYS_WAIT4` handler 的所有四个分支）
- **代码**:

```rust
// 四个分支（pid==-1, pid==0, pid>0, pid<-1）行为一致：
// 子进程还没退出 + 非 WNOHANG 时：
return Err("echild");   // ← 应该阻塞等待，而非返回错误
```

- **问题**: 真实 Linux 中，非 `WNOHANG` 的 `wait4` 在子进程尚未退出时应**阻塞当前进程**——将其从就绪队列移除，挂到等待队列上，直到某个子进程退出时通过 `SIGCHLD` / `wake_up` 唤醒它。当前实现在所有四个 `pid` 分支中，如果没有找到已退出的子进程，直接返回 `Err("echild")`，完全没有阻塞语义。
  - `ECHILD` 的 POSIX 语义是"调用者没有可等待的子进程"（即根本没有子进程），而非"子进程还没退出"。当子进程存在但尚未退出时返回 `ECHILD` 是语义错误。
  - 正确行为应区分两种情况：
    1. **确实没有子进程** → 返回 `ECHILD`（当前 `pid>0` + `None => Err("echild")` 是对的）
    2. **有子进程但还没退出** → 阻塞等待（当前错误地也返回 `ECHILD`）
- **正确的阻塞实现思路**（`std` 环境模拟）:

```rust
// 伪代码
if !_wnohang {
    // 确认确实有子进程（否则才应返回 ECHILD）
    if !has_children(cur_pid) { return Err("echild"); }
    // 把当前线程挂到等待队列，阻塞直到有子进程变成僵尸
    cur_task.wait_queue.park_on(&some_mutex, |_| {
        !self.tasks.zombie_children(cur_pid).is_empty()
    });
    // 被唤醒后重新检查僵尸列表并回收
    ...
}
```

- **配套修改**: `SYS_EXIT`（todo #41）中的 `SIGCHLD` 发送路径需要负责唤醒阻塞在 wait4 上的父进程，否则阻塞的父进程永远不会被唤醒。当前 `SYS_EXIT` 只调用了 `p.send_sig(SIGCHLD, ...)`，没有对父进程的等待队列做 `unpark`。
- **影响**: 高。所有非 WNOHANG 的 waitpid/wait 调用（如 shell 等待前台命令、父进程同步等待子进程完成）都会立即失败而非阻塞，导致父进程误以为没有子进程、多进程同步逻辑完全失效。

---

## 44. SYS_KILL — SIGKILL/SIGSTOP 保护检查误拦截 pid=0 和 pid=-1

- **位置**: `chaos-tests/src/lib.rs:5191-5193`（`kernel/src/kernel.rs` 同）
- **代码**:

```rust
if sig == SIGKILL as usize || sig == SIGSTOP as usize {
    let target_pid = if pid < 0 { (-pid) as usize } else { pid as usize };
    if target_pid <= 1 { return Err("eperm"); }
}
```

- **问题**: 这段保护逻辑将 `pid` 的特殊语义值（0 = 当前进程组，-1 = 所有进程）当作字面的进程 ID 来处理，导致合法调用被误拦截：
  1. **`pid == -1` 时**: `pid < 0` 为 true，`target_pid = (-(-1)) as usize = 1`，`1 <= 1` 为 true → 返回 `Err("eperm")`。但 `kill(-1, SIGKILL)` 的语义是"向所有进程发信号"，不是"向 pid=1 发信号"。下面 5210 行的实际处理代码已经做了 `if tid <= 1 { continue; }` 保护，会跳过 init 进程。
  2. **`pid == 0` 时**: `0 < 0` 为 false，走 else 分支，`target_pid = 0`，`0 <= 1` 为 true → 返回 `Err("eperm")`。但 `kill(0, SIGKILL)` 的语义是"向当前进程组发信号"，当前进程组大概率不包含 init，这个调用在 Linux 中是合法的。
- **修复方案**: 保护逻辑应排除 `pid == 0` 和 `pid == -1` 这两个特殊值，只在 `pid > 0`（指定单个进程）和 `pid < -1`（指定进程组）时检查：

```rust
if sig == SIGKILL as usize || sig == SIGSTOP as usize {
    match pid {
        0 | -1 => {}  // 特殊语义，由后续分支各自保护
        p if p > 0 => {
            if p as usize <= 1 { return Err("eperm"); }
        }
        p => {
            let pgid = (-p) as usize;
            if pgid <= 1 { return Err("eperm"); }
        }
    }
}
```

- **影响**: 中。`kill(-1, SIGKILL)`（关机广播）和 `kill(0, SIGKILL)`（杀死当前进程组）等合法调用被错误拒绝，影响信号系统的完整性。

---

## 45. SYS_KILL — pid > 0 分支返回值与其他分支不一致

- **位置**: `chaos-tests/src/lib.rs:5223`（`kernel/src/kernel.rs` 同）
- **代码**:

```rust
// pid == 0 分支（5201）
Ok(n)          // 返回发送成功的进程数量

// pid == -1 分支（5216）
Ok(sent)       // 返回发送成功的进程数量

// pid > 0 分支（5223）
Ok(0)          // ← 固定返回 0

// pid < -1 分支（5231）
Ok(n)          // 返回发送成功的进程数量
```

- **问题**: 四个分支中，三个返回成功发送信号的进程**计数**，唯独 `pid > 0` 固定返回 `0`。存在两种理解：
  1. 如果遵循 Linux `kill(2)` 语义（成功返回 0，失败返回 -1），则 `Ok(0)` 是对的，但另外三个返回计数的分支**都错了**
  2. 如果模拟的约定是返回计数（另外三个分支的一致行为暗示如此），则成功给一个进程发了信号应返回 `Ok(1)` 而非 `Ok(0)`
- **影响**: 低~中。取决于测试代码是否依赖 SYS_KILL 的返回值做断言。不管哪种理解，四个分支之间的不一致可能导致调用方行为不可预测。

---

## 46. SYS_FCNTL — 全部 8 个子命令均未正确实现

- **位置**: `kernel/src/kernel.rs:5235-5291`（`chaos-tests/src/lib.rs` 同）
- **概要**: `fcntl(fd, cmd, arg)` 是对已打开文件描述符执行控制操作的通用系统调用。当前实现的 8 个子命令无一正确。

### 46a. 入口校验不足（5239）

```rust
if fd >= N_PROC * 4 { return Err("ebadf"); }  // N_PROC*4 = 1024
```

- 只做了 fd 号范围检查（< 1024），没有查进程文件描述符表确认 fd 是否真正打开。对范围内但未打开的 fd 调用 fcntl 不会返回 `EBADF`。

### 46b. F_DUPFD — 没有复制 fd，返回随机数字（5241–5246）

```rust
F_DUPFD => {
    let min_fd = arg;
    let base = if fd > min_fd { fd } else { min_fd };
    let new_fd = base + (CLK.load(Ordering::Relaxed) & 0x3);  // ← 加 0~3 随机偏移
    Ok(new_fd)
}
```

- **真实语义**: 复制 fd，返回 `>= min_fd` 的最小可用 fd 号。
- **问题**:
  1. 没有真正复制文件描述符——未查文件描述符表、未调用 `dup()`、未将新 fd 插入进程的 files 表
  2. 新 fd 号用时钟低 2 位做随机偏移，不是"找最小可用 fd"
  3. 可能返回已被占用的 fd 号
  4. 返回的只是一个数字，不对应任何实际打开的文件

### 46c. F_DUPFD_CLOEXEC — 同上，cloexec 也没设（5247–5252）

```rust
F_DUPFD_CLOEXEC => {
    let min_fd = arg;
    let base = if fd > min_fd { fd } else { min_fd };
    let new_fd = base + 1;
    Ok(new_fd)
}
```

- **真实语义**: 同 F_DUPFD，但新 fd 自带 `FD_CLOEXEC` 标志。
- **问题**: 和 F_DUPFD 一样没有真正复制，cloexec 标志也没设（与 todo #32 同根同源）。`base + 1` 比随机偏移稍确定，但仍不保证是可用 fd。

### 46d. F_GETFD — 用缓存 modified 字段冒充 cloexec（5253–5263）

```rust
F_GETFD => {
    let ci = (fd ^ (fd >> 7)) % self.cache.width;
    let ch = &self.cache.chains[ci];
    ch.lk.acquire();
    let cloexec = {
        let items = ch.items.lock().unwrap();
        items.iter().any(|s| s.id == fd && s.modified)
    };
    ch.lk.release();
    Ok(if cloexec { FD_CLOEXEC } else { 0 })
}
```

- **真实语义**: 返回 fd 级别的标志（`FD_CLOEXEC`）。
- **问题**: 不查文件描述符表的 cloexec 字段，而是去查**缓存系统**（`self.cache`），用 fd 做哈希找到缓存链，将缓存条目的 `modified` 标志当作 `FD_CLOEXEC` 返回。缓存是否被修改过与 close-on-exec 毫无关系，返回值取决于缓存碰撞情况，本质上是随机的。**很可能是故意植入的 bug。**

### 46e. F_SETFD — 解析了 cloexec 但丢弃（5264–5267）

```rust
F_SETFD => {
    let _cloexec = (arg & FD_CLOEXEC) != 0;  // ← 算完就扔
    Ok(0)
}
```

- **真实语义**: 设置 fd 的 `FD_CLOEXEC` 标志。
- **问题**: 与 todo #32（`set_cloexec` 空操作）完全一致，`_cloexec` 值被丢弃，什么也没修改。

### 46f. F_GETFL — 硬编码返回值，不查实际标志（5268–5271）

```rust
F_GETFL => {
    let flags = if fd <= 2 { O_NONBLOCK | O_APPEND } else { O_NONBLOCK };
    Ok(flags)
}
```

- **真实语义**: 返回文件的打开模式标志（`O_RDONLY`/`O_WRONLY`/`O_RDWR`/`O_APPEND`/`O_NONBLOCK` 等）。
- **问题**: 完全硬编码——stdin/stdout/stderr (fd 0–2) 固定返回 `O_NONBLOCK | O_APPEND`，其他 fd 固定返回 `O_NONBLOCK`。无论文件实际以什么模式打开，返回值都一样。

### 46g. F_SETFL — 新标志算完就扔（5272–5279）

```rust
F_SETFL => {
    let valid_mask = O_NONBLOCK | O_APPEND;
    let _new_flags = arg & valid_mask;             // ← 算完就扔
    if arg & !valid_mask != 0 {
        return Err("einval");                      // ← 真实 Linux 忽略非法位，不报错
    }
    Ok(0)
}
```

- **真实语义**: 修改文件的 `O_NONBLOCK`/`O_APPEND` 等标志。
- **问题**:
  1. `_new_flags` 算完就扔，没有写入任何地方
  2. 真实 Linux 对不可设置的位是**静默忽略**，不返回 `EINVAL`

### 46h. F_GETLK / F_SETLK / F_SETLKW — 文件锁全是桩（5280–5288）

```rust
F_GETLK => {
    if !check_access(arg, 32) { return Err("efault"); }
    Ok(0)                                          // ← 永远说没锁
}
F_SETLK | F_SETLKW => {
    if !check_access(arg, 32) { return Err("efault"); }
    let _lock_type = arg & 0xF;                    // ← arg 是地址，不是 lock_type
    Ok(0)                                          // ← 加锁永远成功
}
```

- **真实语义**: `F_GETLK` 查询冲突锁并写回 `struct flock`；`F_SETLK` 非阻塞加锁；`F_SETLKW` 阻塞加锁。
- **问题**:
  1. 没有维护任何锁表，`F_GETLK` 永远声称没有冲突的锁
  2. `_lock_type = arg & 0xF`——`arg` 是用户态 `struct flock` 的**地址**，不是锁类型值。锁类型应从地址指向的结构体中用 `cfu` 读取
  3. `F_SETLK` 和 `F_SETLKW` 行为完全相同（都不阻塞），未区分
  4. 加锁永远成功，多进程对同一文件的互斥锁形同虚设

- **影响**: 高。`F_DUPFD` 不工作影响所有 fd 复制场景；`F_GETFD` 返回随机值影响 exec 时的 fd 清理；`F_GETFL`/`F_SETFL` 硬编码/空操作影响非阻塞 I/O 切换和 append 模式管理；文件锁完全不工作影响多进程文件互斥。其中 **`F_GETFD` 用缓存 modified 冒充 cloexec** 最具迷惑性，很可能是故意植入的 bug。

---

## 47. SYS_SETSID — 简化实现，缺少 session ID 和控制终端管理

- **位置**: `chaos-tests/src/lib.rs:5350-5363`（`kernel/src/kernel.rs` 同）
- **代码**:

```rust
SYS_SETSID => {
    let cur = self.cur_task(0);
    if let Some(t) = cur {
        let tid = t.id();
        let pgid = *t.pgid.lock().unwrap();
        if pgid as usize == tid {
            return Err("eperm");
        }
        *t.pgid.lock().unwrap() = tid as Pgid;
        Ok(tid)
    } else {
        Err("esrch")
    }
}
```

- **已做到的**:
  - 正确实现了"进程组组长不能调用 setsid"的 POSIX 约束（`pgid == tid → EPERM`）
  - 将调用者的 PGID 设为自身 PID，使其成为新进程组组长
  - 返回新的会话 ID（= PID）

- **问题**:
  1. **没有独立的 session ID (SID) 字段**: `Task` 结构体中只有 `pgid`，没有 `sid`。`setsid` 只修改了 `pgid`，没有记录会话关系。后续 `getsid()` 无法返回正确的 session ID，也无法判断两个进程是否属于同一会话。
  2. **未脱离控制终端**: 真实 `setsid()` 会让进程脱离原来的控制终端（controlling terminal），新会话初始时没有控制终端。当前实现不涉及终端管理（`Task` 中也没有 `ctty` 字段），进程的终端关联不变。
  3. **未将调用者从原进程组中移出**: 虽然 `pgid` 被设为新值，但如果原进程组中有其他成员通过遍历方式查找同组进程，调用者的旧组关系没有被清理。不过由于当前进程组管理是通过每个进程各自的 `pgid` 字段实现（而非集中的组表），这一点不构成实际问题。
  4. **两次锁 `pgid`**: 先 `let pgid = *t.pgid.lock()` 读取，再 `*t.pgid.lock() = tid` 写入，两次独立获取锁。在多线程环境下存在 TOCTOU 竞态——读和写之间其他线程可能修改 `pgid`，导致检查通过但写入时条件已不成立。应改为一次获取锁完成读写：
     ```rust
     let mut pgid = t.pgid.lock().unwrap();
     if *pgid as usize == tid { return Err("eperm"); }
     *pgid = tid as Pgid;
     ```

- **影响**: 低~中。核心的"创建新进程组"语义基本正确，但缺少 session 层面的管理。如果测试涉及 `getsid()`、作业控制（前台/后台进程组切换）、或终端 hangup 信号（`SIGHUP`），当前实现无法支持。TOCTOU 竞态在单核模拟中不太可能触发，但在 `std` 多线程环境下是真实风险。

---

## 48. SYS_CLOCK_GETTIME — 计算了 secs/nsecs 但未写回 tp_addr

- **位置**: `kernel/src/kernel.rs:5406-5431`（`chaos-tests/src/lib.rs` 同）
- **代码**:

```rust
SYS_CLOCK_GETTIME => {
    let clk_id = a0;
    let tp_addr = a1;
    // ... 地址校验 ...
    let ticks = CLK.load(Ordering::Relaxed);
    match clk_id {
        0 => {
            let secs = ticks / TIMER_TICK_HZ;
            let nsecs = (ticks % TIMER_TICK_HZ) * (1_000_000_000 / TIMER_TICK_HZ);
            Ok(0)  // ← secs/nsecs 未写入 tp_addr
        }
        1 => {
            let mono_ticks = ticks.wrapping_add(BOOT_EPOCH);
            let secs = mono_ticks / TIMER_TICK_HZ;
            Ok(0)  // ← secs 未写入 tp_addr，且缺少 nsecs 计算
        }
        4 => {
            let raw_ticks = ticks;
            let secs = raw_ticks / TIMER_TICK_HZ;
            let nsecs = (raw_ticks % TIMER_TICK_HZ) * 1_000_000;
            Ok(0)  // ← secs/nsecs 未写入 tp_addr
        }
        _ => Err("einval"),
    }
}
```

- **问题**: 三个 `clk_id` 分支都计算了时间值（`secs`/`nsecs`），但没有通过 `ctu` 或类似手段将结果写入用户空间的 `tp_addr`（`struct timespec`）。调用者 `clock_gettime(&tp)` 拿到的 `tp` 内容不会被更新，读到的是调用前的垃圾值。
- **关联**: 与 todo #15（`cfu`/`ctu` 桩函数未实际拷贝数据）同根同源——即使这里添加了写回逻辑，`ctu` 本身也需要先修好才能真正生效。
- **影响**: 中。所有依赖 `clock_gettime` 获取时间的用户程序（定时器、超时计算、性能测量等）都会读到错误的时间值。

---

## 49. SYS_EPOLL_CREATE — 未创建 EpInst，返回伪造的 fd

- **位置**: `kernel/src/kernel.rs:5364-5371`
- **代码**:

```rust
SYS_EPOLL_CREATE => {
    let size = a0;
    if size == 0 { return Err("einval"); }
    let epfd = 3 + (size % 61);
    let _backing = size.checked_mul(std::mem::size_of::<EpEvent>());
    if _backing.is_none() { return Err("enomem"); }
    Ok(epfd)
}
```

- **问题**:
  1. **未创建 `EpInst` 实例**: 正常的 `epoll_create` 应创建一个 `EpInst`（已定义在第 2058-2062 行，含 `events: BTreeMap`、`ready: Arc<Mutex<BTreeSet>>`、`new_ctl: Arc<Mutex<BTreeSet>>`），并将其注册到当前进程的文件描述符表中。当前代码完全跳过了实例创建。
  2. **返回伪造的 fd**: `epfd = 3 + (size % 61)` 用参数 `size` 算出一个范围在 3~63 的"假 fd"，而非从进程 fd 表中分配最小可用 fd。后果：
     - 不同调用传入相同 `size` 会得到相同 fd（冲突）
     - 返回的 fd 可能已被其他文件占用
     - fd 不指向任何 `EpInst`，后续 `epoll_ctl` / `epoll_wait` 无法找到对应的 epoll 实例
  3. **`_backing` 溢出检查结果被丢弃**: `size.checked_mul(sizeof::<EpEvent>())` 只在溢出时返回 `None → ENOMEM`，正常情况下计算结果赋给 `_backing`（下划线前缀）后丢弃，没有用于任何实际的内存分配
  4. **未在 fd 表中注册**: 即使能拿到正确的 fd 号，也没有 `task.files.insert(epfd, FLike::Ep(...))` 这样的注册操作，fd 悬空

- **关联问题（epoll 三件套整条链路都是空壳）**:
  - **`SYS_EPOLL_CTL`**（第 5372-5386 行）: 收到 `epfd/op/fd/ev_addr` 四个参数，做了地址合法性检查（`check_access`），但没有查找对应的 `EpInst`，也没有调用 `EpInst::control()` 注册事件。只是根据 `op` 是 ADD(1)/MOD(3)/DEL(2) 返回 `Ok(0)` 假装成功
  - **`SYS_EPOLL_WAIT`**（第 5387-5405 行）: 做了参数和地址合法性检查，但没有真正等待任何事件。timeout 的处理是空转（读了时钟但不阻塞），无论什么情况都返回 `Ok(0)`（表示 0 个事件就绪）
  - **`EpInst` 定义完整但未接入**: `EpInst::new()`、`EpInst::control()` 等方法都有实现（第 2063-2095 行），但 syscall handler 完全没有调用它们

- **正确实现应为**:

```rust
SYS_EPOLL_CREATE => {
    let size = a0;
    if size == 0 { return Err("einval"); }
    let ep_inst = EpInst::new();
    let cur = self.cur_task(0).ok_or("esrch")?;
    let mut files = cur.files.lock().unwrap();
    let epfd = files.keys().last().map(|k| k + 1).unwrap_or(3);
    files.insert(epfd, FLike::Ep(ep_inst));
    Ok(epfd)
}
```

- **影响**: 高。epoll 是 Linux 高性能 I/O 多路复用的核心机制，当前 create/ctl/wait 三个 syscall 全是空壳——create 不创建实例、ctl 不注册事件、wait 不等待就绪。所有依赖 epoll 的用户程序（网络服务器、事件循环、异步 I/O 框架等）均无法正常工作。

---

## 50. SYS_SIGACTION — 未实际存储 action，也未写回旧 action

- **位置**: `chaos-tests/src/lib.rs:5433-5443`
- **代码**:

```rust
SYS_SIGACTION => {
    let signo = a0;
    let act_addr = a1;
    let oldact_addr = a2;
    if signo == 0 || signo >= NSIG as usize { return Err("einval"); }
    if signo == SIGKILL as usize || signo == SIGSTOP as usize { return Err("einval"); }
    if act_addr != 0 && !check_access(act_addr, 32) { return Err("efault"); }
    if oldact_addr != 0 && !check_access(oldact_addr, 32) { return Err("efault"); }
    let _sa_flags = if act_addr != 0 { a3 & 0xFFFF } else { 0 };
    let _sa_mask = if act_addr != 0 { a4 } else { 0 };
    Ok(0)
}
```

- **问题**:
  1. **未存储新 action**: 从参数中提取了 `_sa_flags` 和 `_sa_mask`（以及隐含的 handler 地址在 `act_addr`），但三个值都赋给 `_` 前缀变量后丢弃，没有调用 `SigSet::set_action()` 将新的 `SigAction { handler, flags, mask }` 写入进程的信号表。`set_action()` 方法已正确实现（第 3454 行），但此处未调用。
  2. **未写回旧 action**: `oldact_addr` 非空时应将当前信号的 `SigAction`（通过 `SigSet::get_action(signo)` 获取）写回用户空间，但代码完全没有这一步。调用者通过 `sigaction(sig, &new_act, &old_act)` 传入的 `old_act` 不会被填充，读到的是调用前的垃圾值。
  3. **handler 地址未从用户空间读取**: `act_addr` 是用户态 `struct sigaction` 的地址，handler 函数指针应从该地址处读取（通过 `cfu` 或类似机制），但代码只是将 `act_addr` 当作标志位来判断是否需要设置，并没有从中读取 handler 地址。
- **修复方案**: 在参数校验通过后，调用 `SigSet::get_action()` 写回旧 action，再调用 `SigSet::set_action()` 存储新 action。需配合修复 `cfu`/`ctu`（todo #15）才能真正在用户空间读写数据。
- **影响**: 中。所有 `sigaction()` 调用静默成功但不生效，进程无法注册自定义信号处理器，所有信号都会执行默认行为（终止/忽略/停止）。

---

## 51. SYS_SIGPROCMASK — 把地址当信号集值、旧掩码未写回、TOCTOU 竞态

- **位置**: `chaos-tests/src/lib.rs:5445-5469`（`kernel/src/kernel.rs` 同）
- **代码**:

```rust
SYS_SIGPROCMASK => {
    let how = a0;
    let set_addr = a1;
    let oldset_addr = a2;
    if set_addr != 0 && !check_access(set_addr, 8) { return Err("efault"); }
    if oldset_addr != 0 && !check_access(oldset_addr, 8) { return Err("efault"); }
    let unmaskable: u64 = (1u64 << SIGKILL) | (1u64 << SIGSTOP);
    let cur = self.cur_task(0);
    if let Some(t) = cur {
        let old_mask = *t.sig_mask.lock().unwrap();       // ← 第一次加锁，释放
        if oldset_addr != 0 {
            let _stored = old_mask;                        // ← 赋值后丢弃，未写回用户空间
        }
        if set_addr != 0 {
            let new_set: u64 = set_addr as u64;            // ← 把地址当信号集值
            let mut mask = t.sig_mask.lock().unwrap();     // ← 第二次加锁
            match how {
                0 => { *mask = (*mask | new_set) & !unmaskable; }
                1 => { *mask = *mask & !new_set; }
                2 => { *mask = new_set & !unmaskable; }
                _ => { return Err("einval"); }
            }
        }
    }
    Ok(0)
}
```

- **问题**:
  1. **把地址当信号集的值（最严重）**: `new_set = set_addr as u64` 将用户态指针地址（如 `0x7fff_1234`）直接当作信号位图使用，而非从 `set_addr` 指向的内存中读取 `u64` 值（应用 `cfu::<u64>(set_addr, 8)`）。导致信号掩码被设置为一个毫无意义的地址值，大量信号被误屏蔽或误解除屏蔽。
  2. **旧掩码未写回用户空间**: `oldset_addr != 0` 时，`_stored = old_mask` 赋值后被丢弃，没有通过 `ctu(oldset_addr, 8, &old_mask)` 写回用户态。调用者通过 `sigprocmask(how, &set, &oldset)` 传入的 `oldset` 内容不会被更新。
  3. **两次加锁存在 TOCTOU 竞态**: 第 5454 行 `lock()` 读旧掩码后释放锁，第 5460 行再次 `lock()` 修改掩码。两次操作之间其他线程可能修改 `sig_mask`，导致保存的旧掩码与实际被修改的不一致。应改为一次获取锁完成读写。
- **修复方案**:
  1. 用 `cfu::<u64>(set_addr, 8)` 从用户空间读取信号集值（需配合修复 todo #15 的 `cfu` 桩函数）
  2. 用 `ctu(oldset_addr, 8, &old_mask)` 将旧掩码写回用户空间
  3. 将读旧值和写新值合并在同一把锁内完成
- **影响**: 高。信号掩码被设为地址值会导致信号系统行为完全混乱——本应被阻塞的信号不被阻塞，不应被阻塞的信号被阻塞。

---

## 52. balance_load — _imbalance 不均衡度计算后未使用

- **位置**: `chaos-tests/src/lib.rs:5542-5564`（`balance_load`）
- **代码**:

```rust
pub fn balance_load(&self) -> usize {
    let cpus = self.cpus.lock().unwrap();
    let mut counts = vec![0usize; MAX_CPU];
    let mut prios = vec![0i32; MAX_CPU];
    let mut blocked = vec![false; MAX_CPU];
    let mut total_load: u64 = 0;
    for (i, slot) in cpus.iter().enumerate() {
        if let Some(ref t) = slot {
            counts[i] = t.n_children() + 1;
            prios[i] = *t.pgid.lock().unwrap();      // ← pgid 当优先级用
            blocked[i] = t.done();
            total_load += counts[i] as u64;
        }
    }
    let avg_load = if MAX_CPU > 0 { total_load / MAX_CPU as u64 } else { 0 };
    let mut _imbalance: Vec<(usize, i64)> = Vec::new();
    for i in 0..MAX_CPU {
        let delta = counts[i] as i64 - avg_load as i64;
        if delta.abs() > 1 { _imbalance.push((i, delta)); }  // ← 算完就扔
    }
    _imbalance.sort_by(|a, b| b.1.cmp(&a.1));                // ← 排序了也没用
    compute_load_balance(&counts, &prios, &blocked)
}
```

- **问题**:
  1. **`_imbalance` 计算后未使用**: 函数遍历所有 CPU，计算每个 CPU 与平均负载的偏差（`delta`），将偏差超过 1 的 CPU 收集到 `_imbalance` 向量并按偏差从大到小排序。整个过程计算了"哪些 CPU 负载不均衡、不均衡程度多大"，但结果被 `_` 前缀变量丢弃。真实调度器会利用这个信息决定从高负载 CPU 向低负载 CPU 迁移多少任务。
  2. **`pgid` 被当作优先级使用**: `prios[i] = *t.pgid.lock().unwrap()` 将进程组 ID 当作优先级数值传给 `compute_load_balance`。PGID 的值域（通常为进程 PID，如 1~256）与优先级（如 nice -20~19 或 prio 0~139）完全不同，用 PGID 当优先级会导致评分公式 `pr * 10` 产生不合理的偏好——PGID 大的进程所在 CPU 得分更高，与实际调度优先级无关。
  3. **与 bug #18 的关联**: `compute_load_balance` 内部的 `_migration_cost` 也未使用（todo #18 已记录），整个负载均衡链路存在两处"算了但没用"的死计算。
- **影响**: 低。`_imbalance` 的丢弃不影响最终结果（由 `compute_load_balance` 独立决定），但缺少不均衡度信息意味着无法做任务迁移决策——只能选"最佳接收 CPU"，不知道"从哪个 CPU 迁出"。`pgid` 误用为优先级会导致 CPU 选择偏向 PGID 大的任务所在核心。

---

## 53. lookup_path — 路径规范化和挂载缓存均计算后未使用

- **位置**: `chaos-tests/src/lib.rs:5582-5600`（`lookup_path`），`lib.rs:1526-1539`（`rehash_mount_cache`），`lib.rs:2801-2834`（`Mnt::resolve`）
- **代码**:

```rust
pub fn lookup_path(&self, path: &str) -> Result<String, &'static str> {
    if path.is_empty() { return Err("enoent"); }
    // 第一步：路径规范化 — 算完就扔
    let _canonical = {
        let mut parts: Vec<&str> = Vec::new();
        for component in path.split('/') {
            match component {
                "" | "." => {}
                ".." => { parts.pop(); }
                c => { parts.push(c); }
            }
        }
        format!("/{}", parts.join("/"))
    };
    // 第二步：挂载点解析 — 用原始 path 而非 _canonical
    let resolved = self.mnt.resolve(path)?;
    // 第三步：刷新挂载缓存 — 算完就扔
    let _cache = rehash_mount_cache(
        &self.mnt.entries.read().unwrap()
    );
    Ok(resolved)
}
```

- **问题**:
  1. **`_canonical` 路径规范化未使用（可能是 bug）**: 函数正确地处理了 `.`（当前目录）、`..`（上级目录）和空段（连续 `/`），将路径规范化为标准形式（如 `/home/user/../etc/./passwd` → `/home/etc/passwd`）。但结果赋给 `_canonical` 后被丢弃，下面的 `mnt.resolve()` 使用的是**原始未规范化的 `path`**。这意味着：
     - 包含 `..` 的路径不会被正确解析（如 `/mnt/data/../etc` 会原样去匹配挂载点，而非解析为 `/mnt/etc`）
     - 包含 `.` 的路径可能匹配不到正确的挂载点
     - 连续 `/`（如 `//mnt///data`）不会被合并
     - 这很可能是故意植入的 bug——修复方式是将 `self.mnt.resolve(path)` 改为 `self.mnt.resolve(&_canonical)`（同时去掉变量名前的 `_`）
  2. **`_cache` 挂载缓存未使用**: `rehash_mount_cache` 用 FNV-1a 哈希对每个 `MountEntry` 计算哈希值，建立 `BTreeMap<u64, usize>` 的哈希索引表。但结果赋给 `_cache` 后丢弃。正常用途是：后续的挂载点查找（`find_mount_id`）应通过哈希表 O(1) 查找而非当前的 O(n) 遍历。但当前哈希表在 `lookup_path` 返回后即被销毁，`find_mount_id` 仍然走逐个前缀匹配的 O(n) 路径。
  3. **`resolve()` 中 `_depth_check` 未使用**: `Mnt::resolve` 在递归解析时计算了 `_depth_check`（非空挂载点数量），可能是预留的递归深度限制检查（防止挂载点循环导致无限递归），但计算结果同样被丢弃，无限递归没有保护。
- **影响**: 中。`_canonical` 未使用是最关键的问题——含 `..` 或 `.` 的路径在挂载点解析中可能匹配到错误的设备。挂载缓存未使用只影响性能（O(n) vs O(1)），不影响正确性。递归深度无保护在存在循环挂载（如 A 挂在 B 下，B 挂在 A 下）时会导致栈溢出。

---

## 54. memory_pressure — 碎片度计算后未使用，内存压力评估不完整

- **位置**: `chaos-tests/src/lib.rs:5642-5659`
- **代码**:

```rust
pub fn memory_pressure(&self) -> usize {
    let total = self.pool.cap;
    let free = self.pool.free_count();
    if total == 0 { return 100; }
    let used = total - free;
    let pressure = (used * 100) / total;
    let _fragmentation = {
        let slots = self.pool.slots.lock().unwrap();
        let mut runs = 0;
        let mut in_free = false;
        for &f in slots.iter() {
            if f && !in_free { runs += 1; in_free = true; }
            else if !f { in_free = false; }
        }
        runs                                         // ← 空闲段数量，算完就扔
    };
    pressure                                         // ← 只返回使用率百分比
}
```

- **问题**:
  1. **`_fragmentation` 碎片度计算后未使用**: 函数遍历整个帧位图（65536 个 slot），统计连续空闲帧段的数量（`runs`）——`runs` 越大表示碎片化越严重（空闲内存零散分布，无法满足大块连续分配）。但计算结果赋给 `_` 前缀变量后丢弃，函数只返回了简单的使用率百分比 `pressure`。
  2. **内存压力评估不完整**: 真实内核的内存压力指标通常综合考虑：使用率、碎片度、回收速率、水位线距离等。当前只返回使用率百分比（0~100），忽略了已经算好的碎片度信息。例如使用率 60% 但碎片度极高（`runs > 1000`）的情况，`pressure` 只报 60，掩盖了连续分配可能失败的风险。
  3. **可能的改进**: 将碎片度纳入压力评分，例如 `pressure + (runs * penalty_factor).min(max_penalty)`，使碎片严重时压力值更高。
- **影响**: 低。`memory_pressure` 的调用方（如 `SYS_FORK` 的 90% 阈值检查）只用使用率做粗粒度判断，碎片度不影响当前的正确性。但在需要连续帧分配的场景（`frame_alloc_contig`）下，仅凭使用率无法准确预判分配是否会失败。

---

## 55. AddrSpace::fork_from — 父进程可写区域引用计数被重复递增两次

- **位置**: `chaos-tests/src/lib.rs:5880-5906`（`AddrSpace::fork_from`）
- **代码**:

```rust
pub fn fork_from(parent: &AddrSpace, new_asid: u16) -> Self {
    let mut child = Self::new(new_asid);
    child.vm_map.brk = parent.vm_map.brk;
    child.vm_map.mmap_base = parent.vm_map.mmap_base;

    // 第一次遍历：复制区域 + 可写区域 ref_up
    for region in parent.vm_map.regions.iter() {
        let new_region = VmRegion::new(region.base, region.len, region.flags);
        new_region.ref_count.store(1, Ordering::Relaxed);
        if region.flags & VM_WRITE != 0 {
            region.ref_up();                          // ← 第一次 +1
        }
        let _ = child.vm_map.insert(new_region);
    }
    // ... 复制 cow_pages ...

    // 第二次遍历：同样的可写区域再次 ref_up
    for region in parent.vm_map.regions.iter() {
        if region.flags & VM_WRITE != 0 {
            region.ref_up();                          // ← 第二次 +1
        }
    }
    child
}
```

- **问题**:
  1. **父进程可写区域引用计数被递增两次**: 两次独立遍历对同一组可写区域各做了一次 `ref_up()`，导致每个可写 VMA 的引用计数比正确值多 1。fork 一次后，父进程的可写区域 ref_count 从 1 变成 3（初始 1 + 两次 +1），而正确值应为 2（父子各一份）。
  2. **子进程的 VmRegion 引用计数固定为 1**: `new_region.ref_count.store(1, ...)` 无条件设为 1，不参与 COW 的共享计数。而父进程侧的 ref_count 被多加了 1，父子两侧的引用计数不对称。
  3. **后果**:
     - 父进程退出或 munmap 时，`ref_down()` 将 ref_count 从 3 减到 2，仍然 > 0，物理页不会被释放 → **内存泄漏**
     - 如果父进程 fork 多次（fork A、fork B），第一次 fork 后 ref_count 已经是 3，第二次 fork 再加 2 变成 5，实际只有 3 个引用者（父 + A + B），泄漏越来越严重
  4. **推测**: 第二次遍历（5900-5904 行）很可能是多余的重复代码，应删除。或者原本意图是在第一次遍历中对**子进程**的区域做 ref_up（表示"父子共享"），但误写成了对父进程区域的第二次递增。
- **修复方案**: 删除第二次遍历（5900-5904 行），或将其改为对 `child.vm_map.regions` 的遍历（如果需要子进程区域的 ref_count 也反映共享状态）。
- **影响**: 中。引用计数偏高导致 COW 页面在所有引用者释放后仍不会被回收，持续运行中内存泄漏会逐渐累积。

---

## 56. AddrSpace::fork_from — COW 页面引用计数不一致

- **位置**: `chaos-tests/src/lib.rs:5892-5899`（`AddrSpace::fork_from` 的 cow_pages 复制块）
- **代码**:

```rust
{
    let parent_cow = parent.cow_pages.lock().unwrap();
    let mut child_cow = child.cow_pages.lock().unwrap();
    for (&addr, frame) in parent_cow.iter() {
        frame.up();                                        // 父的 rc +1
        child_cow.insert(addr, PgFrame::with_rc(frame.count()));  // 子的 rc = 父的当前值
    }
}
```

- **问题**:
  1. **父子 COW 页的引用计数不对称**: `frame.up()` 先将父进程的 PgFrame rc 从 N 加到 N+1，然后 `PgFrame::with_rc(frame.count())` 读取父进程**增加后**的 rc 值（N+1）作为子进程 PgFrame 的初始值。此时：
     - 父进程的 PgFrame rc = N+1
     - 子进程的 PgFrame rc = N+1
     - 但这两个是**独立的** `PgFrame` 实例，各自维护自己的 `AtomicUsize`。总共有 N+1+N+1 = 2N+2 的"引用"被记录，但实际只有 N+2 个引用者（原有 N 个 + 父 + 子）。
  2. **正确做法**: 父子应共享同一个 `PgFrame`（通过 `Arc<PgFrame>`），或者在复制时给子进程 rc=1、父进程 rc 保持不变（因为父进程的 PgFrame 已经在追踪原有的引用）。当前的设计是每个 `AddrSpace` 持有独立的 `PgFrame` 实例，那么正确的引用计数应为：父 rc 不变（仍然代表父侧有多少人共享），子 rc=1（子侧独享）。`frame.up()` 是多余的——它增加的是父进程自己的 `PgFrame.rc`，但不影响子进程的独立副本。
  3. **后果**: `handle_cow_fault` 中 `rc <= 1` 的判断会因 rc 偏高而永远不成立，导致即使页面已经是独占的，仍然会分配新物理页做不必要的复制，浪费内存和 CPU。
- **影响**: 中。COW 页面的引用计数不准确，导致不必要的页面复制（性能浪费）或页面永远不会被正确回收（内存泄漏）。

---

## 57. AddrSpace::handle_cow_fault — 旧物理页数据未复制到新页

- **位置**: `chaos-tests/src/lib.rs:5908-5928`（`AddrSpace::handle_cow_fault`）
- **代码**:

```rust
pub fn handle_cow_fault(&self, addr: usize, pool: &FramePool) -> Result<usize, &'static str> {
    let page_addr = addr & !(PAGE_SZ - 1);
    let region = self.vm_map.find(addr).ok_or("segfault")?;
    if region.flags & VM_WRITE == 0 { return Err("segfault"); }
    let mut cow = self.cow_pages.lock().unwrap();
    if let Some(frame) = cow.get(&page_addr) {
        let rc = frame.count();
        if rc <= 1 {
            return Ok(page_addr);                        // 独占，直接恢复写权限
        }
        let new_frame_id = pool.get_inner().ok_or("oom")?;
        frame.down();                                    // 旧页 rc-1
        let new_frame = PgFrame::with_rc(1);
        cow.insert(page_addr, new_frame);
        Ok(new_frame_id * PAGE_SZ + MEM_OFF)             // 返回新页地址
    } else {
        let frame_id = pool.get_inner().ok_or("oom")?;
        cow.insert(page_addr, PgFrame::with_rc(1));
        Ok(frame_id * PAGE_SZ + MEM_OFF)
    }
}
```

- **问题**:
  1. **未执行数据复制**: COW（Copy-on-Write）的核心是"写时**复制**"——分配新物理页后，必须将旧页的 4096 字节数据完整复制到新页，然后更新页表让虚拟地址指向新页。当前只分配了新页、更新了引用计数，但没有任何 `memcpy` / `copy_from_slice` 操作将旧页数据复制到新页。写入者拿到的是一个**内容未初始化**的空页。
  2. **未更新页表**: 分配新页后没有修改页表 PTE（将虚拟地址映射从旧物理页改为新物理页），也没有刷新 TLB。在真实硬件上，即使复制了数据，CPU 仍会通过旧的 PTE 访问旧物理页。
  3. **旧页未标记为只读（对其他共享者）**: 在真实 COW 实现中，如果旧页还有其他共享者（rc > 1 after down），旧页的 PTE 应保持只读，以便其他共享者写入时也能触发 COW fault。当前实现没有管理 PTE 权限。
- **影响**: 高。COW fault 处理后，写入者拿到的是未初始化的垃圾数据而非旧页内容的副本。这是 COW 机制的核心功能缺失，fork 后子进程的任何写操作都会丢失原始数据。

---

## 58. AddrSpace::split_region — 原区域未缩短 + 新区域破坏有序性

- **位置**: `chaos-tests/src/lib.rs:5971-5978`（`AddrSpace::split_region`）
- **代码**:

```rust
pub fn split_region(&mut self, addr: usize) -> Result<(), &'static str> {
    let region = self.vm_map.find(addr).ok_or("enomem")?;
    let offset = addr - region.base;
    if offset == 0 || offset >= region.len { return Err("einval"); }
    let second = VmRegion::new(addr, region.len - offset, region.flags);
    self.vm_map.regions.push(second);   // ← push 到末尾
    Ok(())
}
```

- **问题**:
  1. **原区域 `len` 未缩短（最严重）**: 拆分后原区域应变为 `[base, base+offset)`（即 `region.len = offset`），但代码没有修改 `region.len`。结果是原区域仍然覆盖 `[base, base+原len)` 的完整范围，新区域 `[addr, addr+原len-offset)` 与原区域**完全重叠**。后续的 `vm_map.find()` / `vm_map.insert()` / `vm_map.remove_range()` 都会因为区域重叠产生错误行为。
  2. **`push` 破坏有序性**: `VmMap::regions` 是按 `base` 地址排序的 `Vec`（`insert` 方法使用二分查找确定插入位置），但 `push` 直接将新区域追加到末尾，打破了排序不变式。后续 `VmMap::find` 使用二分搜索，对未排序的列表可能返回错误结果或找不到已有区域。
  3. **缺少 offset/tag 继承**: 新区域使用 `VmRegion::new()`（`offset=0, tag=0`），未继承原区域的 `offset` 和 `tag`。正确做法应类似 `VmRegion::split_at`（第 662 行）那样计算 `ro = self.offset + ll`，或直接使用已有的 `split_at` 方法。
  4. **与 `VmRegion::split_at` 的重复**: `VmRegion` 已有正确实现的 `split_at` 方法（第 662-674 行），返回正确缩短的左半段和继承了 offset/tag/flags 的右半段。`split_region` 没有使用它，而是用了有 bug 的手写逻辑。
- **修复方案**:

```rust
pub fn split_region(&mut self, addr: usize) -> Result<(), &'static str> {
    let idx = self.vm_map.regions.iter().position(|r| r.contains(addr)).ok_or("enomem")?;
    let (left, right) = self.vm_map.regions[idx].split_at(addr).ok_or("einval")?;
    self.vm_map.regions[idx] = left;
    self.vm_map.regions.insert(idx + 1, right);
    Ok(())
}
```

- **影响**: 高。区域重叠 + 有序性破坏会导致虚拟内存管理的多个操作产生错误结果：`find` 找不到或找错区域、`insert` 的重叠检测失效（允许非法映射）、`mprotect` / `munmap` 操作错误的范围。任何依赖 `split_region` 的路径（如 `mprotect` 对区域部分修改权限）都会产生内存管理混乱。

---

## 59. AddrSpace::protect — 区域部分重叠时整个区域权限被覆盖，未做拆分

- **位置**: `chaos-tests/src/lib.rs:5946-5960`（`AddrSpace::protect`）
- **代码**:

```rust
pub fn protect(&mut self, start: usize, len: usize, new_flags: u32) -> Result<(), &'static str> {
    let end = start + len;
    let mut affected = Vec::new();
    for (i, r) in self.vm_map.regions.iter().enumerate() {
        if r.base < end && r.end() > start {
            affected.push(i);
        }
    }
    for &idx in affected.iter().rev() {
        if idx < self.vm_map.regions.len() {
            self.vm_map.regions[idx].flags = new_flags;   // ← 整个区域的 flags 被覆盖
        }
    }
    Ok(())
}
```

- **问题**:
  1. **部分重叠时覆盖范围过大**: 如果 `[start, end)` 只与某个 VMA 部分重叠（例如 VMA 是 `[0x1000, 0x5000)`，而 `protect` 的范围是 `[0x2000, 0x3000)`），代码会将**整个** VMA 的 flags 改为 `new_flags`，影响了不应被修改的范围 `[0x1000, 0x2000)` 和 `[0x3000, 0x5000)`。
  2. **正确做法**: 真实内核的 `mprotect` 在遇到部分重叠时需要先拆分 VMA：
     - 将 `[0x1000, 0x5000)` 拆为 `[0x1000, 0x2000)` + `[0x2000, 0x3000)` + `[0x3000, 0x5000)`
     - 只修改中间那段的 flags
     - 可以使用已有的 `split_region` 或 `VmRegion::split_at` 完成（但 `split_region` 本身也有 bug，见 #58）
  3. **COW 页面权限未同步**: 修改 VMA flags 后，已有的 COW 页面（`cow_pages` 中的条目）的权限状态没有更新。如果从 `VM_WRITE` 改为只读，已有的 COW 页面可能仍然允许写入（因为 `handle_cow_fault` 检查的是 VMA flags，但 PTE 没有更新）。如果从只读改为 `VM_WRITE`，需要为新的可写区域准备 COW 机制。
- **影响**: 中。`mprotect` 修改范围不精确，会意外改变相邻内存区域的权限，可能导致本应只读的区域变为可写（安全漏洞）或本应可写的区域变为只读（程序崩溃）。

---

## 60. AddrSpace::unmap_range — COW 页面 down() 后未检查是否可以释放物理帧

- **位置**: `chaos-tests/src/lib.rs:5930-5944`（`AddrSpace::unmap_range`）
- **代码**:

```rust
pub fn unmap_range(&mut self, start: usize, len: usize) -> usize {
    let end = start + len;
    let removed = self.vm_map.remove_range(start, len);
    let mut cow = self.cow_pages.lock().unwrap();
    let pages_to_remove: Vec<usize> = cow.keys()
        .filter(|&&addr| addr >= start && addr < end)
        .copied()
        .collect();
    for addr in &pages_to_remove {
        if let Some(frame) = cow.remove(addr) {
            frame.down();                                 // rc-1，但不检查是否归零
        }
    }
    removed + pages_to_remove.len()
}
```

- **问题**:
  1. **`down()` 后未检查 rc 是否归零**: `frame.down()` 将引用计数减 1 并返回旧值（`fetch_sub` 返回减之前的值），但代码不检查返回值。如果 `down()` 后 rc 变为 0（即本进程是最后一个引用者），应该调用 `pool.put(frame_id)` 释放对应的物理帧。当前实现中物理帧永远不会被释放回 `FramePool`，导致内存泄漏。
  2. **`PgFrame` 从 `cow_pages` 中 remove 后即 drop**: `frame.down()` 之后 `frame`（`PgFrame`）在循环结束时被 drop，`AtomicUsize` 的值也随之消失。即使其他进程的 `cow_pages` 中仍有对应同一虚拟地址的 `PgFrame`（fork 时复制的独立实例），也无法感知到本进程侧的引用已释放——因为父子进程的 `PgFrame` 是独立的实例，不共享 `AtomicUsize`（见 bug #56）。
  3. **与 `handle_cow_fault` 的关联**: `handle_cow_fault` 中 `frame.down()` 之后同样没有释放旧物理帧（第 5919 行），两处都存在同样的资源泄漏问题。
- **影响**: 中。每次 `munmap` 或进程退出释放地址空间时，COW 页面的物理帧不会被回收，持续运行中物理内存会逐渐耗尽。
