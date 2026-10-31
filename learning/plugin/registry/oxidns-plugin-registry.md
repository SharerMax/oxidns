# OxiDNS 插件 registry 实现解析

> 分析基线：`v1.6.1`，分支 `fix/code-review-priorities`（HEAD `04ac8a64`）。
> 代码位置：`src/plugin/registry/`（`catalog.rs` 89 行、`context.rs` 194 行、`init_plan.rs` 136 行、`mod.rs` 678 行、`runtime.rs` 370 行、`tests.rs` 714 行）；
> 运行时结构定义在 `src/plugin/mod.rs`（593 行），依赖分析在 `src/plugin/dependency.rs`（664 行），应用集成在 `src/app/bootstrap.rs` 与 `src/app.rs`。
> 生态位概述见 `../oxidns-plugin.md`；本文只描述当前代码实际行为；与文档/注释不一致之处集中在第 13 节。

## 1. 定位

`registry` 不是插件，而是**插件系统的基础设施**：它把"编译期注册的工厂目录"变成"运行期可服务的插件实例集合"，并负责依赖解析、初始化排序、运行时控制、世代切换（reload/重启）与销毁。所有插件（server/executor/matcher/provider）的生命周期都从这里经过。

三层结构：

| 层 | 类型 | 生命周期 | 职责 |
|---|---|---|---|
| 工厂目录 | `PluginCatalog` | 进程级，`OnceLock` 不可变 | 从 inventory 收集全部 `#[plugin_factory]` 注册，按 plugin_type 提供构造函数 |
| 实例注册表 | `PluginRegistry` | 每个运行时世代一份 | 工厂表 + 实例表（tag → `PluginInfo`）+ 初始化顺序 + 依赖解析 API |
| 世代管理器 | `PluginRuntimeManager` | 进程级单例 | 持有"当前 runtime"指针、串行化世代切换、托管 `AppController` |

`PluginBuildSession` 与 `PluginRuntime` 都是 `PluginRegistry` 的类型别名（`registry/mod.rs:75-76`）——同一个结构在"构建期"和"运行期"扮演两种角色，仅用别名与生命周期所有权表达区分。

## 2. 代码地图

| 文件 | 行数 | 职责 |
|---|---:|---|
| `mod.rs` | 678 | `PluginRegistry` 结构、`init_plugins` 七步初始化、依赖解析 getter、销毁、锁中毒恢复辅助 |
| `catalog.rs` | 89 | 进程级不可变工厂目录，inventory 收集 + 重复类型检测 |
| `context.rs` | 194 | `PluginInitContext`（工厂/插件拿到的解析器门面）、`PluginResolver` trait、`init_quick_setup` |
| `init_plan.rs` | 136 | 从依赖图报告推导初始化计划：live 集剪枝、跳过 provider、构造 create contexts |
| `runtime.rs` | 370 | `PluginRuntimeManager`：init/stage/commit/destroy 世代管理、AppController 集成、全局门面函数 |
| `tests.rs` | 714 | 重复 tag、锁中毒恢复、provider 剪枝、运行时控制、quick setup dependents 传递等 |

`registry` 之外的关键协作文件：

| 文件 | 职责 |
|---|---|
| `src/plugin/mod.rs` | `Plugin`/`PluginFactory`/`UninitializedPlugin`/`PluginHolder`/`PluginInfo`、inventory 注册宏、`analyze_configuration` 静态分析入口 |
| `src/plugin/dependency.rs` | `DependencySpec`、图构建 + 四类校验、Kahn 拓扑排序、DFS 环路径提取 |
| `src/plugin/runtime_control.rs` | `PluginRuntimeControl` 枚举（Matcher/Provider 两态）+ `drain` 分发 |
| `src/plugin/provider/control.rs` | `ProviderRuntimeControl`：reload 串行化（try_lock）、shutdown 拒绝、drain |
| `src/app/bootstrap.rs` | `prepare_assembly`（stage）→ `activate_assembly`（commit）编排 |
| `crates/macros/src/lib.rs` | `#[plugin_factory]` 派生宏（仅支持无状态 unit struct） |

