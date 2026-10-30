# PR 系列:Semaphore Pools(连接池容量改造)

对应设计:`design-semaphore-pool-refactor.md`(S1–S4 全部落地)
基线:分支 `fix/code-review-priorities`,系列 6 个提交(含 1 个修正确认提交),从应急正确性修复到结构性替换。
建议 PR 标题:`refactor(upstream): semaphore-backed pool capacity`

## 提交清单(按边界,依序 cherry-pick)

| # | 提交 | 标题 | 性质 | 依赖 |
|---|---|---|---|---|
| 0 | `3f14471d` | fix(upstream): wake parked pool waiters on release and requeue | 应急正确性修复(可独立合入,被后续重构整体取代) | 无 |
| 1 | `72637c47` | refactor(plugin): split runtime build from install | **被本系列需要**:S4 的 `3e3db421` 对其引入的 `build_registry` 做 cfg 统一 | 无 |
| 2 | `ac055074` | refactor(upstream): semaphore capacity for ReusePool (design S1+S2) | 结构替换:ReusePool → PoolCapacity(Semaphore) | 建议先合 0(上下文);#1 已在 |
| 3 | `cd9f5cbc` | refactor(upstream): semaphore load capacity for PipelinePool (design S3) | 结构替换:PipelinePool → 池级许可 + per-slot CAS | 2(PoolCapacity.add_permits) |
| 4 | `3e3db421` | feat(control,bench): … capacity churn bench (design PR-D + S4) — **仅 bench 部分** | 基准护栏 + PoolCapacity 公开 + cfg 统一 | 3 |
| 5 | `a39dcef5` | fix(bench): pass criterion an owned task Handle per invocation | bench 修正 | 4 |

注意:`3e3db421` 同时包含 reload phase(control.rs/app.rs)与 bench 两部分;拆 PR 时对 bench 相关文件(`benches/pool_capacity_churn.rs`、`Cargo.toml` criterion features、`src/infra/network/upstream/{mod,pool/mod}.rs` 的 PoolCapacity 公开)取其子集,phase 部分归属 staged-reload 系列。干净做法:

```bash
git checkout -b feat/semaphore-pools feat/staged-reload   # 或 main + cherry-pick 0
git cherry-pick 3f14471d   # 可选:先出应急修复 PR
git cherry-pick ac055074
git cherry-pick cd9f5cbc
# 3e3db421 混含 bench(本系列)与文档创建(元数据),且 phase 并不在其中
# (phase 全在 b7c93320,属 staged-reload 系列)。cherry-pick 后剔除
# design-semaphore-pool-refactor.md 即可:
git cherry-pick 3e3db421 && git rm design-semaphore-pool-refactor.md
git cherry-pick a39dcef5
```

S4 bench 子集的文件面:

- 新增 `benches/pool_capacity_churn.rs`(含 a39dcef5 修正);
- `Cargo.toml` + `Cargo.lock`:criterion 加 `features = ["async", "async_tokio"]`,新增 `[[bench]] pool_capacity_churn`;
- `src/infra/network/upstream/mod.rs`:`pub use pool::PoolCapacity;`;
- `src/infra/network/upstream/pool/mod.rs`:**PoolCapacity 由 pub(crate) 改 pub**(`total` 字段改 `AtomicUsize` 支持动态 `add_permits` 精确记账)——这一步在 `3e3db421`,不是 ac055074;
- `src/plugin/registry/runtime.rs` 的 `build_registry` cfg 统一(`drop(test_guard)` + 无条件参数;依赖 `72637c47` 引入的方法);
- **剔除**:`3e3db421` 同时创建了 `design-semaphore-pool-refactor.md`(分支元数据,不进上游)并对 `src/plugin/registry/init_plan.rs` 做纯 fmt 尾随(属 `72637c47`,cherry-pick 该提交时自然带上)。

## PR 描述要点

**动机**:两类连接池用同一套手写等待方案(无锁扫描 + Notify enable/park + 10ms 退避),存在两类真实缺陷:连续 `notify_one` 许可塌缩使已 park 的等待者睡到 deadline(maintain 回队列同样漏通知),且"等待与预约"逻辑在两池各写一份,取消退款全靠人工审查。

**方案**:引入 `PoolCapacity`(tokio Semaphore 封装),语义"一枚许可 = 把一份资源留在空闲池之外的权利":

- **ReusePool**(S2):许可 = 一个借出连接。`get` 先取许可再复用/建连;归还空闲队列或关闭即 Drop 许可,唤醒等待者由 Semaphore 构造保证;删除 `active_count` CAS 预约、`release_notified` 全部编排、maintain 重入队通知。建连失败退许可、扩容先入队后退许可(唤醒者必见连接)。
- **PipelinePool**(S3):许可 = 一个槽位在途负载单位。`insert_slot` 贡献 `max_load` 枚许可;lease 持有并在 Drop 时归还。acquire 三段:快路径(非阻塞取许可→扫槽)→ 冷启动自举(无可州槽位先建槽贡献预算)→ 幻影许可(退役/关闭槽位遗留)以建槽替换或退许可+退避消化。per-slot `inflight` CAS 挑槽、retire/close 超时策略、`reserved_slots` 上限、maintain 修剪全部保留——Semaphore 只接管"等待",不动"挑选与生命周期"。
- **S4**:饱和 churn 基准(16 任务 × 2/4/8 许可,~40–90M 容量事务/s)作为吞吐回归护栏——正是旧方案暴露许可塌缩的模式,新构造下不可能复现。

**影响面**:外部 API(`ConnectionPool::query/maintain`、`ConnectionBuilder`)不变;排队从近似随机唤醒变为 Semaphore FIFO(更可预测);两池删除全部手写 notify。

**测试**:现有 41 项 pool 测试(含 v1.6.1 的连接释放竞态硬门槛 pipeline.rs:899-1006)原样通过(仅记账断言改容量口径);新增 PoolCapacity 单测(限额阻塞+drop 唤醒、取消零泄漏);churn 基准通过。

**风险与回退**:许可与槽位/连接记账漂移由 acquire 的自举/重试路径消化;如需紧急回退,`ac055074`/`cd9f5cbc` 为独立提交可整体 revert,`3f14471d` 应急修复会随之恢复(它先于重构存在)。
