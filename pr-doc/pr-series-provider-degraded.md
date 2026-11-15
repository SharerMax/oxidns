# PR 系列:Provider 规则文件缺失优雅降级

对应需求:[svenshi/oxidns#371](https://github.com/svenshi/oxidns/issues/371)(milestone v1.8.0)
对应设计:`design/design-rule-file-degraded-startup.md`(已按设计落地)
基线:`main` @ `ad2046d3`(v1.7.1);三个 PR 堆叠,依序合入
PR body:分别见 `pr-body/pr-body-provider-degraded-startup.md`、`pr-body/pr-body-provider-degraded-api.md`、`pr-body/pr-body-provider-degraded-webui.md`

> **执行结果(2026-11-15)**:三个分支已推送到 fork `SharerMax/oxidns`,PR 未建。发 PR 时 base 依次指向前一个分支(PR-3 → PR-2 → PR-1 → main),每个 PR 合入后 GitHub 自动改基。

## 分支与提交清单

| PR | 分支 | 提交 | 内容 | 依赖 |
|---|---|---|---|---|
| PR-1 | `feat/provider-degraded-startup` | `36207054` `feat(provider): degrade instead of aborting startup when rule files are missing` | `TextSource` 容错模式(`scan_tolerant`/`open_replay_tolerant`);domain_set/ip_set/adguard_rule 按文件跳过并记录 degraded;geosite/geoip 整体降级(仅 `DnsError::Plugin`,`Runtime` 保持致命);`Provider::degraded_files()` trait 方法;单元 + 集成测试 | 无 |
| PR-1 | 同上 | `7e9f7d13` `test(provider): align geo failure-path test with degraded startup semantics` | 过时用例对齐新语义(见 §3,该提交同时消除了 ISSUE-004 的整批挂起) | PR-1 第 1 项 |
| PR-2 | `feat/provider-degraded-api` | `1b590a19` `feat(api): expose provider degraded state` | reload 响应扩展 `status`/`degraded_files`;新增 `GET /api/plugins/{tag}/status`;`plugin_degraded` gauge(`ProviderRuntimeControl::new` 注册、`drain` 注销);API 契约测试 + 端到端断言 | PR-1(`degraded_files` trait) |
| PR-3 | `feat/provider-degraded-webui` | `43a98ea2` `feat(webui): surface degraded providers with badges and reload recovery` | `providerStates` 采样通道(loadConfig/apply/reload 三个采样点,不轮询);卡片降级徽标 + tooltip;详情面板 `ProviderDegradedNotice`;i18n zh/en;vitest 用例 | PR-2(status 端点与响应字段) |

合并记录:PR-2 分支经 `141ccd18` 合入 PR-1 的 `7e9f7d13`;PR-3 自 `141ccd18` 切出。

## 核心决策(评审时可能被问到的点)

1. **插件自处理而非 registry 编排**:init = "空快照的 reload",与现有 reload 失败保留旧快照语义对称;符合 AGENTS.md 类别内聚约束。
2. **不加 `optional` 开关**:默认行为直接翻转(零配置面);若社区要求 fail-fast,后续增量补反向开关,schema 兼容。
3. **降级粒度**:多源插件按文件跳过(内联规则与其余文件仍生效);geosite/geoip 无内联兜底,整体降级为空快照。
4. **保持致命**:args 解析失败(`create()` 阶段)、线程 panic/启动失败(`DnsError::Runtime`,不掩盖基础设施 bug)、adguard 规则内容错误(reload 失败保留旧快照)、`TextSource` 扫描间指纹不一致。
5. **日志级别用 `error` 而非 `warn`**:规则不生效是功能事故;配套 `plugin_degraded` gauge + status 端点 + WebUI 徽标三层可观测。
6. **热路径零改动**:快照结构与成员查询完全不变,改动全部在 init/reload 编排层。

## 默认行为变化(需在 release note 显式标注)

规则文件缺失从"拒绝启动"变为"降级运行"。典型风险场景:Docker 卷未挂载、路径笔误、下载任务失败——拦截/分流规则可能静默失效。兜底:error 日志 + gauge + WebUI 徽标。文档跟进项:README/website 插件参考"文件缺失行为"说明。

## 测试证据

- 后端:`cargo test --lib` 1302 passed / 0 failed;`cargo test --test plugin_integration` 并行 **116 passed / 0 failed,29.89s**;nightly fmt、all-features clippy 干净(main 上预先存在的 `dial.rs` needless_bool 与 `tar` 未用依赖告警除外)。
- 集成用例 `test_provider_missing_rule_file_degrades_startup_and_recovers`:带缺失文件的配置启动成功 → 匹配失效 → 文件补齐 reload 恢复 → 匹配生效。
- 前端:`pnpm test` 271 passed(含新增 reconcile 用例与 i18n 覆盖审计)、`pnpm typecheck`、`pnpm lint` 干净。
- 基线对照:main 代码并行 115 passed / 29.68s,排除挂起为预存问题。

## 执行期间发现并解决的问题

1. **ISSUE-004(已定位并修复)**:全包并行集成测试整批挂起。根因是过时的 `test_geo_provider_failure_paths_are_reported` 在新语义下 `expect_err` panic——此时 `plugin::init` 已成功激活运行时,panic 使其永远不会被 destroy,泄漏的全局状态污染后续并行用例。`7e9f7d13` 对齐用例语义(并显式 `destroy()`)后,并行运行全部干净。教训:`expect_err` 应放在 init 之前,或失败路径也要显式销毁已激活的 runtime。详见 `learning/issues.md`。
2. **ISSUE-003(待处理,与本系列无关)**:`dynamic_domain_set` 测试并行运行时全局 API 路由竞争(固定 tag `"learned"` + 未获取 `global_api_test_guard`)。在 main 上即可复现,失败集随机漂移。建议其用例补护栏,或 CI 对该模块降并行。详见 `learning/issues.md`。

## 发 PR 操作提示

```
# body 文件在 learning-doc 分支的 pr-doc/pr-body/ 下
gh pr create --repo svenshi/oxidns \
  --head SharerMax:feat/provider-degraded-startup \
  --base main \
  --title "feat(provider): degrade instead of aborting startup when rule files are missing" \
  --body-file pr-doc/pr-body/pr-body-provider-degraded-startup.md
# PR-2 / PR-3 同理,--base 分别暂设为前一个分支的分支名;合入后改基 main
```
