# 设计:零停机配置热重载(Zero-Downtime Reload)

状态:进行中——PR-A(72637c47)与 PR-B(cd9f5cbc 后续提交:staged reload 主链 + 指标分代 + Unix 双绑)已实现;PR-C 中 pending 注册的指标部分已随 PR-B 落地,任务/API 路由延迟注册确认无需(任务由插件自管,API 路由在 activate 阶段注册);PR-D(状态机 phase)待实施
基线:v1.6.1 + 分支 `fix/code-review-priorities`
范围:`src/app.rs`、`src/app/bootstrap.rs`、`src/plugin/registry/runtime.rs`、`src/api/`、各 server 插件

## 1. 问题

当前重载是 stop-the-world(app.rs `handle_reload_command`):

```
校验候选配置 → 快照 previous
→ bootstrap::stop(旧 assembly)+ task::stop_all     ← 全量拆除:API、全部插件、监听器、出站配置
→ bootstrap::assemble(候选)                          ← 重新建监听、重新初始化全部插件
→ 失败则 assemble(previous) 回滚;回滚也失败 → 进程退出
```

拆除与重建之间存在**拒连窗口**(端口无监听),典型为数百毫秒到数秒(取决于插件数量与规则文件大小)。此外任何候选装配失败都会先经历一次全量拆除,坏配置 = 一次真实宕机窗口 + 回滚。

## 2. 目标与非目标

**目标**

- G1:重载期间 DNS 端口始终可服务(Unix 上做到零拒连;见 §7 Windows 说明)。
- G2:候选配置失败(解析/校验/初始化/绑定)时,旧运行时**完全不受影响**,不需要回滚。
- G3:在途请求由旧运行时自然排完(沿用现有 TaskTracker 排水语义,quic/tcp/udp 已对齐)。
- G4:重载状态机可观测(`/reload/status` 暴露阶段),失败原因可诊断。

**非目标**

- 不改变插件销毁逆序、依赖图等 registry 语义。
- 不做"部分重载"(仅热替换单个插件)——那是另一个量级的复杂度。
- 不追求 Windows 上严格零拒连(平台无 `SO_REUSEPORT`,见 §7)。

## 3. 现状关键事实(实现时以此为准)

| 事实 | 位置 |
|---|---|
| 重载入口与顺序 | `app.rs:371-429`(`handle_reload_command`) |
| 装配/拆除 | `app/bootstrap.rs:21-107`(`assemble` / `stop`) |
| 运行时构建含**全局副作用**:`install_global` 出站配置、`set_app_controller`、swap 全局 runtime 指针 | `plugin/registry/runtime.rs:62-127`(`init_runtime`) |
| API Hub 是进程级单例,插件路由注册引用当前 runtime | `api/global.rs:22`(`install_global_api`)、`api/mod.rs:85` |
| 指标注册表是全局 map,按 tag 注册 | `infra/observability/metrics.rs:124`(`register_metric_source`) |
| TaskCenter 全局单例(连接池巡检、cron、dump) | `infra/task.rs:433` |
| 服务器插件在 `init` 内绑定端口并等待启动 oneshot | `plugin/server/udp.rs:110`、`tcp.rs:160` |
| TCP 监听器 Unix 上已设 `SO_REUSEPORT` | `plugin/server/tcp.rs:427-436`(`configure_tcp_socket`) |
| UDP 监听器仅 `SO_REUSEADDR`(**需确认/补齐 reuse_port**) | `infra/network/listen.rs`(`build_udp_socket`) |
| 出站全局为 `clear_global + install_global`,已建连接持 `Arc` 不受影响 | `plugin/registry/runtime.rs:82-87` |

**双运行时并存的冲突点**(设计核心约束):全局出站配置、全局 runtime 指针、API 插件路由、指标注册(同 tag 冲突)、TaskCenter 任务重复。这些都必须推迟到"切换点"处理。

## 4. 总体方案:Build → Swap → Drain

```
阶段 B(Build)   在旧运行时继续服务时,离线构建候选运行时:
                 校验 → create + init(绑定监听)→ 全部成功?
                 失败 → 丢弃候选,旧运行时零感知(G2)
阶段 S(Swap)    生命周期锁内完成一次性切换:
                 1. 旧 servers 停止 accept(发 shutdown 信号,不等待)
                 2. 切换全局:runtime 指针、出站配置、API 插件路由、指标注册
                 3. 候选 servers 进入服务(已绑定,Unix 上与旧监听短暂重叠)
阶段 D(Drain)   等待旧 TaskTracker 排水 → 逆序销毁旧 registry → 汇报成功
```

失败路径只剩一种:Swap/Drain 阶段崩溃 → 按现行方式回滚 `assemble(previous)`(此时才需要回滚,概率已大幅缩小)。

### 4.1 构建隔离(核心改造)

`PluginRuntimeManager::init_runtime` 拆为两步(纯重构,先行独立 PR):

```rust
pub(crate) async fn build_runtime(config: Config) -> Result<PluginRegistry>;
//   - 不触碰任何全局:不 install 出站配置、不 set_app_controller、不 swap 指针
pub(crate) fn install_runtime(registry: PluginRegistry);   // 原有全局副作用集中于此
```

