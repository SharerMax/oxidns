# PR:feat(api): expose provider degraded state

> 目标分支:`svenshi/oxidns:main`(堆叠于 feat/provider-degraded-startup,合入后自动改基)· 对应 issue #371 · milestone v1.8.0

## 摘要

PR-1 让规则文件缺失时 provider 降级启动,但降级状态只存在于 error 日志里。本 PR 把降级状态暴露到管理层:reload 响应携带降级文件列表、新增 provider status 端点、注册 `plugin_degraded` gauge。WebUI 展示在后续 PR 跟进。

## API 变化

**① `POST /api/plugins/{tag}/reload` 响应扩展**(向后兼容,`ok` 语义不变):

```jsonc
// 完全成功
{ "ok": true, "action": "reload_provider", "provider": "adguard",
  "status": "reloaded", "degraded_files": [] }

// 带降级成功(HTTP 仍 200)
{ "ok": true, "action": "reload_provider", "provider": "geosite-cn",
  "status": "degraded", "degraded_files": ["./geosite.dat"] }
```

语义变化:文件缺失不再落入 `provider_reload_failed`(400),而是 `ok: true` + `status: "degraded"`;只有真正致命的构建错误保持 400。调用方兼容——`ok` 字段含义不变,但 WebUI 若对 `provider_reload_failed` 有专门提示逻辑需同步检查(不在本 PR 范围)。

**② 新增 `GET /api/plugins/{tag}/status`(provider)**,复用 matcher status 的路由模式:

```jsonc
{ "ok": true, "provider": "adguard", "status": "running", "degraded_files": [] }
```

matcher 与 provider 是互斥类别,同一路径不会冲突。

**③ 可观测性**:provider 在 `ProviderRuntimeControl::new` 时注册 `plugin_degraded` gauge(1/0,label `plugin_tag`),`drain()` 时注销——跟随 registry destroy 的既有 teardown 路径,不泄漏。指标定义:provider 当前快照是否携带未加载的规则文件。

## 实现

- `ProviderRuntimeControl::degraded_files()`:读取 provider 快照内的权威降级列表(`Provider::degraded_files`,PR-1 引入)。
- `ProviderDegradedMetric`:`MetricSource` 实现,gauge 随抓取实时计算,无缓存状态。
- 路由注册:provider 类别在 `register_plugin_runtime_control_routes` 中增加 `GET /status`。

## 热路径影响

零。gauge 在抓取时计算,查询路径(`contains_name` 等)完全不变。

## 测试

- `api/runtime_control.rs`:新增 2 个契约测试——reload 响应的 reloaded/degraded 两态(含 `degraded_files` 断言)、provider status 端点的 running/degraded 两态。
- `plugin/provider/mod.rs`:端到端测试补 reload 响应新字段断言 + `GET /api/plugins/{tag}/status` 路由断言。
- 全量 `cargo test --lib` 1302 passed / 0 failed;集成测试并行与串行均全绿(见 `learning/issues.md` ISSUE-004:期间发现并修复了过时失败用例泄漏运行时导致的整批挂起,修复提交 `7e9f7d13` 在 PR-1 分支)。

已运行的命令:

```
cargo check --workspace --all-targets
cargo +nightly fmt --all --check
cargo +nightly clippy --all-targets --all-features -- -D warnings
cargo test --lib
cargo test --test plugin_integration
```

## 文档跟进

- API 参考文档与 WebUI 展示(降级徽标、store 采样通道)在后续 PR 跟进。
