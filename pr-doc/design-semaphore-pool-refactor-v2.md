# 设计:v2 连接池等待机制替换(基线 main)

状态:已实现——S1–S5 落地(实现分支 `feat/upstream-pool-capacity-signal`,基线 `main` @ `ad2046d3`)
基线:`main` @ `ad2046d3`(v1.7.1)
范围:`src/infra/network/upstream/pool/{mod,reuse,pipeline}.rs`、`benches/pool_capacity_churn.rs`
取代:`pr-doc/design-semaphore-pool-refactor.md`(v1;同分支内的前作,未合入 main)

## 0. 背景:为什么在 main 之上重做

v1 已在 `fix/code-review-priorities` 上实现(S1–S4),但复核发现三处行为回归,以及文档与实现不一致。根因不是"Semaphore 选错了",而是**v1 让许可承担了它无法承担的语义**。

v2 保留 v1 的目标(等待机制事件化、删除手写 notify 编排、取消安全由构造保证),但**回到 main 的容量模型**,只替换"等待"这一层。

### 0.1 v1 的三处回归(实测)

| # | 现象 | 实测 | 根因 |
|---|---|---|---|
| R1 | PipelinePool 不再按需扩容 | `max_size=4, max_load=1`:改造前第二个并发 acquire 立即成功、槽位数=2;v1 超时失败、槽位数=1 | `acquire()` 先取池级许可再扫槽,许可耗尽时直接 park,**永不进入建槽路径**;`max_conns` 事实上失效 |
| R2 | Pipeline 容量门泄漏幻影许可 | 1 槽 × `max_load=2` 的池,换过一次槽后闸门仍发出 **4** 枚许可(应为 2) | `expand_one` 每次建槽 `add_permits(max_load)`,槽位退役/剪枝/维护移除时从不回收 ⇒ 许可总量单调增长 ⇒ 闸门失效 ⇒ 退化为 10ms 轮询 |
| R3 | ReusePool 突破 `max_size` | `max_size=2` 时活连接数=3 | 许可语义是"一个连接离开空闲队列",空闲连接不持许可 ⇒ `expand()` 一次性算出的 `want` 与并发建连叠加 ⇒ 失去硬上限 |

> 三处均以临时探针在 `3f14471d`(改造前)与 v1 头上分别复现;v1 的 30 项 pool 测试全绿也未覆盖到它们。

### 0.2 根因:许可不可撤销

`tokio::sync::Semaphore` 的计数只能增加(`add_permits`),不能撤销。v1 的许可同时表达了两件事:

- (a)「可以占用一个并发名额」——额度,会随槽位退役而**需要撤销**;
- (b)「等待者应当被唤醒」——信号。

(a) 需要撤销 ⇒ 只能"先取走许可再 `forget`",而高负载下未必取得到 ⇒ 计数与真实容量必然漂移 ⇒ R2;漂移后闸门不再门控 ⇒ 退回 10ms 轮询,即本次改造本要消灭的旧形态。

**v2 的第一原则:许可只表达"可归还"的量(持有许可的资源被归还/销毁时许可随之归还),绝不表达"需要撤销"的量。**

### 0.3 v1 与 v2 的关系

- v2 **不**沿用 v1 的 `PoolCapacity`。v1 分支若要合入,需按 v2 重写(或整体放弃);两个分支不叠加。
- v2 **保留 main 的全部容量语义与扩容行为**(逐条见 §2.2),只替换"等待"这一层,属最小侵入式改造。

## 1. 基线现状(main @ ad2046d3)

### 1.1 两池的等待方案(main 上的实际位置)

**PipelinePool**(`pool/pipeline.rs`)

- 状态:`release_notified: Notify`(L56)、`reserved_slots: AtomicUsize`(L38)。
- 等待:`acquire()`(L225)先 `try_acquire_existing()`(L292)扫槽;无房间则 `try_reserve_slot()`(L395)预留并 `expand_one()`(L334)建槽;池满则 `release_notified.notified()` + `enable()` + 复查 + park(L256–L290)。
- 唤醒:`PipelineSlot::release()`(L515)**只调 `notify_one()`**;`release_without_notify()`(L529)不通知;`maintain()` 修剪路径 L142/L160、`expand_one()` L357、`usable_or_inflight_slot_count()` 剪枝 L436、`SlotReservation::drop` L634 调 `notify_waiters()`。
- 兜底:容量路径失败时 `wait_backoff()`(L447,10ms)。

**ReusePool**(`pool/reuse.rs`)

