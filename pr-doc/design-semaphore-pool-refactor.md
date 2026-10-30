# 设计:连接池容量管理 Semaphore 化改造

状态:已实现——S1–S3(PoolCapacity、ReusePool、PipelinePool 迁移)与 S4(churn 基准回归护栏,~40-90M 容量事务/s)全部落地;两池零手写 notify
基线:v1.6.1 + 分支 `fix/code-review-priorities`
范围:`src/infra/network/upstream/pool/{mod,pipeline,reuse}.rs`

## 1. 问题

两类连接池(DoT/DoQ/DoH 的 `PipelinePool`,UDP/TCP 的 `ReusePool`)用同一套手写等待方案管理容量:

```
acquire:循环 {
    try_acquire_existing()          // 无锁扫描槽位/空闲队列,CAS 占用
    失败 → release_notified.notified().await(enable + 停靠)
         → 10ms wait_backoff 兜底轮询
}
release:归还连接/递减 inflight → notify_one() (+ notify_waiters(),3f14471d 补丁)
maintain:巡检/回队列 →(补丁后)notify_waiters()
```

问题:

- **唤醒正确性靠补丁维持**:3f14471d 用 `notify_one + notify_waiters` 配对修掉了许可塌缩与 maintain 漏通知两类假超时;该模式依赖"每次释放都成对调用"的纪律,后续演进容易再引入漏配。
- **等待与预约逻辑重复两份**:pipeline 的 `try_reserve_slot`/`reserved_slots`/`ActiveReservation` Drop 退款与 reuse 的 `active_count`/expand 各自维护一套"容量记账",交错路径多、测试负担重。
- **取消安全要靠人工审查**:等待期间的取消(Drop)必须精确退回预约,任何新分支都是隐患。

## 2. 目标与非目标

**目标**

- G1:容量等待改用 `tokio::sync::Semaphore`——唤醒丢失按构造不可能,删除成对 notify 纪律。
- G2:删除两池各自的手写容量记账中的等待部分,公共语义(acquire 可被 deadline 包裹取消、release 幂等退款)由 `OwnedSemaphorePermit` 天然保证。
- G3:行为兼容——现有 39 个 pool 测试全部通过,外部 API(`ConnectionPool::query/maintain`、`ConnectionBuilder`)不变。
- G4:性能不回退——饱和建连风暴基准对比。

**非目标**

- 不改连接创建/校验/retire 策略,只替换"等待与许可"这一层。
- 不合并两池为单实现(pipeline 的按槽负载语义与 reuse 的整连接语义保留)。

## 3. 设计

### 3.1 公共层(pool/mod.rs)

新增轻量包装,语义集中在一处:

```rust
pub(crate) struct PoolCapacity {
    semaphore: Arc<Semaphore>,
    /// 退役槽位时临时增发的许可数,用于唤醒等待者让其观察到关闭状态。
    retired_permits: AtomicUsize,
    closed: AtomicBool,
}

impl PoolCapacity {
    fn new(total_permits: usize) -> Self;
    /// deadline 可直接包裹返回的 future(内部 first-line 检查 closed)。
    async fn acquire(&self) -> Result<OwnedSemaphorePermit, PoolClosedError>;
    fn add_permits(&self, n: usize);          // 扩容(新槽位 × max_load / min_size 补齐)
    fn begin_retire(&self, permits: usize);   // close: 先置 closed,再 add_permits 唤醒等待者
    fn is_closed(&self) -> bool;
}
```

要点:

- **关闭语义**:`Semaphore` 无法回收许可,采用标准做法——置 `closed` 后 `add_permits(等待者上界)`,被唤醒的等待者首轮检查 `closed` 返回 `PoolClosedError`,由上层按现行 `DeadlineOutcome`/`QueryTimeoutPolicy` 处理。
- **取消安全**:`acquire` 返回 `OwnedSemaphorePermit`,调用方把它放进 lease/连接守卫;Drop 即退款,替代现有 `ActiveReservation`/`using_count` 的一部分职责(注意:per-slot 的 `inflight: AtomicU16` **保留**,见 3.2)。

### 3.2 PipelinePool(DoT/DoQ/DoH)

容量模型:`总许可 = Σ(每槽 max_load)`。

```
query:
  permit = deadline.run(capacity.acquire())?      // 池级等待(替代 release_notified 循环)
  loop {
      slot = try_acquire_existing()               // 保留:轮询索引 + per-slot inflight CAS
      if let Some(slot) = slot { return Lease { slot, permit, .. } }
      // 理论上许可在手必有空槽;若扫描落空(瞬时交错)→ 让出一个时间片重扫,
      // 重扫仍空视为容量记账漂移 → error!(一次性计数指标)并走 create/预约路径
  }
release(lease):
  slot.inflight -= 1; lease 丢弃 → permit Drop 退款     // 无需任何 notify
retire(slot):
  capacity.begin_retire(slot.max_load)            // 唤醒等待者并使其看到 closed/重扫
expand(新槽):
  capacity.add_permits(max_load)
```

