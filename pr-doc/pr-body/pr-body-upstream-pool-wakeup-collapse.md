# PR:fix(upstream): wake parked pool waiters on release and requeue

**目标分支**:svenshi/oxidns `main`
**来源分支**:SharerMax/oxidns `fix/upstream-pool-wakeup-collapse`(1 个提交,基线 `origin/main` @ `ad2046d3`)
**创建命令**:

```bash
gh pr create \
  --repo svenshi/oxidns \
  --head SharerMax:fix/upstream-pool-wakeup-collapse \
  --base main \
  --title "fix(upstream): wake parked pool waiters on release and requeue" \
  --body-file pr-body-upstream-pool-wakeup-collapse.md
```

---

## 摘要

上游两类连接池(ReusePool / PipelinePool)的手写等待机制存在一个真实的丢唤醒缺陷:等待者以 `Notify::notified()` + `enable()` + 复查 + park 的模式等待空闲容量,而释放侧**只调 `notify_one()`**。`Notify` 在没有已注册等待者时最多只存一枚许可,**连续多次 `notify_one` 会塌缩成同一枚许可**。后果:

1. **许可塌缩(D1)**:两个连接背靠背释放时,第二次 `notify_one` 因许可已存而无效。一枚许可只唤醒一个等待者;另一个已 park 的等待者不再被唤醒——尽管此刻池里已有空闲连接/空闲槽位,它仍睡到查询 deadline,表现为**峰值负载下的假超时**。
2. **maintain 回队列不通知(D2)**:`ReusePool::maintain()` 会把连接从空闲队列 pop 出来检查有效性再 push 回去。在这个窗口里队列瞬时为空,等待者复查弹空后 park;之后 maintain 把连接放回队列却**不发任何通知**,等待者继续睡到 deadline。

两类缺陷同根:等待机制是事件驱动的,但释放路径没有把"资源可用"这个事件可靠地传达给所有已注册等待者。

## 变更

| 位置 | 内容 |
|---|---|
| `pool/pipeline.rs` `PipelineSlot::release()` | `notify_one` 之后追加 `notify_waiters`,并重写注释(原注释「notify_one 存许可即可,不会饿死等待者」的论证不成立) |
| `pool/reuse.rs` `ReusePool::release()` | 同上:`notify_one` + `notify_waiters` 成对 |
| `pool/reuse.rs` `ReusePool::expand()` | 建成空闲连接入队后同样成对通知 |
| `pool/reuse.rs` `ReusePool::maintain()` | 以 `requeued` 标志跟踪本轮是否发生过回队列(有效连接 push 回去、或淘汰连接按 `min_size` 补回),有则 `notify_waiters()` |

`notify_one` 与 `notify_waiters` 成对的原因:等待者的注册窗口横跨「创建 `notified()` → `enable()` → 复查 → park」数个 await 点。`notify_one` 覆盖「已 enable 但尚未 park」的等待者(enable 会认领存好的许可),`notify_waiters` 唤醒所有**已注册**(已 enable)的等待者,二者互补后,无论等待者处于哪个阶段都不会漏唤醒。多余唤醒是安全的:等待者醒来后复查队列/槽位,拿不到就重新 park。

## 缺陷机理(精确时序)

以 ReusePool 为例,`max_size=2`,两个查询占满两连接,两个新查询 W1、W2 并发到达:

1. W1、W2 各自弹空队列,走到 `enable()` + 复查(此刻两连接均在借出,队列空、无空闲容量),双双 park——**均已注册**。
2. 查询 A、B 背靠背完成并释放:旧代码两次 `notify_one`。第一次唤醒 W1(已注册);第二次本应唤醒 W2,但若此时 W2 恰在「复查 → park」之间尚未注册,则只存下一枚许可;而该许可随后被 W1 的 `enable()` 认领……W2 成为孤儿,睡到 deadline。

maintain 场景更直接:W2 park 时 maintain 正握着全部弹出的连接,回队列零通知。

## 影响面

- **热路径**:release 路径多一次 `notify_waiters()`。代价是等待者数量 > 事件数时可能出现惊群(全部醒来复查、部分重新 park);等待者数量受池容量与并发查询数约束,复查是纯内存操作,实测全量测试与既有池基准无可见差异。这是**最小修复**的已知代价——长期方向是用事件化等待机制(每事件恰唤醒一个等待者,如 Semaphore 封装)从结构上消除塌缩,可作后续重构。
- **行为变化**:仅消除假超时(等待者在有容量时被正常唤醒),无配置项、无 API 契约、无响应语义变化。
- `release_without_notify()`(try_acquire 竞态回滚)保持不通知:它不减净容量,等待者的复查路径自会处理。

## 测试

- 无新增测试:缺陷依赖「释放与等待者注册之间的精确交错」,单测难以确定性复现;既有 39 项 pool 测试覆盖相邻唤醒路径(close/retire/expand/退预约),全部通过。

| 命令 | 结果 |
|------|------|
| `cargo +nightly fmt --all --check` | 通过 |
| `cargo test --locked --workspace --all-features`(CI 同款) | 1554 passed / 0 failed / 4 ignored(oxidns lib 1301、integration 115、proto 82、zoneparser 35、ripset 16、feature_gating 5) |
| `cargo test --lib pool`(聚焦) | 39 passed / 0 failed |
| `cargo +nightly clippy --all-targets --all-features -- -D warnings` | 见下方说明 |

> clippy 说明:本机为 Windows,唯一告警是 `src/infra/network/dial.rs:483` 的 `needless_bool`——**main 上既有**(`cfg(not(unix))` 分支,该文件未被本 PR 修改),Linux CI 不编译该分支。