- 状态:`active_count: AtomicUsize`(L45,活连接数:空闲 + 借出 + 建连中)、`release_notified: Notify`(L61)。
- 等待:`get()`(L244)先弹空闲队列;无则 `try_reserve_active()`(L382,CAS 至 `max_size`)建连;池满则 `notified()` + `enable()` + 复查 + park(L274–L306)。
- 唤醒:`release()`(L308)**只调 `notify_one()`**;`close_active_connection()`(L318)调 `notify_waiters()`;`expand()`(L325)建成空闲连接后**只调 `notify_one()`**(L353);`maintain()`(L107–L179)在 L127 把仍有效的连接重新入队、L149 把淘汰连接按 `min_size` 补回队列,**两处都不通知**。

### 1.2 main 上仍然存在的三个缺陷

- **D1 许可塌缩**:`PipelineSlot::release()` 与 `ReusePool::release()` 只调 `notify_one()`。`Notify` 在无等待者时只存一枚许可,连续释放会把多枚许可塌缩成一枚 ⇒ 已 park 的等待者睡到 deadline ⇒ 峰值负载下的**假超时**。
  (注:分支 `fix/code-review-priorities` 上的 `3f14471d` 用 `notify_one + notify_waiters` 配对修掉了它,但**该提交不在 main 上**;v2 不依赖该补丁。)
- **D2 maintain 回队列不唤醒**:`ReusePool::maintain()`(L107–L179)在 L127 / L149 把连接重新入队后不通知等待者 ⇒ 等待者继续睡到 deadline。
- **D3 唤醒纪律靠人工**:"每次释放都必须成对通知"是隐式约定,新增分支极易漏配,历史已反复出现补丁式修复。

### 1.3 出厂默认(决定问题严重度)

`ConnectionInfo::DEFAULT_MIN_CONNS_SIZE = 0`、`DEFAULT_MAX_CONNS_SIZE = 64`、`DEFAULT_MAX_CONNS_LOAD = 64`(`config.rs` L311–L313)。默认 `min_conns=0`:PipelinePool 冷启动建 1 个槽位,随后按负载扩到 `max_size`;ReusePool 同理。

### 1.4 main 的规模基线

`pool/` 三文件共 2059 行(`mod.rs` 209 / `reuse.rs` 878 / `pipeline.rs` 972),测试 28 项(`mod.rs` 2 / `reuse.rs` 13 / `pipeline.rs` 13)。

## 2. 目标与非目标

### 2.1 目标

- **G1** 等待机制事件化:一次"资源可用"事件恰好唤醒一个等待者,**唤醒数恒等于事件数**,塌缩按构造不可能;唤醒点从散落的 `notify_one`/`notify_waiters` 混合调用收敛为每池 4 个可枚举的 `signal()` 调用。
- **G2** 容量语义零漂移:"活连接数 ≤ `max_size`"、"单连接在途 ≤ `max_load`"、"池级在途 ≤ 接纳槽位数 × `max_load`"三条不变式由代码结构保证,不依赖人工记账。
- **G3** 行为兼容:main 的扩容时机、`min_size` 语义、超时策略、连接退休语义逐条保持;外部 API(`ConnectionPool::query/maintain`、`ConnectionBuilder`)不变。
- **G4** 可测:每条不变式都有对应断言;旧缺陷 D1/D2 有覆盖。
- **G5** 性能不回退:池级基准对比 main 基线,吞吐与 P99 均持平(实测无差异,见 §5.3)。

### 2.2 行为兼容清单(逐条对照 main)

| 行为 | main | v2 要求 |
|---|---|---|
| PipelinePool 按需扩容到 `max_size` | 有 | **必须保留**(针对 v1 的 R1) |
| PipelinePool 槽位数 ≤ `max_size` | 有(`reserved_slots` + `insert_slot`) | 保留 |
| PipelinePool 单连接在途 ≤ `max_load` | 有(`slot.inflight` CAS) | 保留 |
| ReusePool 活连接数 ≤ `max_size` | 有(`active_count` CAS) | **必须保留**(针对 v1 的 R3) |
| ReusePool 一个连接同一时刻只借一个 | 有 | 保留 |
| `min_size` 预热与补齐 | 有 | 保留 |
| 超时策略(Reuse / Retire / Close) | 有 | 保留 |
| 槽位退役与 `is_drained_unusable` 修剪 | 有 | 保留 |
| 10ms 退避的用途 | 建连失败重试(容量路径是 park,不轮询) | 与 main 一致(v1 才把轮询引入容量路径) |
| 取消时的预约归还 | `ActiveReservation` / `SlotReservation` 的 Drop 已保证 | 同 main |