- `PipelineSlot` 的 `inflight: AtomicU16`、状态位(`SLOT_ACTIVE/RETIRING/CLOSED`)、`is_drained_unusable` 修剪全部保留——Semaphore 只接管"等什么",不接管"挑哪个槽"。
- 删除:`release_notified`、`release()/release_without_notify()` 的 notify 分支、`wait_backoff`。
- 预约路径(`try_reserve_slot`/`reserved_slots`)简化:创建期间的容量即手中 permit,不再需要独立记账;`SlotReservation` 仅保留防重复建连用途。

### 3.3 ReusePool(UDP/TCP)

容量模型:`许可数 = max_size`,`active_count` 语义并入 permit。

```
get:
  permit = deadline.run(capacity.acquire())?
  conn = idle.pop()
        .unwrap_or_else(|| builder.create(permit 期间).await)   // 建连失败 → permit Drop 自动退款
  return Lease { conn, permit }
release:
  conn 可用且队列未满 → push 回队列,permit Drop 退款
  否则 close_active_connection(conn),permit Drop 退款
maintain:
  巡检/淘汰/min_size 补齐逻辑不变;删除全部 notify 调用
```

- `expand`(min_size 补齐)由后台 maintain 调用 `builder.create` 前先 `try_acquire` 持有 permit,防止与 `max_size` 记账漂移。

## 4. 分阶段实施

| 步骤 | 内容 | 验证 |
|---|---|---|
| S1 | 引入 `PoolCapacity` + 单测(closed 唤醒、add/begin_retire、取消退款) | 新增单测 |
| S2 | **ReusePool 迁移**(较小、语义简单):替换 `release_notified`/`active_count` 等待与记账 | 现有 reuse 测试 + 新增:饱和唤醒(旧 bug 场景)、建连失败退款、maintain 期间 acquire 不滞留 |
| S3 | **PipelinePool 迁移**:池级 Semaphore + 保留 per-slot CAS;删除 notify 与 wait_backoff;简化预约 | 现有 pipeline 竞态测试(899-1006 一带)必须原样通过 |
| S4 | 清理:`pool/mod.rs` 删除公共 notify 辅助;基准测试(见 §5)对比后合入 | bench 报告附 PR |

每步独立提交、独立可回退;S2/S3 之间无依赖,可并行评审。

## 5. 测试与基准

- **回归**:现有 `cargo test --lib pool`(39 项)全绿;v1.6.1 引入的连接释放竞态测试(`pipeline.rs:899-1006`)是硬门槛。
- **新增定向测试**:
  1. 饱和 + 连续释放:旧"许可塌缩"场景,N 等待者全部在 deadline 内被唤醒(旧代码此测试会偶发超时);
  2. acquire 被 deadline 取消后,容量零泄漏(统计可用许可数);
  3. `begin_retire` 后等待者收到 `PoolClosedError` 而非长睡;
  4. 建连失败路径 permit 退款(重试可立即获得容量)。
- **基准**(`cargo bench` 或 criterion 临时用例):固定 `max_size=8`,并发 64 任务混合 get/release 的吞吐与 P99;关注 Semaphore 公平性带来的排队变化(tokio Semaphore 为 FIFO,现行 Notify 方案近似随机唤醒——FIFO 更可预测,但极端场景尾延迟需数据确认)。

## 6. 风险与回退

| 风险 | 缓解 |
|---|---|
| 许可与槽位/连接记账漂移(扫描落空) | §3.2 的漂移检测 + 一次性指标 + 回退到建连路径;S3 期重点评审 |
| `Semaphore` FIFO 唤醒改变尾延迟分布 | 基准门禁:吞吐不低于 95%、P99 不高于 110% |
| 关闭语义遗漏(等待者未观察到 closed) | begin_retire 增发许可数 = 等待者上界;closed 检查在 acquire 首行 |
| 大规模行为回归 | S2/S3 分池迁移、独立回退;保留一轮 `upstream_pool_v2 = true|false` 配置开关(默认 false 灰度)后删除 |

## 7. 验收标准

- 两池零手写 notify;等待逻辑合计代码量净减少;
- 全部现有 + 新增测试通过,基准不回退;
- 旧 bug 场景(许可塌缩假超时)在新结构下**按构造不可复现**(测试 5.2-1 稳定通过)。
