# PR:feat(webui): surface degraded providers with badges and reload recovery

> 目标分支:`svenshi/oxidns:main`(堆叠于 feat/provider-degraded-api,合入后自动改基)· 对应 issue #371 · milestone v1.8.0

## 摘要

PR-1 让规则文件缺失时 provider 降级启动,PR-2 把降级状态暴露到 API。本 PR 是系列的收尾:让用户在 WebUI 插件中心**看见**降级状态并一键恢复——这也是 issue #371 场景里把"报错"送到用户眼前的最后一环(此前插件中心的 `status: "running"` 是硬编码,后端真实状态不会呈现)。

## 行为与交互

- **插件卡片**:provider 处于降级状态时显示黄色边框 + "降级运行 / Degraded" 徽标,tooltip 列出未加载的规则文件路径。
- **详情面板**:新增 `ProviderDegradedNotice` 警告卡片,展示降级说明与"未加载的规则文件"列表,并指向 reload 按钮作为恢复路径。
- **reload 按钮行为不变**:成功后局部更新;由于 PR-2 的 reload 响应已携带权威降级状态,前端直接回填 store,**不再额外发一次 status 请求**。

## 数据流设计(复用既有刷新骨架)

遵循仓库既有的更新机制——**采样而非轮询**,不新增 `useVisiblePolling` 通道:

```
loadConfig / 配置 apply          → refreshProviderStates()(与 refreshMatcherStates 并列)
POST /plugins/{tag}/reload 成功  → 响应回填 providerStates,无需再请求
插件列表增删 / 离线 / 断连        → reconcile / 重置
```

并发保护沿用 store 既有三件套:generation 计数、backendKey 校验、isOfflineMode 检查。卡片经细粒度 Zustand selector 订阅,一次更新只重渲染受影响的卡片。

## 类型与实现

- `ProviderRuntimeState { availability, status, degradedFiles, error? }` + `reconcileProviderStates()`(provider-reload.ts)。
- `ProviderReloadResponse` 扩展 `status`/`degraded_files`;新增 `fetchProviderStatus()`(oxidns-api.ts)。
- 降级判定只依赖 `availability === "ready" && status === "degraded"`;API 不可达时显示为不可用而非降级,避免误报。

## 配置影响

无。不新增任何配置字段或后端依赖(PR-2 的端点已就位)。

## 测试

- `provider-reload.test.ts`:新增 2 个用例——`reconcileProviderStates` 保留已知状态/为新增 provider 注入 loading 默认值/剔除失效 tag;空 provider 列表返回空 map。
- 既有 269 个 webui 用例(含 i18n 覆盖审计)不受影响。

已运行的命令:

```
pnpm test          # 271 passed / 0 failed
pnpm typecheck     # tsc --noEmit 干净
pnpm lint          # eslint 干净
```

## 文档跟进

- website 插件参考的"文件缺失行为"说明与 release note 默认行为变化标注(系列统一在 PR-1 合入时处理)。