### 2.3 非目标

- 不改连接创建/校验/退役策略,不合并两池实现(pipeline 的按槽负载语义与 reuse 的整连接语义保留)。
- 不引入新配置项,不改变 `min_conns` / `max_conns` 语义。
- 不动 `pool/` 以外的模块(除基准登记与文档同步)。

### 2.4 相对 main 的净收益(诚实核算)

| 维度 | main | v2 | 净收益 |
|---|---|---|---|
| 唤醒塌缩(D1) | `release()` 只 `notify_one`,`Notify` 只存一枚许可 | 一事件一枚令牌,恰好唤醒一个等待者 | **唯一实质收益**:塌缩按构造不可复现 |
| maintain 回队列(D2) | 回队列不通知 | 回队列发事件 | 结构性不可达,纯防御 |
| 唤醒纪律 | 散落的 `notify_one`/`notify_waiters` | 每池 4 个可枚举 `signal()` 点 | 纪律仍在,只是收敛可评审 |
| 性能 | 基准 | 交替跑持平(§5.3) | 0 |
| 扩容 / `max_size` 上限 | 有 | 原样保留 | 0(这是 v1 弄丢的) |
| 取消安全 | Drop 已退还预约 | 同样 | 0(main 本来就没问题) |
| 10ms 轮询 | 容量路径 park,不轮询 | 同样 | 0(v1 反而引入了轮询) |
| 公开 API | — | 零新增 | 0 |

**必须说清的三点,否则就是夸大**

1. **v2 没有消除"必须记得通知"的纪律。** 生产者仍要在每池 4 个事件点调 `signal()`;v2 只是让它们同形、可枚举、可评审。
2. **v2 不比 main 更"取消安全"。** main 的 `ActiveReservation` / `SlotReservation` 的 Drop 本就退还预约,v1 文档里"取消安全要靠人工审查"的前提对 main 不成立。
3. **v2 不是"去掉了 10ms 轮询"。** main 的容量路径本来是 park(v1 才把轮询引入容量路径)。

**D1 的触发条件(main 今天仍带)**

main 的 `PipelineSlot::release()`(pipeline.rs:515)与 `ReusePool::release()`(reuse.rs:308)只调 `notify_one()`。`Notify` 在无等待者时**只存一枚许可**,故连续释放会塌缩:

1. 池满、N 个等待者已 park;
2. 一批释放发生 → `notify_one` 逐个唤醒 → **唤醒者尚未重新 park 时,等待队列为空**;
3. 此时再释放 → 落进"存一枚许可" → 多次事件塌成 1 枚;
4. 后来者注册时领走这枚过期许可 → 复查发现无资源 → 重新 park → **那批塌缩掉的唤醒永久丢失**;
5. 结果:明明有空闲容量,等待者睡到 deadline ⇒ 峰值负载下的假超时。

`3f14471d` 的提交说明即描述该现象,而那 22 行的应急补丁(`notify_one + notify_waiters` 配对 + maintain 回队列通知)**至今不在 main 上**。

**决策口径**

- 若 D1 在生产上**没有**实际咬人,最省的做法是把 `3f14471d` 单独合进 main(修掉 D1 + D2,风险接近零),重构可缓。
- 若要做机制重构,v2 是 main 基线上风险最低的形态;v1 那条路已被三个回归证伪,应直接放弃。
- v2 的正当理由只有一条:**把一个会睡到 deadline 的假超时,从纪律问题变成构造不可能**。不应被描述为性能优化或架构升级。

## 3. 设计

### 3.1 三条不变式(设计的全部支点)

- **I1(资源上限)**:活连接数 ≤ `max_size`。由 CAS 预约计数器表达(`active_count` / `reserved_slots`),预约与归还一一对应。
- **I2(并发上限)**:单连接在途 ≤ `max_load`;池级在途 ≤ 接纳槽位数 × `max_load`。由 per-slot CAS 与槽位数上限**结构性**保证。
- **I3(无漂移)**:任何"可归还"的量用许可表达;任何"需撤销"的量用计数器表达。**不存在需要撤销的许可。**

### 3.2 公共层:`AvailabilitySignal`(`pool/mod.rs` 新增)

一个**计数信号**,不是容量闸:

