# OxiDNS 代码架构解析

> 插件驱动的高性能 DNS 服务器 · Rust + Tokio 多线程运行时
>
> 基线:v1.6.1(`release/v1.7.0` 开发线) · 同步日期:2026-11-05
>
> 规模:`src/` 下 291 个 Rust 源文件 · 5 个 workspace crate

本目录由 `learning/oxidns-architecture.html` 拆分而来,按原页面的七个章节组织,保留全部表格、流程步骤与评审结论。流程图改用 Mermaid 重绘,便于在 Markdown 中阅读和 diff。

## 章节索引

| 文件 | 原页面标签 | 内容 |
|------|-----------|------|
| [01-overview.md](01-overview.md) | 总览 | 核心请求路径、规模统计、Workspace 组成、Cargo 功能 Bundle |
| [02-architecture.md](02-architecture.md) | 架构与模块 | 分层架构、主要模块职责、`infra/network` 细分 |
| [03-flows.md](03-flows.md) | 关键执行流程 | 启动、DNS 热路径、配置热重载、自升级、服务安装与守护 |
| [04-plugins.md](04-plugins.md) | 插件目录 | 插件组合机制,Server / Matcher / Executor / Provider 清单 |
| [05-api-config.md](05-api-config.md) | API 与配置 | 管理 API 端点、`config.yaml` 顶层结构、校验要点、WebUI |
| [06-lifecycle.md](06-lifecycle.md) | 生命周期与嵌套 | 普通插件生命周期、sequence 生命周期与深度剖析、嵌套语义、限制、暗坑 |
| [07-review.md](07-review.md) | 代码评审 | P0–P3 评审结论、重构系列状态、已验证项、已知缺口 |

## 建议阅读顺序

1. 先读 [01-overview.md](01-overview.md),建立请求路径与 crate 边界的整体印象。
2. 再用 [02-architecture.md](02-architecture.md) 对照目录结构理解分层约束。
3. [03-flows.md](03-flows.md) 覆盖启动、热路径与重载三条最重要的执行链。
4. 需要写配置或排查插件组合时查 [04-plugins.md](04-plugins.md) 与 [05-api-config.md](05-api-config.md)。
5. 修改 `sequence`、`cache`、`fallback` 等组合型插件前,务必读 [06-lifecycle.md](06-lifecycle.md)。
6. [07-review.md](07-review.md) 记录评审发现、修复状态与仍然存在的缺口。

## 维护说明

- 本目录是学习材料,不是行为契约。代码、配置与测试始终是事实来源。
- 更新内容时优先核对:`Cargo.toml` 特性图、`src/plugin/*/mod.rs` 注册目录、`src/build_info.rs`、`config*.yaml`,以及 `pr-doc/` 中的系列状态。
- 与 `learning/issues.md` 的编号保持一致(当前为 ISSUE-001)。
- 原始 HTML 保留在 `learning/oxidns-architecture.html`,用于查看带样式的可视化版本。
