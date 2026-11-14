# PR:fix(cache): ECS scope validation, single-flight, and persistence hardening

> **状态:已被拆分方案取代,勿直接使用。** 缓存系列已拆为四个可独立评审的子 PR(P1–P4),方案见 `pr-doc/pr-split-cache-hardening.md`;其中 P2(请求侧 ECS 剥离)已单独建分支 `fix/ecs-request-strip`,正文见 `pr-doc/pr-body-ecs-request-strip.md`。
> 来源分支 `fix/cache-hardening` 已删除(本地与 fork),提交以标签 `archive/cache-hardening` @ `770f51a0` 归档;其基线 `origin/release/v1.7.0` @ `871f8da3` 已被上游 #422(修复 #403)推进到 `56b7b10f`,直接用该分支建 PR 会重新引入缓存 dump 覆盖缺陷并与上游改动冲突。

**目标分支**:svenshi/oxidns `release/v1.7.0`
**来源分支**:SharerMax/oxidns `fix/cache-hardening`(**已删除**,归档标签 `archive/cache-hardening`;原 11 个提交 + 1 个 release 合并提交)
**创建命令**:

```bash
gh pr create \
  --repo svenshi/oxidns \
  --head SharerMax:fix/cache-hardening \
  --base release/v1.7.0 \
  --title "fix(cache): ECS scope validation, single-flight, and persistence hardening" \
  --body-file pr-doc/pr-body-cache-hardening.md
```

---

## 摘要

缓存执行器的正确性修复(P0)、性能与持久化加固,均由代码评审发现:

1. **ECS scope 插入期校验(P0)**——缓存键在链尚未运行时即已计算,而下游执行器(如 `ecs_handler` 且 `send: true`)或客户端自带 ECS 可能让上游答案带有子网范围,原实现会把子网相关答案存到全局键并服务于所有客户端。现在在插入(含 lazy 刷新)时校验请求当前 ECS scope 与键中 scope 一致,并拒绝带非零 scope 的回显答案;新增 `cache_skip_total{reason="ecs_scope_mismatch"...}` 计数与 3 项测试。
2. **采样 LRU 旋转游标**——原先每轮都从第 1 个分片采样,尾部区域永远免于淘汰;现在游标跨轮推进,完整覆盖整个 map。
3. **dump 序列化移出运行时线程**——`dump_cache_to_bytes` 原先在 tokio worker 上内联克隆并编码整个 map,改为 `spawn_blocking`;staging 文件名加 pid+序号,旧实例的销毁 dump 不再与新实例的周期 dump 交错写入同一 `.tmp`。
4. **lazy 无 stale 窗口语义文档化**——`max(原始 TTL, lazy_cache_ttl)` 规则与"未开 `short_circuit` 不回写"均已写明,纯文档。
5. **冷/过期 miss single-flight**——首个 miss 成为取数 leader,并发 miss 有界等待(`enable()` + 复查防唤醒丢失),leader 成功则所有 waiter 命中结果;新增 `cache_single_flight_wait_total`;2 项测试。
6. **leader 失败后有界重新选举**——waiter 重新注册并参与下一轮选举(共享等待预算,轮次耗尽后自行取数),成功 leader 仍唤醒全部 waiter。新增配置 `single_flight_wait_ms`(默认 5000)与 `single_flight_max_rounds`(默认 3),非零值校验;`cache_single_flight_wait_total` 已补齐指标文档与 WebUI schema/本地化(中/英)。
7. **fmt**——绕行单行长调用,满足 nightly rustfmt(无语义变化)。
8. **ecs_handler 请求侧剥离(随本系列挑入)**——`forward=false` 时 post-stage 现在也移除请求里的 ECS,客户端残留或中途合成的子网不再滞留在上下文(mid-chain 行为不变,上游仍看到 prepared request);与 P0 互补——没有它,跨客户端残留 ECS 会一直污染链后半段的请求上下文,P0 的 scope 拒绝逻辑会被动触发。从源分支单独 cherry-pick,不引入其余历史。
9. **ECS 缓存写入校验文档中/英同步**——补充 insert/lazy 校验与 `ecs_scope_mismatch` 跳过原因的说明,统一中英文参考,纯文档。
10. **缓存跳过原因 WebUI 本地化**——补齐剩余 `cache` 跳过原因的中/英文案与指标键,仅 WebUI i18n,无 Rust 行为变化。

## 合入基线