配套引入 `PluginCreateContext { candidate: bool }`(registry 已有 `PluginCreateContext`,加一个标志):

- `candidate: true` 时,插件的 `init` 侧副作用**延迟注册**:
  - `register_metric_source` → 改为登记在 registry 内的 pending 列表,install 阶段统一提交(避免同 tag 冲突);
  - `task_center::spawn_*`(池巡检、dump、cron)→ 同样 pending,install 时启动;
  - API 插件路由(`ApiRegister::plugin(tag)`)→ pending,install 时注册并注销旧 tag 路由;
  - 出站配置 `install_global` → 从 build 阶段移除,只在 install 执行。
- 监听器绑定照常(init 内),Unix 上 reuseport 允许与旧监听共存。

### 4.2 切换点(时序)

```
lifecycle_lock 持有:
  old.stop_accepts()            // 仅发 watch/CancellationToken 信号
  install_runtime(candidate)    // 全局指针 + 出站 + pending 注册 + 旧插件路由注销
  old_registry.destroy().await  // 逆序销毁(内含各 server destroy 的 tracker 等待)
lifecycle_lock 释放
```

- 旧 server 的 destroy 已含 `tasks.close(); tasks.wait().await`(排水),无需新机制。
- 在途请求持有的 `Arc<dyn Executor>` 保证旧插件在排水期间存活(现有语义)。
- 出站全局切换安全:旧连接池持有 `Arc` 出站 profile,`clear_global` 只影响新连接创建。

### 4.3 状态机与可观测

`AppController` 重载状态扩展为 `Idle → Validating → Building → Swapping → Draining → Succeeded/Failed(phase, reason)`;`/reload/status` 增加 `phase` 字段;每个阶段打 info 日志。忙碌拒绝语义不变(整个 B~D 期间 `ReloadBusy`)。

## 5. 分阶段实施(每步可独立合入)

| PR | 内容 | 风险 |
|---|---|---|
| PR-A | `init_runtime` 拆 `build_runtime`/`install_runtime`,行为不变;`PluginCreateContext` 加 `candidate` 标志(暂无人消费);补单测 | 低(纯重构,现有 1316 测试守底) |
| PR-B | reload 流程改为:先 `build_runtime`(含绑定)再 `stop` 旧再 `install`;**候选失败零影响**;Unix 上实现双绑重叠(确认/补齐 UDP reuse_port;quinn endpoint 用带 reuse_port 的 socket 构建) | 中 |
| PR-C | pending 注册体系(指标/任务/API 路由延迟到 install)+ 旧路由注销;Windows 路径固化(见 §7) | 中 |
| PR-D | 状态机 phase + `/reload/status` 扩展 + 文档(ai/operations-runbook.md、WebUI 文案) | 低 |

## 6. 测试计划

1. **单测**:状态机 phase 迁移;candidate 标志下 pending 注册的收集与提交;build 失败不影响全局(断言旧 runtime 指针与出站全局未变)。
2. **集成(核心验收)**:CI(Linux)循环触发 reload 的同时,以固定速率向 53 端口发查询,断言整个重载周期内 `ECONNREFUSED == 0` 且无超时(允许 P99 抖动阈值)。
3. 候选配置非法(语法/依赖环/端口被第三方占用)→ 服务不中断、状态 `Failed(Building)`。
4. 双绑重叠:重载瞬间并发查询,响应全部有效(不分新旧,均完整)。
5. 内存:重载前后 RSS 对比,双运行时峰值 ≤ 2× 插件内存(文档化该预算)。
6. Windows:行为退化验收(见 §7)——重载窗口 ≤ 现状,且候选已预验证。

## 7. 平台差异与风险

- **Windows 无 `SO_REUSEPORT`**:候选无法与旧监听同端口共存。方案:Windows 走"Build(不绑端口)→ stop 旧 → install(绑定)"——仍把校验/初始化前置,把窗口压缩到纯绑定耗时,且绑定失败时旧运行时已停,回滚路径与现状一致。用 `#[cfg(unix)]` 分支,不引入运行时开关。
- **QUIC(DoQ/DoH3)双绑**:quinn `Endpoint` 需以设置了 reuse_port 的 UDP socket 构建;若某平台不支持,退化为该协议的短暂窗口(仅该协议)。
- **双运行时资源峰值**:连接池 ×2、规则集 ×2;Drain 完成即释放。文档化;必要时在 Build 前做内存水位检查(超限直接拒绝重载,走旧路径)。
- **query_recorder(SQLite)双实例**:候选 build 会打开同一 db 文件——SQLite 支持多连接,但需确认 WAL 模式与 busy_timeout;必要时该插件声明 `single_instance`(build 阶段跳过、install 阶段重建)。
- **cron/dump 任务重复**:由 pending 注册天然解决(候选任务在 install 才启动,旧任务在 drain 停止)。
- **回退**:PR-B 起每个阶段保留配置开关 `runtime.reload_strategy = legacy | staged`(默认 staged 出问题可一键回退 legacy 路径),观察一个版本后移除。

## 8. 验收标准

- Linux CI:重载零拒连测试通过(§6.2)。
- 坏配置重载:服务零中断、零回滚发生。
- 现有全部测试通过;`/reload/status` 可观测全过程。
