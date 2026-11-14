# API 与配置

> 基线:main v1.7.1(`ad2046d3`) · 2026-11-14

## 管理 API

均在 `/api` 下,Basic 认证,可选 TLS + CORS。

| 端点 | 功能 |
|------|------|
| `GET /healthz · /readyz · /health` | 健康 / 就绪(启动、插件初始化、监听状态) |
| `GET /control` | 运行时长 + 采样进程指标(CPU / 内存) |
| `POST /shutdown · /restart · /reload` | 生命周期控制(经 AppController 命令通道) |
| `GET /reload/status` | 重载状态(pending / in_progress / last_error / 最近时间戳)与配置版本(SHA256:running_version / target_version) |
| `GET /system` | 系统信息:版本 · 构建能力(build_info)· OS / arch · 运行时长 · 配置路径 |
| `GET /config · PUT /config · POST /config/validate · GET /config/check` | 配置读取 / 保存(版本冲突检测,保存成功后触发重载;失败按 config_conflict / config_validate_failed / reload_busy / config_save_failed 区分返回)/ 校验请求体(行列诊断)/ 校验当前文件 |
| `GET /metrics` | Prometheus 文本格式指标 |
| `GET /logs · GET /logs/stream` | 环形缓冲日志尾部 / SSE 实时流(broadcast) |
| `POST /upgrade/check · /upgrade/apply · GET /upgrade/status` | 自升级(单许可信号量防并发) |
| `/plugins/<tag>/...` | 插件自注册面:matcher `GET /status · POST /mode`;provider `POST /reload`;各插件统计与控制 |
| 非 `/api` 路径 | WebUI 静态文件(Next.js 静态导出;clean URL 的 .html 兄弟规则;SPA 回退 index.html) |

## 配置文件顶层结构(config.yaml)

| 段 | 内容 |
|----|------|
| `include` | 附加配置文件;插件排在包含文件之前,递归深度 ≤8 |
| `runtime` | `worker_threads`(默认 CPU 并行度) |
| `api` | `http` 监听;`ssl`(cert/key,可选 mTLS client_ca);`auth`(basic 用户名/密码);`cors`;`webui`(root/index) |
| `log` | level(off..trace);file + rotation(Never / Minutely / Hourly / Daily / Weekly + max_files) |
| `network` | `outbound`:默认 profile + 命名 profiles(自定义解析器 nameservers/bootstrap + SOCKS5 代理) |
| `plugins` | `{tag, type, args}` 插件实例清单;tag 语法 `[A-Za-z0-9_.-]` ≤64,保留 `qs.*` 前缀 |

## 校验要点

- `worker_threads` 不为 0;日志级别必须为六个 tracing 级别之一。
- TLS cert 与 key 必须成对;`require_client_cert` 必须配 `client_ca`;Basic 认证用户名/密码非空白。
- outbound 默认 profile 必须已定义;profile 名去空白且非空。
- 插件 tag 语法与重复检测(重复报出两个下标);类型必须在工厂目录中存在。
- 整体跑一遍插件依赖图分析(`analyze_configuration`,与 `oxidns check` 同源)。

## WebUI

Next.js 16 App Router + React 19 + TypeScript + Tailwind CSS 4 + Ant Design 6,CodeMirror 6 编辑 YAML(带 lint 与 diff 视图),recharts 图表,zustand 状态;页面:dashboard / logs / plugins / settings。构建为静态导出,由升级器安装到 `webui/` 目录,运行时由 API 的 `StaticFileServer` 提供。