```rust
pub(crate) struct AvailabilitySignal {
    /// 事件令牌:一次"资源可用"事件 = 一枚令牌,恰好唤醒一个等待者(FIFO)。
    tokens: Semaphore,
    /// 已登记但尚未被唤醒的等待者数量;用于避免令牌堆积(无等待者时不发令牌)。
    waiters: AtomicUsize,
}

impl AvailabilitySignal {
    pub(crate) fn new() -> Self;

    /// 生产者:资源变为可用时调用。有等待者才发令牌,避免无界堆积。
    pub(crate) fn signal(&self);

    /// 消费者:等待"资源可用"并当场取走资源。
    ///
    /// `try_again` 必须**尝试真正取走资源**并返回它(如占用一个槽位名额、
    /// 弹出一条空闲连接),而不是返回布尔谓词 —— 否则复查会取走资源却把它丢掉。
    /// 返回 `Ok(None)` 表示:事件已消费但复查仍未取到资源,调用方应重新评估。
    pub(crate) async fn wait<T>(
        &self,
        deadline: QueryDeadline,
        try_again: impl Fn() -> Option<T>,
    ) -> Result<Option<T>>;

    #[cfg(test)]
    pub(crate) fn pending_tokens(&self) -> usize; // 断言无堆积
}
```

**协议(写进代码注释,评审逐条核对)**

1. `signal()`:`if waiters.load(Acquire) > 0 { tokens.add_permits(1) }`。
2. `wait()`:
   ```
   waiters.fetch_add(1, AcqRel);
   // 复查:覆盖"事件先于登记"的窗口;复查即取资源,取到即返回
   if let Some(value) = try_again() {
       waiters.fetch_sub(1, AcqRel);
       return Ok(Some(value));
   }
   let token = match deadline.run(tokens.acquire_owned()).await {
       DeadlineOutcome::Completed(t) => t,
       DeadlineOutcome::Expired => { waiters.fetch_sub(1, AcqRel); return Err(deadline.timeout_error()); }
   };
   waiters.fetch_sub(1, AcqRel);
   token.forget();   // 消费令牌
   Ok(None)         // 事件已到但资源已被别人取走 ⇒ 调用方重新评估
   ```
3. `token.forget()` 是关键:令牌代表"一次事件",事件既已被消费(哪怕复查后仍不满足),必须**永久消费**。若归还(`drop`),令牌会堆积 ⇒ 后续等待者立即拿到过期令牌 ⇒ 忙等。
4. 虚假唤醒是安全的:唤醒后必须复查真实条件,不满足则继续循环(返回 `Ok(None)` 让调用方重试)。
5. `try_again` 必须"取走资源"而不是"判断资源是否存在":复查若只判断不取,会漏掉这次事件对应的资源,造成一轮无谓等待;若取了又丢掉,则会泄漏槽位名额。
6. 令牌守恒:每次 `signal()` 最多 +1,每次 `wait()` 恰好 -1 ⇒ 可用令牌数不会无界增长(测试断言 `pending_tokens() <= 1`)。

**为什么不用 `Notify`**:main 的 `Notify` 需要"成对 notify"纪律(D1/D3 的根源),且 `notify_waiters()` 是广播(惊群)。计数信号一次事件唤醒一个等待者(FIFO),唤醒点数固定 ⇒ 既消除纪律依赖,又得到可预测的排队顺序。

**为什么不用 `Semaphore` 直接当容量闸**:见 §0.2——容量随资源生命周期增减,而许可不可撤销。

### 3.3 PipelinePool(main 之上最小侵入)

**保留不动**:`reserved_slots` + `insert_slot`(I1)、`slot.inflight` CAS(I2)、`retire` / `close` / `close_if_idle` / `is_drained_unusable`、`SlotReservation` 的 Drop 退款、`expand` / `expand_one` / `try_reserve_slot` 的扩容路径。

**替换**:`release_notified: Notify` → `room_signal: AvailabilitySignal`。

```
async fn acquire(&self, deadline: QueryDeadline) -> Result<PipelineLease<'_, C>> {
    loop {
        // (1) 快路径:扫槽 + per-slot CAS
        if let Some(slot) = self.try_acquire_existing() {
            return Ok(PipelineLease::new(slot, &self.room_signal));
        }
        // (2) 没有有房间的槽位:连接数未达上限则扩容(main 行为)
        if let Some(reservation) = self.try_reserve_slot() {
            match self.expand_one(reservation, deadline).await {
                Ok(Some(slot)) => {
                    if slot.try_acquire(self.max_load) {
                        return Ok(PipelineLease::new(slot, &self.room_signal));
                    }
                }
                Ok(None) => {}
                Err(e) => {
                    if deadline.remaining().is_none() {
                        return Err(deadline.timeout_error());
                    }
                    debug!("Failed to create pipeline-pool connection: {:?}", e);
                }
            }
            self.wait_backoff(deadline).await?; // 仅建连失败路径保留退避
            continue;
        }
        // (3) 池已达槽位上限:等"某个槽位释放名额"事件,醒来后必须重新扫槽
        match self.room_signal.wait(deadline, || self.try_acquire_existing()).await? {
            Some(slot) => return Ok(PipelineLease::new(slot, &self.room_signal)),
            None => continue, // 事件已到但名额被抢走 ⇒ 回到 (1)/(2) 重新评估
        }
    }
}
```