## 3. 工厂注册路径

```mermaid
flowchart TD
    A["插件 crate 编译期<br/>plugin_factory 派生宏 / register_plugin_factory!"] --> B["inventory::submit<br/>FactoryRegistration<br/>plugin_type + module_path + fn 构造器"]
    B --> C["进程首次访问时<br/>PluginCatalog::from_inventory<br/>inventory::iter 收集"]
    C -->|"重复 plugin_type"| E1["Err: inventory 无效"]
    C -->|"OK"| D["GLOBAL_CATALOG: OnceLock<br/>不可变 HashMap&lt;type, kind+ctor&gt;"]
    D --> E["每个 PluginRegistry::load_catalog<br/>逐工厂调用 ctor() 得到独立实例"]
    E --> F["registry.factories + factory_kinds"]
```

要点（`catalog.rs:26-47`、`plugin/mod.rs:169-206`）：

- 注册信息只有三个字段：类型名、`module_path!()`、`fn() -> Box<dyn PluginFactory>`。**类别（DependencyKind）不靠注册者声明，而是从 module_path 字符串推断**：包含 `::plugin::server::` → Server，executor/matcher/provider 同理（`plugin/mod.rs:192-206`）；有单元测试保证所有注册插件都能落到合法类别（`plugin/mod.rs:567-592`）。
- `#[plugin_factory]` 宏只接受无状态的 unit struct / 空 brace struct；有状态工厂须改用 `register_plugin_factory!`（`crates/macros/src/lib.rs:31`）。
- 目录进程级只建一次（`OnceLock`，失败结果也被缓存——重复类型错误会永久生效）；但**每个 PluginRegistry 通过 `iter_factories()` 重新调用构造函数**得到自己的工厂实例（`registry/mod.rs:144-149`）。工厂无状态，克隆成本低。
- `analyze_configuration`（`plugin/mod.rs:108-167`）绕过 registry 直接从 inventory 重建工厂表做静态依赖分析，供 config 加载、CLI 校验和 API 依赖图端点使用——启动主路径不走它。

## 4. 启动初始化主流程

入口在 `PluginRuntimeManager`：`init_runtime`（冷启动，`runtime.rs:62-100`）或 `stage_runtime`（staged reload 构建，`runtime.rs:160-193`），两者都落到 `build_registry` → `PluginRegistry::init_plugins_with_runtime_controls`（`registry/mod.rs:187-346`）。

```mermaid
flowchart TD
    S0["Step 0：重复 tag 校验<br/>带 plugins[i]/plugins[j] 索引"] --> S1
    S1["Step 1：未知 plugin_type 校验<br/>（先于依赖分析，报错更友好）"] --> S2
    S2["Step 2：analyze_dependencies<br/>每个工厂上报 DependencySpec<br/>图构建 + 四类校验 + Kahn 拓扑排序"] --> S3
    S3["build_runtime_init_plan<br/>从非 provider 节点出发做可达性标记<br/>不可达 provider 剪枝 + warn"] --> S4
    S4["Step 3：prepare_startup 钩子<br/>仅 live 插件，按源配置顺序<br/>（download 等预置文件）"] --> S5
    S5["build_create_contexts<br/>按边反向汇总 dependents<br/>sorted_plugins 按拓扑序重排"] --> S6
    S6["Step 4：逐插件 create → init<br/>→ 包 runtime_control → PluginInfo"] --> S7{"DashMap insert<br/>返回旧值？"}
    S7 -->|"Some（重复）"| E1["Err: Duplicate runtime tag"]
    S7 -->|"None"| S8["init_order.push(tag)"]
    S8 --> S6
    S6 -->|"全部完成"| OK["info: All plugins initialized"]
```

