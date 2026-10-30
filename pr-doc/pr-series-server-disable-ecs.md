# PR 系列:Server 禁用 ECS(disable_ecs)

基线:分支 `fix/code-review-priorities`,单个提交 `fc15334e`,可独立成 PR,不依赖其他系列。

建议 PR 标题:`feat(server): add disable_ecs to strip EDNS Client Subnet`

## 提交清单

| # | 提交 | 标题 | 性质 | 依赖 |
|---|---|---|---|---|
| 1 | `fc15334e` | feat(server): add disable_ecs to strip EDNS Client Subnet | 新增四个 server 入口的可选开关 + WebUI/文档同步 | 无 |

`git cherry-pick fc15334e` 即可;若只发代码 PR,可剔除 `docs/` 与 `webui/` 相关文件,仅保留 `src/plugin/server/`。

## 行为定义

- 配置项 `disable_ecs`(必填:否;默认 `false`),`udp_server` / `tcp_server` / `quic_server` / `http_server` 均支持;HTTP 版对所有 `entries` 路径统一生效。
- `false`(默认):行为完全不变。
- `true`:请求进入执行链前移除 EDNS `CLIENT-SUBNET`(ECS)option;响应回写前移除响应中的 ECS option(OPT 记录本身保留)。
- 等价于 RFC 7871 中"服务端忽略 ECS 且不回显"的语义,不涉及拒绝或丢弃查询。

## 实现位置

- `src/plugin/server/request.rs`:剥离逻辑集中在共享的 `RequestHandle`,而不是四个协议各自实现:
  - `strip_request_ecs` 在 `handle_request` 入口、`DnsContext::new` 之前执行,保证下游执行器(含 `ecs_handler`、`client_ip_from_ecs`)与 cache key 都看不到客户端子网;
  - `finalize_response` 由关联函数改为 `&self`,在"按请求补 OPT"之后移除响应 ECS,避免误删 OPT;
  - Refused / BADVERS / 主路径三条出口统一走 `self.finalize_response`。
- `udp.rs` / `tcp.rs` / `quic.rs` / `http/mod.rs`:配置结构新增 `disable_ecs: Option<bool>`,factory 中 `unwrap_or(false)` 后传入 `RequestHandle`;HTTP 在路由循环前取一次值,所有 route 共享。
- 测试辅助构造(`udp.rs` 测试、`tcp/tests.rs` 4 处、`http/http_server.rs`、`http/entry.rs`)补 `disable_ecs: false`。

命名说明:初版为 `drop_ecs`,后统一改为 `disable_ecs`。仓库布尔配置既有习惯是 `enable_*` / `skip_*`(如 `enable_http3`、`enable_pipeline`、`skip_webui`),无 `drop_*` 先例;且 `drop` 在 DNS 语境中通常指丢弃报文,易被误读为"带 ECS 的请求直接丢包"。

## 影响面

- 请求热路径:仅在 `disable_ecs = true` 时各多一次 `edns_mut().remove()`;默认路径只有一次 bool 判断,无分配。
- 下游可见性:`ecs_handler`、`client_ip_from_ecs` 等依赖 ECS 的策略在该入口下失效;需要按客户端子网分流的部署应保持默认 `false`。
- 不改变外部 API、配置文件其余字段和默认配置行为。

## 测试

- 新增 2 项 `RequestHandle` 用例:`disable_ecs = true` 时执行链看不到子网、响应不回显子网且 OPT 保留;默认 `false` 时请求 ECS 原样透传。
- `cargo test --lib -- plugin::server::` 48 passed。
- `cargo fmt -- --check`、`cargo +nightly clippy --all-targets --all-features -- -D warnings` 通过。
- WebUI:`prettier --check`、`plugin-definitions-advanced` 与 i18n 覆盖审计(6 项)、`tsc --noEmit` 通过。

## 同步的可维护表示

- WebUI:[server.ts](file:///d:/code/other/oxidns/webui/lib/plugin-definitions/server.ts)(四个 server 的高级 switch,标签"禁用 ECS"/"Disable ECS")、[docs.ts](file:///d:/code/other/oxidns/webui/lib/plugin-definitions/docs.ts)、zh-CN / en-US 的 `plugin-defined.ts` 与 `docs.ts`、[plugin-definitions-advanced.test.ts](file:///d:/code/other/oxidns/webui/lib/plugin-definitions-advanced.test.ts) 基线。
- 文档:[datagram-stream.mdx](file:///d:/code/other/oxidns/docs/docs/plugin-reference/server/datagram-stream.mdx)(udp/tcp)、[encrypted-http.mdx](file:///d:/code/other/oxidns/docs/docs/plugin-reference/server/encrypted-http.mdx)(http/quic),及 `docs/i18n/en/.../server/` 英文镜像。

## 风险与回退

- 主要风险是语义误用:开启后依赖 ECS 的执行器静默失效。文档已在字段说明中列出 `ecs_handler` / `client_ip_from_ecs` 的影响与"不泄漏上游子网"的收益。
- 回退简单:默认值为 `false`,既有部署无需改动;如需撤销功能,整体 revert `fc15334e` 即可,无跨提交依赖。