- `wait_for_room` 即上例的 (3) 分支:`room_signal.wait(deadline, || self.try_acquire_existing())` —— 复查闭包**直接占用槽位名额**,取到即用,不会漏掉或泄漏名额。**关键**:被唤醒 ≠ 一定有房间(名额可能已被别人抢走),返回 `None` 时必须重新走 (1)/(2) 分支。
- **退役槽位窗口**:池已达槽位上限但存在退役未排空的槽位时,等待者 park 在信号上;唤醒来源是"该槽位排空(事件 1)"或"该槽位被剪掉(事件 3)",醒来后可能已可扩容 ⇒ 必须重新走 (1)/(2) 分支,不得原地重试。
- **事件点(共 4 处,静态可枚举,全部收敛到 `room_signal.signal()`)**:
  1. `PipelineSlot::release()`——一个在途名额被归还(main 的 `notify_one` 位置);
  2. `expand_one()` 插入新槽——新槽有房间;
  3. `insert_slot()` / `usable_or_inflight_slot_count()` 剪掉失效槽——池拓扑变化,等待者应重新评估(可能此时已可扩容);
  4. `SlotReservation::drop`——预留额度归还,槽位数可能下降。
  > 纪律由"成对 notify"变为"4 个静态可枚举的事件点",其中 3 个是低频的拓扑变化;这是可评审、可 lint 的差异。
- 容量路径与 main 一致:park 等待,不轮询;`wait_backoff` 仅保留给建连失败重试(v1 把轮询引入了容量路径)。

### 3.4 ReusePool(main 之上最小侵入)

**保留不动**:`active_count` CAS 预约(I1)、空闲队列 `ArrayQueue` 与其 `max_size` 容量、`min_size` 语义、`BorrowedConnection` 的 Drop 关闭语义与日志字段、`expand` / `try_reserve_active` / `create_reserved_connection` 的建连路径。

**替换**:`release_notified: Notify` → `conn_signal: AvailabilitySignal`。

```
async fn get(&self, deadline: QueryDeadline) -> Result<BorrowedConnection<'_, C>> {
    loop {
        // (1) 快路径:弹空闲连接(失效连接顺带关闭)
        if let Some(conn) = self.pop_available_connection() {
            return Ok(BorrowedConnection::new(self, conn));
        }
        // (2) 无空闲连接:未达上限则预约并建连
        if let Some(reservation) = self.try_reserve_active() {
            match self.create_reserved_connection(reservation, deadline).await {
                Ok(conn) => return Ok(BorrowedConnection::new(self, conn)),
                Err(e) => {
                    if deadline.remaining().is_none() {
                        return Err(deadline.timeout_error());
                    }
                    debug!("Failed to create reuse-pool connection: {:?}", e);
                }
            }
            self.wait_backoff(deadline).await?; // 仅建连失败路径保留退避
            continue;
        }
        // (3) 池已达 max_size:等"有连接可用"事件
        match self.conn_signal.wait(deadline, || self.pop_available_connection()).await? {
            Some(conn) => return Ok(BorrowedConnection::new(self, conn)),
            None => continue, // 事件已到但连接被抢走 ⇒ 回到 (1)/(2) 重新评估
        }
    }
}
```

- `pop_available_connection()` = 弹空闲队列;弹出的连接若不可用则关闭它(顺带释放 `active_count` ⇒ 事件 2)并返回 `None`。该闭包有副作用(可能关闭一条失效连接),因此**只能由等待路径调用**,不得在别处试探性调用。
- **事件点(共 4 处)**:
  1. `release()`——连接归还空闲队列 ⇒ 变得可借(main 的 `notify_one` 位置);
  2. `close_active_connection()`——连接销毁 ⇒ `active_count` 释放 ⇒ 可以建新连接;
  3. `maintain()` 把弹出的连接重新入队 ⇒ 变得可借(**这正是 main 的 D2 缺陷点**);
  4. `expand()` 建成空闲连接 ⇒ 变得可借。
