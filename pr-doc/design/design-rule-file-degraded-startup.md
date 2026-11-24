# 设计:规则文件缺失时优雅降级(报错但不阻止启动)

状态:设计定稿,未实现(对齐 milestone v1.8.0)
需求来源:[issue #371](https://github.com/svenshi/oxidns/issues/371)(来自 TG 群 Haru_831)
基线:`main` @ `a5ae6e15`
范围:`src/infra/io/text_source.rs`、`src/plugin/provider/{geosite,geoip,ip_set,domain_set}.rs`、`src/plugin/provider/adguard_rule/{mod,compiler}.rs`、`src/plugin/provider/mod.rs`、`src/api/runtime_control.rs`、`webui/lib/store.ts` 及插件卡片组件
取代:无(首版)

## 0. 背景与诉求

Geosite、AdGuard Rule 等依赖外部规则文件的插件,文件缺失/损坏时 `init()` 返回
`Err`,`StartupSession::initialize` 随之走 `session.abort()`(`src/plugin/registry/runtime.rs:65`),
整个进程拒绝启动。用户诉求:**报错,但不阻止启动**。

### 0.1 现状加载点

| 插件 | 内联规则 | 文件源 | 文件缺失时 |
|---|---|---|---|
| `domain_set` | ✅ `exps` | ✅ `files`(多个) | init 失败,启动中止 |
| `adguard_rule` | ✅ `rules` | ✅ `files`(多个) | init 失败,启动中止 |
| `ip_set` | ✅ | ✅ 多个 | init 失败,启动中止 |
| `geosite` / `geoip` | ❌ 无 | 单文件(dat/protobuf) | init 失败,启动中止 |
| matcher 文件规则源(`TextSource`) | ✅ | ✅ 多个 | init 失败,启动中止 |

### 0.2 已有可复用机制

- Provider 快照 = `ArcSwap` + 默认空快照(如 `GeoSiteSnapshot::default()`),
  "匹配不到任何东西"在数据结构上天然安全;
- 运行时 reload(`POST /api/plugins/{tag}/reload`,实现在 `src/api/runtime_control.rs:129`)
  失败时保留旧快照——降级语义可以与之对称;
- provider reload 是**全量重建**:从原始 args 重读内联 + 全部文件再原子换快照,
  文件补齐后 reload 即恢复,无需重启;
- `download` executor 有 `startup_if_missing` + `prepare_startup` 钩子,
  可在 provider init 前下载文件,构成自愈闭环。

## 1. 核心决策

| 决策点 | 结论 | 理由 |
|---|---|---|
| 处理位置 | **插件自己处理**,registry 不动 | init = "空快照的 reload",与现有 reload 失败保留旧快照的语义天然对称;每个插件才知道降级意味着什么;符合 AGENTS.md "category-local lifecycle semantics stay within their owning plugin package" |
| 是否加 `optional` 开关 | **不加**,作为新默认行为 | 零配置面(PluginConfig schema、WebUI 定义、locale 均不动);后续有 fail-fast 需求再增量补反向开关 |
| 日志级别 | **error**(非 warn) | 规则不生效是功能事故,warn 会淹没在日常日志里 |
| 降级粒度 | 多源插件**按文件跳过**;单文件插件**整体降级为空快照** | domain_set/adguard 有内联兜底,缺一个文件不应作废其余源;geosite/geoip 无兜底,整体空 |
| 保持致命 | args 解析失败(`create()` 阶段);`TextSource` 扫描间指纹不一致 | 配置写错必须让用户改;指纹不一致是并发写保护,语义不同,不应静默跳过 |
| matcher 文件规则源 | **第二期** | 与 `$set` 引用、内联混排交织,避免单 PR 行为面过大 |

**必须接受的代价**:默认行为翻转后,"规则静默失效"取代"启动失败"成为新的失败形态
(典型场景:Docker 卷未挂载、路径笔误、下载任务失败)。兜底依赖三层可观测性
(error 日志 + degraded gauge + WebUI 徽标),并在 release note 显式 call out。

## 2. 后端设计

### 2.1 共享容错加载(`src/infra/io/text_source.rs`)

`TextSource` 现状:文件缺失 → 整个构建 `Err`。新增 tolerant 模式(新方法或选项):

- 单文件缺失/不可读 → `error!` 日志(含 tag、路径、原因)+ 跳过该文件,其余源正常扫描;
- 扫描结果携带 `skipped_files: Vec<(path, reason)>`,由 provider 转入快照的 degraded 标记;
- 指纹不一致(扫描间文件被并发改写)保持中止语义不变。

### 2.2 各 provider 改造点

| 插件 | 文件 | 行为 |
|---|---|---|
| `domain_set` | `domain_set.rs` | 逐文件容错;快照记录 degraded 文件 |
| `adguard_rule` | `mod.rs` + `compiler.rs`(`build_rule_buckets`) | 同上 |
| `ip_set` | `ip_set.rs` | 同上(多文件) |
| `geosite` / `geoip` | `geosite.rs` / `geoip.rs` | 单文件:仅 `DatFileSession::open` 的 I/O 失败(`DnsError::Io`)→ `error!` + degraded 标记;dat 内容/选择器错误(`DnsError::Plugin`)保持致命;degraded 状态独立于快照,reload 失败时保留上次正常 matcher |

统一约束:

- **文件不可用**才降级:多源 provider 按文件跳过;单文件 provider 仅 `DnsError::Io` 走降级;
- **config 级错误**(selector 解析失败、空文件名等)仍在 `create()` 阶段致命;**内容错误**(非法 CIDR、无匹配条目、dat 解析失败等)保持 `Err`,不能伪装成文件不可用;
- `reload()` 在文件不可用时返回 `Ok(())` + degraded 标记,但不替换已发布的快照:启动时快照为空,reload 时保留上次正常 matcher;
  只有真正致命的构建错误(任务 panic、内存失败、内容错误)保持 `Err`。

### 2.3 Provider trait 扩展(`src/plugin/provider/mod.rs`)

```rust
/// Files that failed to load in the last snapshot build (empty = healthy).
fn degraded_files(&self) -> Vec<String> { Vec::new() }
```

### 2.4 API 契约(`src/api/runtime_control.rs`)

**① reload 响应扩展**(向后兼容,`ok` 语义不变):

```jsonc
// 完全成功(现状不变)
{ "ok": true, "action": "reload_provider", "provider": "adguard",
  "status": "reloaded", "degraded_files": [] }

// 带降级成功(新增;HTTP 仍 200)
{ "ok": true, "action": "reload_provider", "provider": "geosite-cn",
  "status": "degraded", "degraded_files": ["./geosite.dat"] }
```

handler 在 reload 成功后读 `provider.degraded_files()` 填充;
文件缺失不再落入 `provider_reload_failed`(400)。

**② 新增 provider 状态端点** `GET /api/plugins/{tag}/status`
(仿照 matcher 已有路由,复用 `register_plugin_runtime_control_routes` 注册循环):

```jsonc
{ "ok": true, "provider": "adguard", "status": "degraded",
  "degraded_files": ["./rules/missing.txt"] }
```

**③ 可观测性**:provider 注册 `plugin_degraded` gauge(1/0,label `plugin_tag`),
走现有 `MetricSource` 机制;WebUI 展示走 status 端点,gauge 用于告警/面板聚合。

### 2.5 热路径影响:零

快照结构、`contains_name`/`contains_ip` 等查询路径完全不变;改动全部在
init/reload 编排层。

## 3. WebUI 设计(复用既有刷新骨架)

WebUI 插件列表当前是**前端从 `GET /api/config` 解析派生**的
(`pluginsFromConfig`,`oxidns-config.ts:94`),`status: "running"` 为硬编码,
后端真实状态不会自动呈现——必须补运行时通道。遵循既有更新机制
(**采样而非轮询**,不新增 `useVisiblePolling` 通道):

1. **store**(`webui/lib/store.ts`):新增 `providerStates: Record<tag, ProviderRuntimeState>`;
   在 `loadConfig` 采样点(`store.ts:500` 旁)加 `refreshProviderStates()`,与
   `refreshMatcherStates` 并列;配置 apply、provider reload 成功后局部再采样;
   沿用 backendKey / configLoadGeneration / isCurrentBackend 防脏数据三件套。
2. **卡片徽标**(`plugin-card-template.tsx`):`status === "degraded"` 时显示黄色
   Badge + tooltip 列出缺失文件;细粒度 selector 订阅,不惊动整页。
3. **详情面板**:provider 详情展示 degraded 文件列表 + "重新加载"按钮
   (现有 reload 动作,响应已带 `degraded_files`)。
4. **不动的东西**:`plugin-definitions/*.ts`(表单 schema 无新字段)、
   `configFromPlugins` 保存链路(降级是运行时状态,不进 YAML)。
5. **i18n**:`webui/lib/i18n` 中英文文案(degraded 徽标、tooltip、reload 部分成功提示)。

## 4. 恢复闭环

```
文件缺失 → 降级启动(error 日志 + gauge=1 + status API degraded)
   ↓ 用户放置文件 / download 插件(startup_if_missing)下载成功
   ↓ POST /api/plugins/{tag}/reload(或 WebUI 按钮)
全量重建(args 里全部内联 + 文件)→ ArcSwap 原子换快照
   ↓ gauge=0,status=reloaded,徽标消失
```

全自动组合:`download`(startup_if_missing)+ 配置 apply 触发的采样,
文件补齐后无需人工介入。

## 5. 实施拆分与测试

| PR | 内容 | 测试 |
|---|---|---|
| PR-1 `feat(provider): degrade instead of aborting startup when rule files are missing` | `text_source.rs` 容错模式;5 个 provider 改造 + `Provider::degraded_files` | 每 provider 补"缺失→Ok/空快照"、"多文件缺一→其余生效"、"文件恢复→reload 收编";`tests/plugin_integration.rs` 补"带缺失文件的配置启动成功 + matcher 返回 false + reload 恢复" |
| PR-2 `feat(api): expose provider degraded state` | reload 响应扩展、`GET /api/plugins/{tag}/status`、gauge | API 测试更新响应结构断言 |
| PR-3 `feat(webui): surface degraded providers` | store 通道、徽标、i18n(zh/en) | webui 单测 |

文档同步(按 `docs/ai/change-impact-matrix.md` 触发):

- `README.md` / `README_EN.md`:能力描述更新;
- website 插件参考:geosite/geoip/adguard_rule/domain_set/ip_set 页面加"文件缺失行为"说明;
- **release note 必须标注破坏性默认值变化**(文件缺失从拒绝启动 → 降级运行);
- `config*.yaml` 注释可顺带提及(非必需)。

## 6. 风险与回退

- 无 hot path 风险;
- 唯一行为风险是"规则静默失效",由 error 日志 + gauge + WebUI 徽标三层兜底;
- 若社区反馈强烈,后续 PR 增量加 `optional: false` 反向开关恢复 fail-fast,
  schema 兼容,无需迁移。
