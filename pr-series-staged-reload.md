# PR 系列:Staged Reload(零停机热重载)

对应设计:`design-zero-downtime-reload.md`(PR-A/B/C/D 全部落地)
基线:分支 `fix/code-review-priorities`,系列 3 个提交,自低风险纯重构起步,逐步引入行为变更。
建议 PR 标题:`feat(reload): staged zero-downtime configuration reload`

## 提交清单(按边界,依序 cherry-pick)

| # | 提交 | 标题 | 性质 | 依赖 |
|---|---|---|---|---|
| 1 | `72637c47` | refactor(plugin): split runtime build from install | 纯重构,行为不变 | 无 |
| 2 | `36b27388` | feat(reload): staged zero-downtime reload on Unix (design PR-B) | 行为变更(Unix 路径) | 1 |
| 3 | `b7c93320` | feat(control): expose reload phase in /reload/status (design PR-D) | 可观测,独立可用 | 2(phase 语义) |

包含的非本次系列核心提交(混入在分支其它位置,拆 PR 时随第 2 项一并带上):

- `src/infra/observability/metrics.rs` 的分代注册(begin/flush/abort_metric_staging、unregister 摘最旧、渲染按 tag 取最新)——在 36b27388 内;
- `src/infra/network/listen.rs` UDP `SO_REUSEPORT`(36b27388 内);
- `src/app.rs` 重载分派与 phase 标记(36b27388 / b7c93320 内);
- 状态机测试与 serde 格式(b7c93320 内)。

## PR 描述要点

**动机**:重载是 stop-the-world:先全量拆除(API、插件、监听器)再重建,存在数百毫秒到数秒的拒连窗口;候选配置任何失败都会先经历一次真实拆除再回滚。

**方案**:Build → Swap → Drain。

- PR1 把 `PluginRuntimeManager::init_runtime` 拆为 `retire_previous_runtime` / `install_outbound_profiles` / `build_registry` / `install_runtime`,`PluginCreateContext` 增加 `candidate` 标志(暂无消费者)——纯重构。
- PR2 引入 `prepare_assembly` / `activate_assembly` 两相:候选运行时在旧代继续服务时离线构建(server 插件 init 内绑端口,UDP/TCP `SO_REUSEPORT` 双绑共存);指标注册进入旁路 staging(flush 不去重、旧代注销摘最旧同 tag、渲染按 tag 保留最新);Unix 重载走 staged 路径,候选失败零影响,提交后才失败才退回经典回滚;非 Unix 保持原 stop-the-world。
- PR3 给 `/api/reload/status` 增加 `phase`(`validating → building → swapping`,失败保留 `failed`,成功清空),snake_case 序列化。

**影响面**:默认行为在 Unix 上变为零停机;非 Unix 不变。后台任务无需协调(插件自管 task-center 句柄并在 destroy 停止)。

**测试**:新增 staged 指标分代测试(旧代注销不误删候选、失败候选零残留)、reload phase 状态机与 serde 测试;全量 1322 项测试通过;clippy full/minimal/standard 干净。

**风险与回退**:候选 init 期间的出站拨号看到的是上一代 profile(文档化取舍);Windows 保持旧行为;如需紧急回退,重载分派是独立 `#[cfg(unix)]` 分支,可直接摘除。

---

## cherry-pick 操作序列(供执行)

```bash
git checkout -b feat/staged-reload main
git cherry-pick 72637c47
git cherry-pick 36b27388
git cherry-pick b7c93320
git push upstream-or-fork feat/staged-reload
```

注意:

- `36b27388` 的 diff 含 `metrics.rs`(分代注册)与 `listen.rs`(UDP reuseport)的变更,与本系列自洽;文件面为 `app.rs`(+130,重载分派)、`app/bootstrap.rs`(prepare/activate 两相)、`listen.rs`、`observability/metrics.rs`、`plugin/{mod.rs,registry/{mod.rs,runtime.rs}}`——与 semaphore 系列(只动 `infra/network/upstream/pool/*`)**冲突面为空**。
- `72637c47` 同时被 semaphore 系列需要(其 S4 的 cfg 统一针对本提交引入的 `build_registry`),先合入本系列,semaphore 系列基于其后再 cherry-pick 即可。
- phase 可观测完整落在本系列的 `b7c93320`(`control.rs` + `app.rs`),不需要从 `3e3db421` 携带任何东西。