- 借出语义不变:一个连接同一时刻只被一个查询持有;`BorrowedConnection` 析构关闭连接的既有行为不变。

### 3.5 与 main 的结构差异一览

| main | v2 |
|---|---|
| `release_notified: Notify` | `room_signal` / `conn_signal: AvailabilitySignal` |
| `notified()` + `enable()` + 复查 + park | `signal.wait(deadline)` + 复查(复查仍在调用方) |
| `notify_one()` / `notify_waiters()` | `signal()` |
| 容量路径的 `wait_backoff()` | 删除(仅保留建连失败退避) |
| `active_count` / `reserved_slots` / `slot.inflight` | **完全不动** |

净效果:两池的手写 notify 编排清零,容量语义与扩容行为逐条不变。

## 4. 分阶段实施

| 步骤 | 内容 | 验证 |
|---|---|---|
| S0 | **红测先行**:在 main 上补能复现 D1/D2 的失败测试,以及行为护栏测试(扩容、上限、不变式) | 新测试在 main 上应**失败**(D1/D2)或通过(护栏) |
| S1 | 引入 `AvailabilitySignal` + 单测(事件唤醒、令牌不堆积、虚假唤醒安全、取消安全) | 新增单测 |
| S2 | **ReusePool 迁移**(改动面最小):`release_notified` → `conn_signal`,删 notify 编排 | D2 红测转绿;D1 红测仍红(待 S3) |
| S3 | **PipelinePool 迁移**:`release_notified` → `room_signal`,删 notify 编排与容量路径退避 | D1 红测转绿;全部护栏测试通过 |
| S4 | 清理:删除 `Notify` 依赖;核对 4+4 个事件点齐全;确认容量路径无 `wait_backoff` | `grep -n "Notify" src/infra/network/upstream/pool/` 为 0;评审逐点核对 |
| S5 | 基准:池级 churn 基准接入 `.github/hotpath-benchmarks.json` | bench 报告(main vs v2 对比) |
| S6 | 文档同步:`ai/`、`learning/`、`README`(如涉及)与本设计状态更新 | 文档评审 |

每步独立提交、独立可回退;S2 与 S3 之间无依赖(共用 S1 的原语),可并行评审。

## 5. 测试与基准

### 5.1 回归

- 现有 `cargo test --lib pool`(**main 上 28 项**:`mod.rs` 2 / `reuse.rs` 13 / `pipeline.rs` 13)全部通过;测试体本身不因迁移而改写,若记账断言必须调整,需在 PR 说明中逐条列出。
- 完整门禁:`just check`(fmt + clippy `--all-targets --all-features` + `cargo test`)。

### 5.2 新增测试

**(a) 能复现 main 缺陷的红测(S0 先行)**

1. `pool_reuse_saturation_wakes_all_waiters`:N 个等待者 + 连续释放 ⇒ 全部在 deadline 内被唤醒(main 上因 D1 超时)。
2. `pool_reuse_maintain_requeue_wakes_waiter`:等待者 park 在空池上,`maintain()` 回队列后必须被唤醒(main 上超时 ⇒ D2)。
3. `pool_pipeline_saturation_wakes_all_waiters`:同上,pipeline 侧(D1)。

> **实施修正**:第 1、3 项落为 `test_get_serves_every_waiter_after_a_release_burst` /
> `test_acquire_serves_every_waiter_after_a_release_burst`(等待者链式取用,全部在 deadline 内完成)。
> 第 2 项(D2)在当前代码里**结构上不可达**:`maintain()` 只在空闲队列非空时才回队列,而等待者只在队列为空且池已达 `max_size` 时才 park,两者互斥 ⇒ D2 是防御性缺口而非可观测缺陷。因此没有写"main 上必失败"的红测,改为在 `maintain()` 的回队列与扩容路径保留 `signal()`(成本为一次原子读),并由既有的 maintain 测试覆盖行为。
>
> D1 的"许可塌缩"同样不是确定性可复现:`Notify::notify_one` 只在**等待队列为空**时才存单枚许可,而池满时等待队列通常非空 ⇒ 塌缩需要"唤醒后尚未重新 park"的窄窗口。因此红测采用链式压力形态(统计全部等待者在 deadline 内完成),在 main 上偶发、在 v2 上按构造稳定。

**(b) 行为护栏(防 v1 回归)**