分支已合入 `origin/release/v1.7.0`(`871f8da3`,合并提交 `4a6d62ef`),因此相对目标分支的 diff 仅为下列 11 个提交;该基线已包含管理 API 系列(#401)、DNS 服务加固系列(#402)及上游 `learn_domain`/hosts 标准格式/query-recorder 改版。合并时 11 个与上游重叠的 docs/WebUI 文件自动合并,逐一核对后两侧内容均保留(`single_flight_*` 与上游 `<PluginAvailability>` 注解并存)。

## 提交

| 提交 | 内容 |
|---|---|
| `c49fdc2b` | fix(cache): validate ECS scope at insert time(P0) |
| `959f558e` | perf(cache): rotate sampled-LRU scan window across the map |
| `93797a12` | perf(cache): dump serialization off the runtime thread, unique staging |
| `2c8c03b0` | docs(cache): spell out lazy stale-window semantics |
| `bc838851` | docs(cache): document ECS scope mismatch skip and single-flight behavior(拆分文档同步提交出的缓存部分) |
| `9ddc9540` | perf(cache): single-flight cold and expired misses |
| `d20fa028` | feat(cache): re-elect single-flight leader and make it configurable |
| `ea831344` | style(cache): wrap long executor chain call to satisfy fmt |
| `44c39246` | fix(ecs_handler): strip request ECS after downstream when not forwarded |
| `20f48f70` | docs(plugin): describe cache ECS write validation and sync English reference(纯文档) |
| `770f51a0` | fix(webui): localize remaining cache skip reasons(WebUI i18n) |

## 测试

- 新增 11 项单测:ECS 3(`test_response_echoes_scoped_ecs`、`ecs_injected_by_downstream_executor_is_not_cached`、`client_supplied_ecs_is_not_cached_under_global_key`),single-flight 2(`concurrent_cold_misses_share_one_downstream_fetch`、`miss_single_flight_follower_falls_back_when_leader_fails`),重新选举与配置校验 4(`miss_single_flight_reelects_one_leader_when_the_first_fails`、`validate_config_rejects_zero_single_flight_wait`、`validate_config_rejects_zero_single_flight_max_rounds`、`single_flight_config_overrides_are_resolved`),ecs_handler 2(随 `44c39246`)。
- 全量 `cargo test --all-features`(分支最终态 `44c39246`,2026-11-05):**1369 passed / 0 failed / 3 ignored**——lib 1257、feature_gating 5、plugin_integration 107;用例执行约 46s,编译 0 warning。3 项 ignored 是 query_recorder 的压测用例(`benchmark_bounded_statistics_filters`、`benchmark_storage_variants`、`benchmark_v2_queue_pressure`),不是失败跳过。
- 聚焦测试:`cargo test --lib --all-features ecs_handler` → **9 passed**(含 `44c39246` 新增 2 项)。
- WebUI:`vitest run lib/plugin-definitions-advanced.test.ts lib/i18n/i18n-coverage.audit.test.ts` 6/6 通过。
- 门禁:`cargo +nightly fmt --all --check` 干净;Windows nightly `cargo +nightly clippy --all-targets --all-features -- -D warnings` 仅剩基线既有的 `src/infra/network/dial.rs:483`,本系列触及文件零告警。

## 影响面

- **行为变化(正确性)**:带下游注入或回显非零 ECS scope 的响应不再写入缓存(`cache_skip_total{reason="ecs_scope_mismatch"}`);带 ECS 答案如需各自缓存,把 `cache` 放在 ECS 执行器之后,或开启 `ecs_in_key`(仅对客户端提供的 ECS 生效)。
- **新配置**:`cache.single_flight_wait_ms`(默认 5000)、`cache.single_flight_max_rounds`(默认 3),显式 0 会被配置校验拒绝;无其它 schema 变化。
- **性能**:冷/过期高频域名共享一次下游取数;LRU 采样覆盖全 map;dump 不再阻塞运行时线程。
- **WebUI**:缓存插件表单、字段文档(中/英)与 i18n 键已补齐 single-flight 两项配置与 `cache_single_flight_wait_total` 指标。

## 已知事项

- `659dcf15` 本身不含纯学习笔记类改动;这些笔记不随本 PR 携带。
- 本系列不携带 `87cad1b2`(Windows nightly 修补),`dial.rs:483` 为基线既有告警,不影响 Linux CI。
- **P0**:`c49fdc2b` 建议优先评审合入。