要点：

- **依赖收集刻意保持轻量**（`registry/mod.rs:214-222` 注释）：启动期只做图校验，完整的 schema 解析/校验只发生在各工厂 `create()` 内一次，避免双重解析。
- **prepare_startup 按源配置顺序而非拓扑序**（`registry/mod.rs:254-260` 注释）：启动副作用（如 download 建预置文件）只要求可预测，不要求依赖先就绪。
- **失败即回滚**：`build_registry` 中 `init_plugins_with_runtime_controls` 失败会 `candidate.destroy().await`，把已初始化的插件按逆序清掉再返回错误（`runtime.rs:135-142`）；`init_runtime` 路径上 `install_outbound_profiles` 之后的所有失败也会 `outbound::clear_global()`（`runtime.rs:89-93`）。
- **重复运行时 tag 防御**：DashMap 的 `insert` 会静默覆盖，故检查返回值（`registry/mod.rs:331-340`）。配置期已在 Step 0 拦截，这里兜底 quick setup 等运行时合成 tag 的冲突。
- 每个插件的创建参数里都带上了"谁引用我"（dependents），见第 8 节。

## 5. 依赖分析与拓扑排序（dependency.rs）

每个工厂通过 `get_dependency_specs` 声明"我引用了谁"。以 sequence 为例（`sequence/mod.rs:347-395`）：把每条规则的 `matches[i]` / `exec` 解析成 `DependencySpec::matcher/executor(...)`，quick setup 表达式则递归展开被引工厂的 `get_quick_setup_dependency_specs` 并生成 `args[i].exec -> quick_setup(type).field` 复合字段名（`plugin/mod.rs:234-276`）。

```mermaid
flowchart TD
    A["收集全部 DependencySpec"] --> B{"逐边校验"}
    B -->|"target == self"| C1["自引用错误"]
    B -->|"target 不存在"| C2["missing 错误"]
    B -->|"expected_kind != 实际 kind"| C3["类别不匹配错误"]
    B -->|"expected_plugin_type 不符"| C4["类型不匹配错误"]
    C1 & C2 & C3 & C4 --> D["errors 汇总一次性报出"]
    B -->|"通过"| E["入度 = 去重依赖数<br/>反向图 dep→dependents"]
    E --> F["Kahn：入度 0 的 tag 排序入队"]
    F --> G["出队 → 递减 dependents 入度<br/>归零再入队"]
    G --> H{"sorted 数 == 节点数？"}
    H -->|"否"| I["find_cycle_path DFS<br/>报具体环路径 a -> b -> a<br/>否则列出全部未决节点"]
    H -->|"是"| J["DependencyGraphReport<br/>nodes 按 tag 排序 / edges 按字段排序<br/>init_order 即拓扑序"]
```

要点（`dependency.rs:198-454`）：

- 四类校验（自引用/缺失/类别/具体类型）**先收集全部错误再一次性报出**（`dependency.rs:319-324`），不是遇错即停，方便用户一次修完配置。
- **确定性**：初始队列按 tag 排序（`dependency.rs:328-337`）、边按 `(field, target_tag, expected_kind)` 排序（`dependency.rs:298-307`）、节点按 tag 排序——同一份配置永远产出同一份 report 与 init_order。这对 CLI diff、API 图端点和测试都重要。
- 环检测在 Kahn 之后：若拓扑序不完整，用三色标记 DFS 在剩余节点中**提取具体环路径**（`find_cycle_path`，`dependency.rs:397-454`），报错形如 `Circular dependency detected: a -> b -> a`；理论上 DFS 找不到环时退化为列出全部未决节点（防御分支）。
- `resolve_dependencies`（`dependency.rs:181-196`）是早期 API，现标记 `allow(dead_code)` 仅测试使用；主路径是 `analyze_dependencies` + registry 自己消费 report。

