# 架构与模块

> 基线:v1.6.1(`release/v1.7.0` 开发线) · 2026-11-05

## 分层架构

| 层 | 路径 | 关键内容 |
|----|------|----------|
| 入口层 | `src/main.rs` · `src/cli/` | `main.rs` — Windows SCM 分发 / CLI;`cli/` — start · check · build-info · probe · service · upgrade · export-dat |
| 应用编排 | `src/app/` | `bootstrap` assemble/stop — 装配与拆除;启动时序 · 优雅关停 · 热重载 · exec 重启;`build_info` — 编译期能力报告 |
| 管理面 | `src/api/` | HTTP(S) 管理 API · Basic 认证 · CORS;SSE 日志流 · Prometheus 指标;WebUI 静态文件服务 |
| 插件层 | `src/plugin/` | server ×4 · matcher ×20 · executor ×35 · provider ×6;`registry` — 工厂目录 · 依赖图 · 初始化计划 · 运行时控制 |
| DNS 核心 | `src/core/` | `DnsContext` — 每请求上下文;`DomainRuleMatcher` — 精确/后缀/关键词/正则;`IpPrefixMatcher` — v4/v6 区间;响应分类(CNAME 链追踪) |
| 基础设施 | `src/infra/` | `network` — 监听 · 传输 · TLS · 上游池 · 引导解析 · 出站策略;`service` — 系统服务;`upgrade` — 自升级;`observability` — 日志/指标;TaskCenter · TtlCache · AppController |
| 配置 | `src/config/` | YAML 模式 · include 合并 · `${VAR}` 展开 · 校验与行列诊断 |
| 配套 crate | `crates/` | `oxidns-proto` — 报文与编解码;`oxidns-macros` — 插件工厂宏;`oxidns-zoneparser`;`oxidns-ripset` |

依赖方向约束:`plugin → infra` 单向依赖,`infra` 不得引用插件 trait 或注册表;`build_info.rs` 位于 crate 根,因需同时引用 infra 常量与插件目录。

## 主要模块

| 模块 | 职责 | 关键入口 |
|------|------|----------|
| `src/main.rs` | 19 行薄入口:Windows 上先尝试 SCM 服务分发,否则走 CLI | `main()` main.rs:12 |
| `src/lib.rs` | 库导出面:api / app / build_info / cli / config / core / infra / plugin;再导出 `plugin_factory` 宏与 `proto` | lib.rs:32-45 |
| `src/cli/` | clap 命令定义与分发;check 输出 file:line:col 诊断,`--graph` 打印插件依赖图;probe upstream 单线程 runtime 探测上游 | `run()` cli/mod.rs:361 |
| `src/app/` | 前台启动编排:`run_until_shutdown` 时序、assemble/stop 装配拆除、热重载、Unix exec 同 PID 重启、启动 banner | `run_until_shutdown` app.rs:64 |
| `src/api/` | 管理 API:精确 + 前缀路由、Basic 认证、CORS、TLS(ALPN h2/http1.1);`StaticFileServer` 服务 WebUI 静态导出 | `ApiHub` api/hub.rs:285 |
| `src/core/` | 每请求上下文 DnsContext(ingress / request / response / marks / 类型化扩展)、域名与 IP 规则匹配原语、响应分类(CNAME 链 ≤16 跳) | `DnsContext` core/context.rs:213 |
| `src/plugin/` | 四类插件 + registry:inventory 工厂目录、Kahn 依赖排序、初始化计划(无存活依赖的 provider 跳过)、逆序销毁、API 运行时控制包装 | `init_plugins_with_runtime_controls` registry/mod.rs:187 |
| `src/config/` | YAML 模式与校验:递归 include(深度 ≤8,被包含文件插件排前)、解析后 `${VAR}/${VAR:-default}` 展开、插件 tag 语法与重复检测、行列诊断 | `init()` config/mod.rs:42 |
| `src/infra/service/` | 系统服务:service-manager 原生后端(systemd / launchd / Windows SCM),System 级安装、开机自启、OnFailure 3s 重启;Windows 失败恢复动作 | `install()` service/mod.rs:42 |
| `src/infra/upgrade/` | 自升级:GitHub release 发现 → sha256 校验下载 → tar.gz/zip 解包 → 二进制替换(备份回滚)→ WebUI 目录替换;fs2 排它锁防并发 | `apply_unchecked` upgrade/mod.rs:83 |
| `src/infra/observability/` | tracing 日志(控制台 + 滚动文件 + 1000 条环形 LogBuffer 供 SSE);拉取式指标注册表 → Prometheus 文本;热路径只允许预持有 AtomicU64 计数 | `start_logging` logging.rs:29 |
| `src/infra/task.rs` | TaskCenter:统一调度 fixed 间隔与 cron 后台任务(jiff 实现,30s 墙钟复核),持有 Weak 引用避免阻止回收 | `spawn_fixed` task.rs:480 |
| `src/infra/control.rs` | AppController:Shutdown / Reload / Restart 命令通道、运行时长、重载版本跟踪(SHA256)、采样进程指标(CPU/内存) | `AppController::new` control.rs:233 |
| `src/infra/cache/ttl.rs` | TtlCache:DashMap + ahash 的通用 TTL 缓存,采样 LRU 淘汰,供插件缓存共用 | `TtlCache` ttl.rs:44 |
| `src/infra/clock.rs` | AppClock:OnceLock 单调时钟基线(无更新任务),提供启动纪元时间戳与实例 ID | `AppClock::start` clock.rs:40 |
| `src/build_info.rs` | 编译期能力报告:bundle、启用 feature、按类别枚举存活插件工厂目录 | `snapshot()` build_info.rs:56 |

## infra/network 细分

| 子模块 | 职责 |
|--------|------|
| `listen.rs` | socket2 双栈监听(IPV6_V6ONLY=false、SO_REUSEADDR、TCP backlog 512);支持 `ip:port` / `[v6]:port` / `:port` 简写 |
| `transport/` | 传输帧:TCP 2 字节长度前缀(读写分离)、UDP、QUIC 流 |
| `tls_config/` | 全局安装 ring provider;服务端证书与可选 mTLS;客户端 webpki-roots + 0-RTT |
| `resolver/` | 引导名址解析:NameResolver 聚合多协议 NameserverClient(udp/tcp/dot/doh/doq/doh3),条目缓存上限 4096;引导服务器必须是 IP(避免鸡生蛋) |
| `upstream/` | UpstreamBuilder 按协议分发;`conn/` 各协议连接(UDP/TCP/DoT/DoQ/h2/h3);`pool/` 双池设计;`bootstrap/` 域名上游先解析 IP,ArcSwap 热换池(TTL 刷新复用池、IP 变化重建)。v1.6.1:取消/异常的查询即关闭连接并归还池容量,IPv6 方括号字面量按 IP 直连、不走域名解析,池诊断日志携带上游身份 |
| `udp_socket/` | 回包目标保留:通配监听下记录对端/本地 IP/接口(cmsg / WSARecvMsg 平台后端),保证回复源地址与查询目的地址一致 |
| `outbound.rs · proxy.rs` | 全局出站策略:命名 profile(自定义解析器 + SOCKS5),由 `config.network.outbound` 驱动,重载时整体重装 |
| `buffer_pool.rs · deadline.rs` | 8196 字节线缓冲全局池(256 个);QueryDeadline 单调预算贯穿 引导→取连接→握手→DNS I/O 全程 |