4. `pipeline_scales_out_under_load`:`max_size=4, max_load=1`;第一个租约占满唯一槽位后,第二个并发 acquire 必须在 deadline 内成功,且 `slots.len() == 2`。
5. `pipeline_slot_count_never_exceeds_max_size`:并发 acquire 下 `slots.len() <= max_size`。
6. `reuse_live_connections_never_exceed_max_size`:并发 get + expand 下 `active_count <= max_size`。
7. `pipeline_per_slot_load_never_exceeds_max_load`。
8. `availability_signal_no_token_pileup`:事件多、等待者少时 `pending_tokens() <= 1`。
9. `availability_signal_cancelled_wait_leaks_nothing`:等待被取消(deadline 到期 / abort)后 `waiters` 归零、令牌数不增长。
10. `pipeline_waiter_wakes_when_slot_capacity_freed`:强化 main 的 `test_acquire_waits_until_slot_release`。

### 5.3 基准

- **池级**(新增 `benches/pool_capacity_churn.rs`,并登记进 `.github/hotpath-benchmarks.json`):
  - PipelinePool:固定 `max_size=8, max_load=8`,64 并发查询混合 acquire/release,测吞吐与 P50/P99;
  - ReusePool:固定 `max_size=8`,64 并发借还;
  - 饱和释放场景:测"等待者从 park 到被唤醒"的延迟分布(main 的 D1 在此表现为长尾与超时)。
- **对照基线**:同一 bench 在 `main@ad2046d3` 与 v2 分支上**交替运行**(顺序跑会被机器随时间的漂移污染,单次顺序跑曾读出"快 25%"的假象,main 的 saturated 同一份代码两次跑出 473 vs 395 µs,17% 波动):

  | 对比 | single_borrow | saturated_borrow | 结论 |
  |---|---|---|---|
  | v2 vs main | 40.9 vs 40.7 µs | 337.9 vs 337.8 µs | 无差异(p>0.05) |
  | v2 vs v1 | 55.3 / 59.7 µs(两轮符号相反) | 460 / 488 µs | 无差异(噪声内) |

  ⇒ **G5 结论是"无回退",不是"有提升"**。
- **门禁**:吞吐与 P99 相对 main 均不得回退;等待者唤醒延迟 P99 不得高于 main。
- 沿用仓库既有 hotpath 流程:`just bench-hotpath target="pool_capacity_churn"`,由 CI 出报告。

## 6. 风险与回退

| 风险 | 缓解 |
|---|---|
| 令牌纪律写错 ⇒ 丢唤醒 ⇒ 假超时 | 复查 + `forget()` 双保险;§5.2(a) 红测直接覆盖;`waiters` / `pending_tokens` 可观测 |
| FIFO 唤醒改变尾延迟分布 | §5.3 基准门禁;main 的唤醒近似随机,FIFO 更可预测,极端场景以数据确认 |
| 事件点遗漏(新增分支忘调 `signal()`) | 事件点仅 4+4 个且可静态枚举;评审清单 + §5.2 测试逐类覆盖 |
| `maintain()` 路径改动引入回归 | S2 单独提交;maintain 的连接计数语义不变,只增加一次 `signal()` |
| 大规模行为回归 | 每步独立提交可回退,`git revert` 单步即回到 main 语义 |
| 与 v1 分支冲突 | v2 不含 v1 的 `PoolCapacity`;两分支不叠加,合入顺序由系列文档决定 |

**回退**:任一步 `git revert` 即回到 main 行为。不设配置开关——一次性结构替换不值得长期保留灰度开关。

## 7. 验收标准

- `pool/` 目录下 `Notify` 出现次数为 0,`notify_one` / `notify_waiters` 配对纪律消失。
- main 的 D1、D2 有对应红测,在 v2 上转绿;D3 由"事件点静态可枚举 + 评审清单"约束。
- 行为护栏测试(扩容、上限、每槽负载、令牌不堆积、取消安全)全部通过,防止 v1 的 R1/R2/R3 复发。
- 容量路径与 main 一致(park 等待,不轮询);退避只覆盖建连失败。
- 外部 API 不变;`just check` 全绿;池级基准不回退。
- 文档与实现逐条对应:§3 的每个 API 与每个事件点都能在代码中指出唯一位置。

## 8. 实施结果

**落地范围**(`feat/upstream-pool-capacity-signal`,基线 `main` @ `ad2046d3`)