## 6. Live 集剪枝（init_plan.rs）

依赖图完整拓扑排序后，`build_runtime_init_plan` 做**可达性剪枝**：provider 只有被非 provider 插件（间接）引用才初始化。

```mermaid
flowchart TD
    A["遍历全部节点"] --> B{"kind != Provider？"}
    B -->|"是"| C["加入 live 集并入栈"]
    B -->|"否"| D["暂不加入"]
    C --> E["沿边正向传播<br/>target 入 live 集则入栈"]
    D --> E
    E --> F["收敛后：provider 且不在 live 集"]
    F --> G["记入 skipped_providers（按 tag 排序）<br/>warn: no live dependents"]
    F --> H["过滤 report：<br/>nodes/edges/init_order/sequence_flows<br/>仅保留 live 成员"]
```

要点（`init_plan.rs:23-97`）：

- 起点是**全部非 provider 插件**——server/executor/matcher 天然 live；provider 链（provider → provider）会通过边传播正确保留（`init_plan.rs:32-49`）。
- 未被引用的 provider（及其独占依赖）被跳过初始化并告警，但不报错——配置里"备用"数据集是合法的。
- `build_create_contexts`（`init_plan.rs:99-136`）按边反向汇总：target 的 dependents 列表 = 所有指向它的边，按 `(tag, field, plugin_type)` 排序，且**只含 live 边**——剪掉的 provider 不会出现在任何 dependents 里（有专项测试 `tests.rs:479`）。

## 7. 单个插件的创建与初始化

`create_plugin_info_and_init`（`registry/mod.rs:352-408`）是每个插件的生命周期关键点：

```mermaid
flowchart TD
    A["PluginInitContext::new<br/>registry Arc + tag + create_context"] --> B["factory.create(config, init_context)<br/>同步，纯解析/校验/构建"]
    B --> C["uninitialized.init_and_wrap(context)<br/>调用 Plugin::init（异步）"]
    C --> D{"holder 类别？"}
    D -->|"Matcher 且 api feature<br/>且 api.http 已配置"| E["附加 MatcherRuntimeControl"]
    D -->|"Provider（无条件）"| F["附加 ProviderRuntimeControl"]
    D -->|"其他"| G["无 runtime_control"]
    E & F & G --> H["PluginInfo<br/>tag / plugin_name / holder / args / control"]
    H --> I["plugins.insert(tag, Arc&lt;PluginInfo&gt;)"]
```

要点：

- `init_and_wrap`（`plugin/mod.rs:86-106`）把 `Box<dyn Xxx>` 初始化后转成 `Arc` 存入 `PluginHolder` 四态枚举；`init` 阶段插件即可通过 `init_context` 解析自己的依赖目标（依赖已按拓扑序先行初始化）。
- **runtime control 挂载规则不对称**（`registry/mod.rs:368-397`）：Provider 无条件挂（支持 API reload）；Matcher 仅在 `api` feature 且 `config.api.http.is_some()` 时挂（`runtime.rs:134` 传入开关）——无 API 的构建不为之付出 Arc/控制结构成本。
- `PluginInfo` 保留原始 `args`（YAML Value），供 API 运行时控制路由等按 tag 查询。

## 8. 依赖解析 API

运行期/构建期统一通过两类门面解析依赖：

| 门面 | 使用者 | 能力 |
|---|---|---|
| `PluginInitContext`（`context.rs:52-144`） | 工厂 `create` / 插件 `init` | `tag()`、`dependents()`、`executor/executor_of_type/matcher_ref/provider/provider_of_type`、`plugin`（裸 PluginInfo）、`init_quick_setup` |
| `PluginResolver` trait（`context.rs:16-50`） | 需要解析能力的其他组件 | 同上除 tag/dependents 外的解析方法，`PluginRegistry` 实现 |

