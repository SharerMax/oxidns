# fix(ecs_handler): strip request ECS after downstream when not forwarded

> **状态:已放弃(2026-11-11)。** 分支 `fix/ecs-request-strip` 已删除(本地与 fork),提交以标签 `archive/ecs-request-strip` @ `c6537b87` 归档;本正文保留作历史参考,**不再提交 PR**。
> 放弃原因:上游 PR #426(`afc23736` `fix(ecs_handler): correct ECS source selection and preset validation`)已进入 `origin/main` 与 `release/v1.7.0`:它在 `forward: false` 时于**链前**剥离客户端 ECS(非 IN 类查询同样覆盖),并在插件文档中明确写下「request ECS remains available after execution; this plugin does not restore or clear the query when the downstream chain returns」。本 PR 主张的是**链后**剥离请求 ECS(含内部合成的子网),与上游既定语义相反。
> 若日后要重提,应先窄化为「链后仅清理内部合成 ECS」并与维护者确认意图;原始改动仍可从 `fix/code-review-priorities` 的 `e23a1870` 取回。

## 背景

`ecs_handler` 在 `forward: false`（ECS 不从客户端原样转发）时，回程阶段只剥离**响应**里的 ECS，请求侧的 ECS 仍然留在 `DnsContext` 中：

- 非 IN 类查询：`prepare_request` 命中 `query_class != DNSClass::IN` 直接返回 `false`，客户端自带的 ECS 从未进入准备流程，却仍原样挂在请求上；
- 链内合成的 ECS（`send: true` 或 `preset`）：合成后写入请求，链结束后同样残留。

残留的请求侧 ECS 会被链后的组件观察到（缓存键、查询摘要、指标、日志），出现「上游根本没有收到 ECS，链后却读得到 ECS」的错位；配合 `cache` 的 `ecs_in_key` 时，还会让缓存键带上一个从未发往上游的网段维度。

## 变更

代码（`src/plugin/executor/ecs_handler.rs`）：

- 回程阶段在剥离响应 ECS 的同时，一并剥离请求 ECS（`strip_ecs_from_message(context.request_mut())`）；
- **链中状态不变**：上游仍在链中看到准备好的请求，清理只发生在下游链结束之后；
- 单元测试：新增 `EcsProbe` 探针执行器，断言「链中可见 ECS、链后请求已无 ECS」；新增非 IN 类查询残留 ECS 的用例；原有 `send` 用例从「链后请求仍带 ECS」改为断言链内可见、链后已清理。

文档：

- `website/docs/plugin-reference/executor/resolution.mdx` 与英文版 `website/i18n/en/docusaurus-plugin-content-docs/current/plugin-reference/executor/resolution.mdx` 同步说明回程清理范围（此前只写了响应 ECS）。

## 影响面

- 仅 `ecs_handler` 插件；代码改动集中在单个文件，无配置项、指标或 WebUI 变化；
- 只影响 `forward: false` 且位于该插件下游的链；
- **行为变化**：链结束后 `DnsContext.request` 不再携带 ECS。若有下游组件依赖「链后仍能读到请求 ECS」，应改用 `client_ip_from_ecs`，或在 `ecs_handler` 之前完成读取；
- 与 `forward: true` 的路径无关：客户端 ECS 原样转发的场景不进入回程清理分支，行为保持不变。

## 测试

| 命令 | 结果 |
|------|------|
| `cargo check --all-targets` | 通过 |
| `cargo test --lib ecs_handler` | 9 passed（0 failed），含 2 个新用例 |
| `cargo test` | 1391 passed / 0 failed / 4 ignored（lib 1274、integration 112、其余 5） |
| `cargo +nightly fmt --all --check` | 通过 |
| `cargo +nightly clippy --all-targets --all-features -- -D warnings` | Windows 上命中上游既有 lint：`src/infra/network/dial.rs:483`（`needless_bool`，仅 `cfg(not(unix))` 分支）。该文件未被本 PR 修改，Linux CI 不编译该分支 |

## 关联

- 拆分自原 `fix/cache-hardening` 的 `44c39246`，cherry-pick 后 patch-id 一致（`f7ed03018179b3c2fc8d8a6b34358c82c2ea8420`）；该源分支已删除，提交以标签 `archive/cache-hardening` 归档；
- 本 PR 与缓存侧 ECS scope 校验语义互补，两者可独立评审、独立合入；
- 基线：`origin/release/v1.7.0`（`56b7b10f`）。