| 文件 | 变更 |
|---|---|
| `pool/mod.rs` | 新增 `AvailabilitySignal`(计数信号)与 4 个单测;`active_count` / `reserved_slots` / `slot.inflight` 未动 |
| `pool/reuse.rs` | `release_notified: Notify` → `conn_signal: AvailabilitySignal`;`get()` 改为"弹空闲 → 预约建连 → 信号 park"三段;事件点 4 处 |
| `pool/pipeline.rs` | `release_notified: Notify` → `room_signal: AvailabilitySignal`;`acquire()` 保留按需扩容;事件点 4 处;`release_without_notify` → `release_without_signal` |
| `benches/pool_capacity_churn.rs` | 新增池级基准(loopback UDP fixture,公开 `Upstream` API 驱动) |
| `Cargo.toml` / `.github/hotpath-benchmarks.json` | 注册基准 target 与 hotpath 条目(`tier: extended`,`measurement: native`,与 `forward_local` 同类) |

**与设计的两处协议细化**

1. **唤醒后必须复查并取走资源**:`wait()` 在拿到令牌后会再执行一次 `try_again()`,取到即返回 `Ok(Some(resource))`;只有资源已被他人取走才返回 `Ok(None)` 让调用方重新评估。仅在 park 前复查会漏掉事件对应的资源。
2. **令牌以等待者数封顶**:`signal()` 仅在 `waiters > tokens.available_permits()` 时发令牌 ⇒ 可用令牌数不超过等待者数,事件风暴不会堆积令牌,避免过期令牌造成的忙等。`forget()` 仍是硬要求:归还令牌会让令牌堆积。

**实施中修正的第三处(实现自身暴露)**

3. **取消必须归还等待者登记**:第一版只在 deadline 超时路径上 `fetch_sub(waiters)`,任务被 abort 时登记残留 ⇒ 幻影等待者使后续事件空发令牌(令牌无人消费)。改为:正常路径显式归还,取消路径由 RAII 守卫兜底(编译器可能把守卫的 Drop 推迟到调用方作用域结束,所以不能只靠守卫)。新增 `availability_signal_cancelled_wait_leaves_no_waiter`,并验证过:去掉守卫即失败。

**外部接口**:`ConnectionPool::query/maintain`、`ConnectionBuilder`、连接池构造参数均未变;未新增公开类型(基准走既有公开 `Upstream` API),`src/lib.rs` 的导出面不变。

**验证**

- `cargo test --lib upstream::pool::`:38 项通过(main 28 项 + 新增 10 项:信号单测 5、pipeline 护栏 3、reuse 护栏 2)。
- 既有 28 项 pool 测试未改写即通过;外部 API 未变。
- `pool/` 目录下 `Notify` 出现次数为 0;容量路径与 main 一致(park 等待),`wait_backoff` 仅保留在建连失败重试路径。
- 基准(v2 vs main,交替运行):无差异(§5.3)。

## 附录 A:不变式检查表(评审用)

| 不变式 | 表达机制 | 违反时的症状 | 覆盖测试 |
|---|---|---|---|
| 活连接数 ≤ `max_size` | `active_count` CAS 预约 | 超卖,上游连接数超配 | §5.2-6 |
| 槽位数 ≤ `max_size` | `reserved_slots` + `insert_slot` | 同上 | §5.2-5 |
| 单连接在途 ≤ `max_load` | `slot.inflight` CAS | 单连接被打爆 | §5.2-7 |
| 池级在途 ≤ 槽位数 × `max_load` | 结构推导 | —— | §5.2-7 |
| 等待者不丢唤醒 | 计数信号 + 复查 + `forget()` | 假超时 | §5.2-1/2/3/10 |
| 令牌不堆积 | `waiters` 门控发令牌 | 忙等,CPU 飙升 | §5.2-8 |
| 等待取消不泄漏 | `waiters` 增减配平 | 后续事件不再唤醒等待者 | §5.2-9 |

## 附录 B:与 v1 的对照

| 维度 | v1 | v2 |
|---|---|---|
| 基线 | `fix/code-review-priorities` | `main @ ad2046d3` |
| 容量模型 | 重写为"许可 = 并发额度"(总量随槽位增减) | 保留 main 的 CAS 预约计数器 |
| 许可语义 | 可增不可减 ⇒ 幻影许可(R2) | 不出现需撤销的许可 |
| 按需扩容 | 丢失(R1) | 保留 |
| `max_size` 上限 | 丢失(R3) | 保留 |
| 等待原语 | 池级 Semaphore 容量闸 | 计数信号 `AvailabilitySignal` |
| 10ms 轮询 | v1 把它引入 pipeline 容量路径 | 与 main 一致:容量路径 park,不轮询 |
| 基准 | 仅闸门级微基准,未接入 hotpath 套件 | 池级 + 接入 `.github/hotpath-benchmarks.json` |
| 文档状态 | 标"已实现",但与实现不符 | 设计稿,逐条对应代码与测试 |