所有 getter 共享同一校验骨架（`registry/mod.rs:447-595`）：缺失报 `references missing plugin`、类别不符报 `expects executor plugin, but ...`、可选的 `*_of_type` 进一步校验具体 plugin_type。`matcher_ref` 额外把表达式级取反（`reverse`）与该 matcher 配置 tag 的运行时控制捆绑成 `MatcherRef`（`registry/mod.rs:531-547`）。

**quick setup 的特殊路径**：sequence 构建链时对 `$tag` 引用走 registry 查询，对 `type param` 内联表达式则经 `init_quick_setup`（`context.rs:133-143`）直接 `factory.quick_setup` + `init_and_wrap`：

- 合成 tag 形如 `qs.exec.<seq_tag>.<node>.<type>` / `qs.match.<seq_tag>.<node>.<match>.<type>`（`sequence/chain.rs:626-629`、`655-658`）；
- 使用 `PluginCreateContext::default()`（无 dependents）；
- **不进入 `registry.plugins`，也不进 `init_order`**——它们是 sequence 私有实现细节（`registry/mod.rs:365-367` 注释），不占 `plugin_count`、不可被 API reload；
- 销毁由持有它们的 sequence 插件代劳：`Sequence::destroy` 遍历 `quick_setup_executors/matchers` 逐一调用 `destroy()`（`sequence/mod.rs:145-157`），链条闭合。

`dependents()` 是反向视图："谁引用我"。目前生产插件无消费者，属预留能力（`plugin/mod.rs:216-224` 注释）。

## 9. 运行时控制（runtime controls）

`PluginRuntimeControl`（`runtime_control.rs`）是挂在 `PluginInfo` 上的类别级控制句柄，当前两态：

**Provider**（`provider/control.rs:33-90`）——reload 串行化器：

```mermaid
flowchart TD
    R["reload() 调用"] --> A1{"accepting_reloads?"}
    A1 -->|"false"| X1["Err: shutting down"]
    A1 -->|"true"| A2{"try_lock reload_lock"}
    A2 -->|"被占"| X2["Err: Busy（不排队）"]
    A2 -->|"获得"| A3["再查 accepting（双检）"]
    A3 --> A4["tokio::spawn 独立执行 provider.reload()"]
    A4 -->|"调用方被 cancel"| A5["spawned 任务继续持有锁<br/>reload 不中断"]
```

- 并发 reload **立即拒绝而非排队**（`try_lock_owned`）；调用方任务被 abort 时，reload 在独立任务中继续完成并自然释放锁（有专项测试 `control.rs:164-206`）。
- `drain()`（`control.rs:77-80`）：置 `accepting_reloads = false` 后**等待**当前 detached reload 完成——运行时销毁用它保证"旧快照构建不会与替代者初始化重叠"。

**Matcher**（`matcher/control.rs`，`api` feature）——序列链运行期的 matcher 基准结果控制（API 可动态调整匹配行为），由 `MatcherRef` 在每次求值时先查控制再应用表达式取反。

销毁时的统一语义：`PluginRuntimeControl::drain` 对 Matcher 是空操作、对 Provider 等待 reload 排空（`runtime_control.rs:20-28`）。

## 10. 运行时世代管理（runtime.rs）

`PluginRuntimeManager`（`runtime.rs:43-56`）是进程单例（`GLOBAL_MANAGER: OnceLock`，`runtime.rs:297-303`），持有：

- `current: RwLock<Option<Arc<PluginRegistry>>>` —— 当前世代指针（读多写少，请求路径只读）；
- `controller: Mutex<Option<Arc<AppController>>>` —— **唯一权威所有者**，世代切换不会丢失或竞态它（`runtime.rs:231-236` 注释）；
- `lifecycle: Arc<AsyncMutex<()>>` —— 串行化所有世代转换（init/stage/commit/destroy）。

**冷启动 `init_runtime`**（`runtime.rs:62-100`）：

```mermaid
flowchart LR
    L["lifecycle 锁"] --> T["spawn 任务"]
    T --> R1["retire_previous_runtime<br/>先销毁旧世代"]
    R1 --> R2["install_outbound_profiles<br/>清空 + 安装新全局 outbound"]
    R2 --> R3["build_registry<br/>catalog → init_plugins"]
    R3 -->|"失败"| R4["outbound::clear_global + Err"]
    R3 -->|"成功"| R5["install_runtime<br/>写 current 指针"]
```

先 retire 再 build 是**已知的耦合**：插件初始化会查进程级全局（outbound profile），候选者将替换它，故旧世代必须先退位（`runtime.rs:83-87` 注释；staged reload 设计将解除该耦合，见 `pr-doc/design-zero-downtime-reload.md`）。

**Staged reload**（零停机路径，`runtime.rs:160-213`；应用编排 `bootstrap.rs:83,119`、`app.rs:540-547`）：

```mermaid
sequenceDiagram
    participant App as app reload 编排
    participant M as PluginRuntimeManager
    participant Old as 旧世代 runtime
    participant New as 候选 runtime

    App->>M: stage_runtime(config)
    Note over M: begin_metric_staging：候选指标进暂存区
    M->>New: build_registry（不 retire、不换 outbound）
    Note over New: 初始化期拨号看到旧世代 outbound（已知权衡）
    M-->>App: 候选 Ok → flush_metric_staging（发布候选指标）
    App->>App: prepare_assembly 完成（旧世代持续服务）
    App->>Old: bootstrap::stop（停止 + destroy 旧世代）
    App->>M: commit_staged_runtime(candidate, config)
    M->>M: 换 outbound 全局 + 写 current 指针
    M-->>App: 新世代激活
    Note over M: 失败路径：abort_metric_staging，current 不变
```

要点：

- stage 阶段**只在冷启动**（`current_runtime()` 为 `None`）才安装 outbound（`runtime.rs:171-174`）；reload 场景下候选初始化期看到的是旧 profile，这是注释里明示的权衡。
- **指标暂存**：候选的 metric 注册进 staging 列表，避免旧世代销毁时按 tag 注销误删候选的同名序列；`flush_metric_staging` 在旧世代销毁**之前**发布，使旧世代的注销只删掉自己那代最旧的条目（`runtime.rs:161,176-181,188-191`）。
- commit 要求旧世代**已销毁**（由 app 编排保证顺序：先 stop 再 activate）。
- `destroy_runtime`（`runtime.rs:215-229`）：取走 current → 旧世代 destroy → `outbound::clear_global()`。
- debug 构建有可选的测试串行化开关（`enable_runtime_test_serialization` + `TestRuntimeGuard`，`runtime.rs:20-40`）：测试持 `OwnedMutexGuard` 存入 registry，下一个 init 需等前一个测试的运行时真正销毁——guard 的获取刻意放在 lifecycle 锁**之外**，避免死锁（`runtime.rs:63-66` 注释）。

## 11. 销毁流程

`PluginRegistry::destroy`（`registry/mod.rs:632-663`）：

```mermaid
flowchart TD
    A["clone init_order"] --> B{"为空？"}
    B -->|"是"| Z["释放测试 guard，直接返回"]
    B -->|"否"| C["逆序遍历"]
    C --> D["plugins.remove(tag)"]
    D --> E["runtime_control.drain()（provider 等 reload）"]
    E --> F["plugin.destroy()"]
    F -->|"失败"| G["error 日志，继续下一个"]
    F -->|"成功"| C
    C -->|"完毕"| H["清空 init_order"]
```

要点：

- **逆 init_order 销毁**——依赖者先于被依赖者消亡，与初始化严格对称。
- 销毁前先 `drain` runtime control，保证 provider 的异步 reload 不会与销毁竞态（第 9 节）。
- 单个插件 destroy 失败**不中断**整体销毁，只记 error。
- 世代销毁由 manager 驱动（retire/destroy_runtime），`AppController` 的清除由 app 层负责。

## 12. 测试覆盖（registry/tests.rs）

| 测试组 | 代表用例 |
|---|---|
| 基础行为 | registry 创建、查询不存在的 tag 返回 None（`tests.rs:35-47`） |
| 错误传播 | init 失败后运行时保持停止态、`current` RwLock 中毒可恢复（`tests.rs:70-129`） |
| 运行时控制 | 无 API 时不挂 matcher control；destroy 等待被取消的 provider reload；reload 拒绝非 provider/缺失 tag（`tests.rs:49-68,181-238,622-700`） |
| 初始化计划 | 不可达 provider 链剪枝、quick setup dependents 传入 create context、create contexts 只含 live dependents（`tests.rs:314-562`） |
| reload 编排 | manager 层 reload_provider 走 current runtime（`tests.rs:622`） |

`plugin/mod.rs` 与 `dependency.rs` 内嵌测试另覆盖：全量注册插件的 kind 推断、拓扑序确定性、四类校验错误文案、环路径提取。

## 13. 设计要点与观察

**设计要点**：

- **毒锁恢复**：本模块所有 `Mutex`/`RwLock` 都经 `lock_mutex`/`read_rwlock`/`write_rwlock` 包装，中毒时恢复守卫值而非传播 panic（`registry/mod.rs:46-70`）——持锁线程 panic 不应永久瘫痪世代切换，也不应把失败的 init 误判为成功。临界区本身都极小（swap/take/clone）。
- **DashMap 换取 `Arc<Self>` 内部可变性**：`plugins` 用 DashMap 使 registry 可以 `Arc` 共享给插件的同时继续注册实例。
- **无状态工厂 + 每世代重建**：目录进程级一份不可变，工厂实例每世代新造——世代之间零共享，reload 隔离彻底。
- **错误一次报全**：依赖校验收集全部错误统一返回，改善配置排错体验。
- **确定性排序贯穿**：Kahn 初始队列、边、节点、dependents、skipped providers 全部显式排序。

**观察**：

1. **重复 tag 双重校验**：`init_plugins` 的 Step 0（`registry/mod.rs:194-202`，报 `plugins[i]`/`plugins[j]` 索引）与 `analyze_dependencies` 开头（`dependency.rs:212-218`，报依赖图错误）检查同一条件，文案与错误类别不同。Step 0 先触发，后者实际不可达；两处信息各有价值（索引 vs 图上下文），保留可辩护但属冗余。
2. **循环内的未知类型检查不可达**：Step 1 已对全部配置做过 `factories.contains_key`，创建循环内的逐插件检查（`registry/mod.rs:307-315`）永远不触发，纯防御。
3. **`plugin_tags()` 顺序不稳定**：直接取 DashMap 迭代序（`registry/mod.rs:598-603`）；`runtime_controls()` 则显式按 tag 排序。调用方若依赖 tag 顺序需自行排序。
4. **quick setup 插件的指标孤儿**：quick setup 插件若在 `init` 里注册指标源（如 forward），其 `destroy` 由 sequence 代调，指标注销路径正常；但它们不占 `plugin_count`、不出现在任何 API 插件列表，与"tag 可见性"直觉有偏差——排障时需知道 `qs.exec.*`/`qs.match.*` 前缀 tag 的来源。
5. **`load_catalog` 与 `analyze_configuration` 的轻度重复**：两者都从 inventory 重建工厂表并做重复类型检测（`registry/mod.rs:144-149` vs `plugin/mod.rs:117-136`）。后者面向静态分析独立成套，代价是同类逻辑两份。
6. **staged reload 的 outbound 可见性权衡**是注释里显式承认的已知限制（候选初始化期拨号用旧 profile），依赖此行为的新插件应参考 `pr-doc/design-zero-downtime-reload.md` 而非假设所见即所得。
