# 代码评审

> 基线:main v1.7.1(`ad2046d3`) · 评审原始取证 v1.6.1(`bd183f69`)分支代码,纯静态审查 · 状态同步于 2026-11-14
>
> 下文 P0–P3 的 `file:line` 证据取自取证时的 v1.6.1 分支代码,行号在当前 main 上已漂移;结论本身逐条对照过 main 复核,凡已进 main 的修复在状态列注明。

## 评审概要

方法:四个方向并行深读(服务器与 DNS 核心 / 执行器链与缓存 / 网络基础设施 / 生命周期与 API·配置),全部结论带 `file:line` 证据;P0 与 P1 结论逐条人工复核过源码。

**修复进展**:评审问题已修复 24 项(P0×2 · P1×5 · P2×4 · P3×13),其余 11 项缓议并在下表注明原因;分支 `fix/code-review-priorities` 自 v1.6.1(`bd183f69`)起累计 117 个提交(114 个非合并:74 项代码与工程改动 + 40 项文档与学习笔记),fmt/clippy 全 bundle 通过。**2026-11-14 更新:该分支已按子系统拆分完毕并删除**,全部未合入提交已按系列落到基于 main 的独立分支(哈希映射见 `pr-doc/pr-series-review-fixes.md`「拆分执行结果」);本文及各页提到的「未进 main」提交,除已注明被上游覆盖/丢弃者外,均可在对应分支中找到。按子系统拆分后,系列 1(PR #401)与系列 2(PR #402,#412 补齐 opcode/DoQ 关停)已合入 main(随 v1.7.0 发布);系列 3 拆为四个子 PR(P1–P4),其中 P2 经历两次收敛:全量剥离版(链后剥离全部请求 ECS)与上游 PR #426(`afc23736`)的链前剥离语义冲突,已放弃;窄化版只清理**内部合成**的 ECS,另立分支 `fix/ecs-synthesized-strip`(基线 `origin/main`),待建 PR;P1(ECS scope insert 校验)与 P4(single-flight)已拆分支 `fix/cache-ecs-scope` 与 `feat/cache-single-flight`;系列 4 的 DoH 响应体上限与压缩指针环检测已分别合入上游(#431 / #432,上游随后以 `83153730` 收紧环判定),对应分支已删除;bootstrap 池的 `load_full` 调整与 probe 的有界阻塞池调整已分别单独成 PR(分支 `perf/upstream-bootstrap-load-full`、`perf/probe-blocking-pool`,基线 `origin/main`);EDNS0 系列的 E1(`EdnsOption::code()` 公共 API 重构)已由上游以 PR #415(`343f9208`)合入,E2/E3(`drop_edns0opt` executor 与 `edns0opt` matcher)未进 main——`edns0opt` matcher 当前在本地分支 `feat/edns0opt-matcher`,上游方向上以 #425 的 `edns_filter`(请求 EDNS0 选项过滤)覆盖了部分需求;系列 5–7 尚未建 PR(系列 7 的 staged reload 需按上游托管生命周期重做)。上游已推进到 v1.7.1(`ad2046d3`):#422 修复缓存 dump 覆盖(见「已知缺口」)、#433 改进配置保存错误处理、hotpath CI 增加回归门禁与 PR 自动评论(`c141fe96`、`ad2046d3`)。提交号为对应分支短哈希。

| 指标 | 数值 | 说明 |
|------|------|------|
| 已修复 | 24 | P0×2 · P1×5 · P2×4 · P3×13 |
| 缓议 | 11 | P2×6 · P3×5,均注明原因 |
| 优化方向 | 5 | 保留为后续方向 |
| 分支提交(v1.6.1 起) | 117 | 114 非合并:74 代码/工程 + 40 文档/笔记 |

## P0 立即修复(已全部修复)

### 1. 管理 API 在鉴权之前无限读取请求体(未认证 OOM)——已修复 `4a47f389`,已随系列 1 合入上游 #401

- **证据**:`api/request.rs:32-46` 的 `read_hyper_request` 无上限收集 body;`api/server.rs:177` 在剥离 `/api` 前缀、静态文件分支和 `is_authorized`(server.rs:226)之前**最先**调用它。
- **触发**:未认证客户端向任意路径(哪怕 404 的)流式发送数 GB chunked body 即可撑爆内存;示例配置 `config.yaml:12` 监听 `:9199` 且 `auth:` 处于注释状态,默认暴露面现实存在。
- **对照**:DNS 面的 DoH 服务有 64KiB 上限(`http_server.rs:226`),仅控制面缺失。
- **修复**:加 1–4MiB 上限返回 413,复用 DoH 侧 `read_hyper_body` 模式。

### 2. ECS 跨客户端缓存污染(cache 前算 key,链后注入 ECS)——分支已修复 `cc079fc7` + `e23a1870`,insert 校验未进 main

> **main 现状**:上游 #426(`afc23736`)把请求 ECS 的剥离移到链前(prepare_request),并修正 ECS source 选择与 preset 校验,缩小了暴露面;但 cache 的 insert/lazy 按当下请求重算校验 key(缓存系列 P1)尚未进 main,链中注入合成 ECS 后 key 漂移的风险在 main 上仍存在。

- **证据**:`cache/key.rs:114-138` 在链执行前用当时请求算 key;`cache/mod.rs:1408-1434` 的 insert 校验只比对 qname/qtype/qclass,不看 ECS;`ecs_handler.rs:105-142` 在下游为请求注入合成 ECS。
- **机制**:`cache → ecs_handler(send: true) → forward`(自然写法)下,客户 A(1.2.3.4)未命中 → 上游返回子网相关地理答案 → 以 `ecs_scope: None` 的全局 key 入缓存 → 客户 B(5.6.7.8)直接命中 A 的答案。
- **注意**:`ecs_in_key: true` 救不了该顺序——它只对客户端自带 ECS 生效;此组合无测试覆盖,文档也没有位置警告。
- **修复**:insert 与 lazy 刷新按**当下**请求重算/校验 key(此时可能已注入 ECS),不一致、或 key 未带 scope 而响应回显非零 scope 时拒缓存并计数 `ecs_scope_mismatch`;`ecs_handler` 在 `forward=false` 时于链后同步剥离请求 ECS,避免污染后续指令与日志。

## P1 高优先级(已全部修复)

| # | 问题 | 位置 | 要点与修复 | 状态 |
|---|------|------|------------|------|
| 3 | Windows 服务 `sc stop` 立即 exit(0),绕过优雅关停 | `service/windows.rs:117-121` | StopRequested 分支直接报 Stopped 并退出进程,应用线程从未收到信号,在途请求被拦腰截断。修复:Stop 时经 `AppController::request_shutdown()` 通知、join 应用线程后再报 Stopped | 已修 `0145cf63`(未进 main,系列 5 待发;main 的 StopRequested 仍是立即 exit(0)) |
| 4 | 阻塞 getaddrinfo 跑在 tokio worker 线程 | `dial.rs:355`、`conn/udp.rs:314-320` | 域名上游未配 bootstrap 时(文档支持的模式)每次建连在 async 上下文同步解析;系统解析器卡顿 5–30s 会占满全部 worker。修复:spawn_blocking + 解析结果缓存 | 已修 `4db4f6a0`(未进 main) |
| 5 | DoQ / DoH3 请求任务逃过关停排水不变量 | `quic.rs:277`、`http/http3_server.rs:173` | 裸 `tokio::spawn`,不在 UDP/TCP 遵守的 TaskTracker 体系内;关停/重载时与插件销毁竞态。修复:tracker 传入连接处理器,镜像 TCP 两阶段排水 | 已修 `3786db4b`(已进 main:server/quic.rs TaskTracker + #412) |
| 6 | 上游 DoH 响应体无上限,Content-Length 驱动预分配 | `conn/doh.rs:75-84`、`conn/h2.rs:235-260`、`conn/h3.rs:241-268` | 恶意/被劫持上游一个假 Content-Length 即可触发 GB 级分配;resolver 侧同类代码已修(64KiB 钳制),upstream 侧漏掉。修复:复用 resolver 的 response_buffer 上限 | 已修 `d08d26e7`(已合入上游 #431) |
| 7 | 连接池 Notify 单许可塌缩 + maintain 不通知等待者 | `pool/pipeline.rs:515-527`、`pool/reuse.rs:308-316,121-158` | 池饱和时连续归还的许可塌缩成一个,已 park 的等待者睡到 deadline——峰值负载下的"假超时"。修复:`notify_waiters()` 或整体换 `tokio::sync::Semaphore` | 已修 `3f14471d`(已进 main:pipeline.rs:142 release 时 `notify_waiters()`) |

## P2 中优先级(修复 4 项,缓议 6 项)

| # | 问题 | 位置 | 要点 | 状态 |
|---|------|------|------|------|
| 8 | TCP keepalive 未设 `with_time`,`idle_timeout` 形同虚设 | `server/tcp.rs:424` | 只改探测间隔,TCP_KEEPIDLE 仍是系统默认 ~2h;无应用层读超时兜底(slowloris 面) | 已修 `359b9a35`(已进 main,tcp.rs:428) |
| 9 | UDP 每报文无上限 spawn 任务 | `server/udp.rs:210-227` | 无信号量/限流,伪造源洪水 → 无界任务与内存 | 缓议:准入/限流策略需行为决策 |
| 10 | dual_selector 首选状态缓存无容量上限 | `dual_selector.rs:358-367` | 只有到期清理,公网唯一域名流量下内存单调增长 | 缓议:淘汰策略选型 |
| 11 | fallback / probe 以 `next=None` 运行 with_next 执行器 | `fallback.rs:374`、`dual_selector.rs:386` | cache 配成 fallback 主/备或 probe_executor 时,未命中静默无应答,校验期无诊断 | 缓议:拒绝还是接线下游需设计决策 |
| 12 | Basic auth 非恒时比较;"Basic " 大小写敏感 | `api/auth.rs:24,33` | 逐字节短路比较泄露前缀计时信号 | 已修 `06b42ea1`(已进 main,auth.rs:37 `constant_time_eq`) |
| 13 | Windows 无条件 SO_REUSEADDR、无 SO_EXCLUSIVEADDRUSE | `listen.rs:92` | 本机任意进程可绑同端口分流 DNS 流量 | 缓议:绑定行为变化需对照重绑测试验证 |
| 14 | 服务错误路径退出码 0 | `windows.rs:47-51,113-115` | 启动失败/回滚失败后 SCM 视为正常停止,OnFailure 重启策略失效 | 已修 `d10edfa7`(未进 main;main 仅 Restart 路径 exit(1)) |
| 15 | API 连接任务未跟踪,SSE 不随 stop 结束 | `hub.rs:460-465`、`logs.rs:86-117` | 重载后旧连接(含日志流)跨代存活,与 DNS 面排水契约不一致 | 缓议:管理面排水契约扩展 |
| 16 | 入站不校验 QR 位 / opcode | `server/request.rs:43-115` | QR=1 的伪造"响应"包被跑完整条流水线并应答 | 已修 `be372447`(已进 main,request.rs:62-67,补 EDNS BADVERS) |
| 17 | http_client 下载无大小上限;重定向语义宽松 | `http_client.rs:173-274` | 整包进内存可 OOM;302/303 不转 GET、转发 Authorization、允许 https→http | 缓议:上限值与重定向语义需权衡 |

## P3 小问题与优化方向(修复 13 项,缓议 5 项)

### P3 小问题

- **已修 `fc424339`** — `conn/request_map.rs` 事务 ID 随机起点 + 随机奇数步长(Kaminsky 面);未进 main(系列 4 待发);*问题区校验仍缓议——需重构已验证的无锁槽位状态机*。
- **已修 `21ff3d0c`** — `upstream/bootstrap.rs` ArcSwap Guard 改 `load_full`;已单独成 PR(分支 `perf/upstream-bootstrap-load-full`,基线 `origin/main`),未进 main。
- **已修 `2c026e91`** — 升级:下载总超时 30s→300s;二进制回滚失败改为 error 日志;未进 main(系列 5 待发)。
- **已修 `202a8403`** — `proto/name.rs` 压缩指针环检测 O(n²)→O(1)(强制严格前指);已合入上游 #432(上游随后以 `83153730` 把判定基准收紧到「上一个指针目标」,覆盖 label→pointer→label 型环)。
- **已修 `3d00fd4f`** — fallback `threshold: 0` 尊重为立即待命(serde 默认 500,文档同步);备链闸 select 加 `biased`;已随系列 2(#402)进 main。
- **缓议** — selection 中 `IncompleteAlias` 高于 `DefinitiveNegative`——经查为**有意设计**,测试 `balanced_selection_keeps_cname_only_above_negative_fallback` 明确钉死,翻转需维护者决策。
- **已修 `62594797`** — lazy 无 stale 窗口(`lazy_cache_ttl < ttl` 时)已在插件文档说明;*stale 命中未开 short_circuit 的回写仍缓议——缓存语义取舍*。
- **已修 `53f80b16`** — `TtlCache::sample_last_access` 加旋转游标,采样覆盖全表;未进 main(main 的实现仍取前 N 条)。
- **缓议** — 重载会静默重置 matcher 三态运行时覆盖——需跨代持久化设计。
- **已修 `9c82d833` / `7316413c` / `b1fc8490`** — 入站垃圾包 warn 洪水降 debug;TCP 帧长 u16 硬校验;EDNS version>0 回 BADVERS(RFC 6891);已随系列 2(#402)进 main。
- **缓议** — DoQ 协议违规不 CONNECTION_ERROR(0x3e9,RFC 9250 SHOULD)——传输层需先区分客户端流重置与内容违规,否则会误杀健康连接。
- **已修 `c3078f54`** — dump 序列化移 spawn_blocking;staging 文件名加 pid+序号防重载期交错;未进 main(缓存系列 P3;dump 覆盖保护部分已由上游 #422 覆盖)。
- **已修 `71d699d8`** — probe 阻塞操作改走 tokio 有界阻塞池(封无界线程创建);已单独成 PR(分支 `perf/probe-blocking-pool`,基线 `origin/main`;评审后补上 CLI 的 `shutdown_background()`),未进 main。该模块只服务 `oxidns probe` 诊断命令,不是服务端热路径。

### 优化方向(五项:分支落地 3 项——均未进 main,缓议 1 项,部分完成 1 项)

- **缓议 — 热路径分配削减**:评估结论——查询报文的 `Message` clone 是 Arc 引用计数 + 单 Vec 分配(极廉价),改"编码一次再 from_bytes"会引入完整解析开销、大概率负优化;真正的收益需要把上游池/连接层 API 改成 wire-bytes 通路(跨层重构),应有基准数据支撑后再动;缓存命中侧预编码响应缓冲同理(需先压测确认拷贝是瓶颈)。
- **分支已完成 `4c4cb1dc` — 冷 miss single-flight(未进 main)**:首个 miss 成为取数 leader,并发同 key miss 有界等待并在唤醒后命中 leader 的新条目(leader 失败则 waiter 重新选举接任 leader,轮次耗尽后自行回退取数);消除热门域名过期风暴,新增 `cache_single_flight_wait_total` 计数。
- **分支已完成 — 零停机重载(未进 main)**:按 `pr-doc/design-zero-downtime-reload.md` 实现完毕(`36b27388` staged 主链 + 指标分代 + Unix 双绑;`b7c93320` 状态机 phase;`72637c47` build/install 拆分;`cd9881f4` 候选 hub 路由)——Unix 上重载零拒连、坏配置零回滚;建立在旧 `prepare_assembly`/`activate_assembly` 钩子上,需按上游托管生命周期(`start_application`/`stop_application`)重新设计。
- **分支已完成 — 池化改造(未进 main)**:按 `pr-doc/design-semaphore-pool-refactor.md` 实现完毕(`ac055074` ReusePool、`cd9f5cbc` PipelinePool 迁移 Semaphore;`3e3db421` churn 基准护栏)——两池零手写 notify,唤醒丢失按构造不可能;其依赖的应急唤醒修复 `3f14471d` 已进 main。
- **部分完成 `f9055c07` — 协议细节**:UDP 预检已前移到任务创建之前(垃圾包零任务成本;已进 main,udp.rs:213);名压缩提前启用(减 TC 回退)、TCP `into_split` 改所有权半柄、recvmmsg 批量收包三项未动。

## 设计与实施(重构系列)

评审修复与两个架构级优化已全部实施并推送分支 `fix/code-review-priorities`(117 个提交:114 个非合并,74 项代码与工程改动 + 40 项文档与学习笔记;fmt/clippy 全 bundle 通过)。**2026-11-14 拆分已完成并清理:原分支已删除**,全部提交按子系统拆为基于 main 的独立分支(哈希映射见 `pr-doc/pr-series-review-fixes.md` 的「拆分执行结果」一节;拆分方案、cherry-pick 序列与 PR 描述要点见 `pr-doc/` 目录:`pr-series-review-fixes.md`(五个修复系列)、`pr-series-staged-reload.md`、`pr-series-semaphore-pools.md`、`pr-series-edns0.md`、`pr-split-cache-hardening.md`(缓存系列四拆);学习笔记与 PR 文档迁至 `learning-doc` 分支)。现状:系列 1/2 已合入 main(#401/#402,#412 补齐);系列 3 拆为 P1–P4 并已落分支(`fix/cache-ecs-scope`、`perf/cache-ttl`、`feat/cache-single-flight`,P2 窄化版即 `fix/ecs-synthesized-strip`);EDNS0 系列 E1 已由上游 #415 合入,matcher 已拆 `feat/edns0opt-matcher`,`drop_edns0opt` 被上游 #425 `edns_filter` 覆盖后丢弃;系列 4 的 #431/#432 已合入上游,余量拆 `fix/upstream-hardening`;系列 5 拆 `fix/service-upgrade`;系列 6 拆 `feat/semaphore-pools`(v2 方案另见 `feat/upstream-pool-capacity-signal`);系列 7 拆 `feat/staged-reload`。以上均已基于 main 落盘、待建 PR。

| 系列 | 提交 | 核心内容 | 状态 | 风险 |
|------|------|----------|------|------|
| 1 管理 API 加固 | 3 | 请求体上限(鉴权前 OOM)· 恒时 auth 比较 · 配置保存原子化 | 已合入 #401 | 低 |
| 2 DNS 服务加固 | 8 | QR/opcode 预检(+任务前移)· BADVERS · TCP 帧/keepalive · DoQ 排水 · 日志降级 · fallback threshold | 已合入 #402(#412 补齐) | 低 |
| 3 缓存正确性与性能 | 4 拆为 P1–P4 | **P0** ECS 跨客户端污染修复(insert/lazy 校验 + 请求侧剥离)· single-flight(含重新选举)· 采样 LRU 轮转 · dump 加固 · stale 语义文档 | P2 窄化版(`fix/ecs-synthesized-strip`)待建 PR;P1/P3/P4 未进 main;dump 覆盖保护部分由上游 #422 覆盖 | 低 |
| 4 上游与协议健壮性 | 7 | DoH 响应体上限 · 阻塞解析出 worker · 随机事务 ID · 指针环 O(1) | #2、#6 已合入上游(#431/#432);#3、#7 已单独成 PR;其余未建 PR | 低 |
| 5 平台与升级 | 3 | Windows SCM 优雅关停 · 错误退出码 · 升级超时/回滚日志 | 未建 PR,未进 main | 中 |
| 6 Semaphore Pools(重构) | 6 | ReusePool/PipelinePool 迁移 Semaphore;两池零手写 notify;churn 基准护栏;含应急唤醒修复 `3f14471d`(该项已拆 `fix/upstream-pool-wakeup-collapse` 待建 PR) | 已拆 `feat/semaphore-pools`(栈式依赖 `feat/staged-reload`);v2 方案另在 `feat/upstream-pool-capacity-signal` | 中 |
| 7 Staged Reload(重构) | 5 | Unix 重载零拒连、坏配置零回滚;指标分代;`/reload/status` 带 phase;插件路由进候选 hub;非 Unix 回退仅条件编译 | 已拆 `feat/staged-reload`(已按 main 的 `StartupSession` 生命周期适配落盘),待建 PR | 中 |
| EDNS0 选项过滤与匹配 | 4 | `EdnsOption::code()` 公共 API(纯重构前置)· `drop_edns0opt` executor · `edns0opt` matcher · WebUI 注册 | E1 已由上游 #415 合入;E2/E3 待建 PR(`edns0opt` matcher 当前在本地分支 `feat/edns0opt-matcher`) | 低 |

> 另有共享 lint 提交 `87cad1b2`(横跨系列 1/4/5 的文件,须在其依赖的提交之后携带);分支元数据提交(设计文档状态、系列拆分与学习笔记)不进上游。当前合并顺序:P1(ECS scope 校验)→ P4(single-flight,需 P1)→ P3 → 系列 4 剩余/5 → 6(semaphore;`3f14471d` 上下文已在 main)→ EDNS0 E2/E3 → 7(staged-reload,体量最大且需按上游托管生命周期重做)。缓存子 PR 一律从 main 新建分支,避免重新引入 #403,冲突预期见 `pr-doc/pr-split-cache-hardening.md`。

> 系列 7 的两个追加修复:`cd9881f4` 在 `stage_runtime` 之前安装候选 API hub,使插件 `init()` 期的 scoped route 进入候选 hub(staging 失败时恢复旧 register),staged reload 后 `/api/plugins/<tag>/*` 不再 404;`03ac4154` 用 `#[cfg(not(unix))]` 把 stop-the-world 回退限定到非 Unix 构建,消除 aarch64-musl 构建的 `unreachable statement` 告警,Unix 运行时行为不变。

> 发布验证:`aarch64-unknown-linux-musl` release 二进制已在 `0e4c67a9` 重建,编译告警消失;UPX 4.2.2 `-9 --lzma` 压缩后 10.94 MB → 3.97 MB(36.3%),`upx -t` 自检通过;环境中无 qemu,压缩产物未做运行验证。

## 分支特有(未进 main)

以下实现只存在于本仓库分支,正文其余章节均按 main 描述:

- **Staged Reload(系列 7)**:`36b27388` / `b7c93320` / `72637c47` / `cd9881f4` / `03ac4154` — Unix 重载零拒连、`/reload/status` phase 状态机、候选 hub 路由。main 用托管生命周期 + stop-then-rebuild(见 03-flows),系列 7 需按新生命周期重做,方案见 `pr-doc/design-zero-downtime-reload.md`、`pr-doc/pr-series-staged-reload.md`。
- **Semaphore Pools(系列 6)**:`ac055074` / `cd9f5cbc` / `3e3db421` — 双池迁移 Semaphore,唤醒丢失按构造不可能;方案见 `pr-doc/design-semaphore-pool-refactor.md`。
- **Cache single-flight(缓存系列 P4)**:`4c4cb1dc` / `659dcf15` — 冷/过期未命中 single-flight + leader 重新选举,`single_flight_wait_ms` / `single_flight_max_rounds` 配置与 `cache_single_flight_wait_total` 指标;main 无此机制。
- **Cache dump 加固(缓存系列 P3)**:`c3078f54` — dump 序列化 spawn_blocking + pid/序号 staging;覆盖保护语义已由上游 #422 以不同方式实现。
- **ECS 窄化剥离(缓存系列 P2)**:分支 `fix/ecs-synthesized-strip`(基线 `origin/main`)— 只清理内部合成的 ECS;全量链后剥离版与 #426 链前语义冲突已放弃。
- **EDNS0 插件对(系列 E2/E3)**:`drop_edns0opt` executor(分支提交 `9ba81e58`)与 `edns0opt` matcher(本地分支 `feat/edns0opt-matcher`,提交 `3a6bf04d`);`EdnsOption::code()` 前置重构已由上游 #415 合入。上游的 `edns_filter`(#425)覆盖了请求侧 EDNS0 过滤的需求面。
- **其余待发单项**:bootstrap `load_full`(`perf/upstream-bootstrap-load-full`)、probe 有界阻塞池(`perf/probe-blocking-pool`)、阻塞解析出 worker(`4db4f6a0`)、随机事务 ID(`fc424339`)、系列 5 Windows/升级三项。

## 已验证无问题的方面

解压缩炸弹防护(指针环/255 字节/指针数上限齐全)、缓存 TTL 与 RFC 2308 负缓存语义、dump 加载校验、JoinSet 取消安全(v1.6.1 后池状态机与 Drop 路径经交叉验证一致)、include 环与 env 展开安全、静态文件路径穿越防护、升级 sha256 强制校验与 zip-slip 防护、TCP 两阶段排水、`exec_restart` 参数保真、QueryDeadline 单调预算、TaskCenter 调度与停止顺序、Windows udp_socket(近两次修复后)——这些常见雷区均已正确处理。

## 已知缺口

**ISSUE-001 消息编解码失去第三方差分校验(仍待处理)**:上游 `38ce2436` 删除了 `message_hickory_compat.rs` 与 `hickory-proto` dev 依赖,当前 `oxidns-proto` 的 wire 测试全部是编码器/解码器自洽回环,无法发现对称性错误(名字压缩、EDNS 字段语义、截断边界、SVCB 参数顺序等)。建议用外部实现生成的原始字节 fixture 补单向或双向校验,详见 `learning/issues.md` 的 ISSUE-001;在补齐前,涉及 `crates/proto` 编解码的改动应按跨实现兼容性风险对待。

**ISSUE-002 重载失败回滚覆盖缓存 dump(已由上游修复)**:失败重载时,候选 cache 实例在 `destroy()` 中把空/部分缓存写回同一个 `dump_file`,覆盖旧代的完整 dump,回滚后只能加载被覆盖后的条目。上游 PR #422(`1992c8f9` `fix(cache): preserve persisted entries across failed reloads`)已按「持久化提交门」思路修复:激活前先恢复并裁剪 dump,周期持久化只在运行时成功激活后启动,不可读/非法/超时的 dump 受保护不被覆盖。`learning/issues.md` 的 ISSUE-002 已转为「已修复(上游)」;原 `fix/cache-hardening` 分支基线是修复前的 `871f8da3`,已于 2026-11-11 删除(提交归档于标签 `archive/cache-hardening`),缓存子 PR 一律从 main 新建分支。

## 上游动态(2026-11-07 起)

上游从 v1.6.1 时代(`bd183f69`)推进到 v1.7.1(`ad2046d3`),对后续 PR 拆分与重做的主要影响:

1. **系列 1/2 合入**(#401 / #402 / #412):管理 API 加固与 DNS 服务加固随 v1.7.0 进入 main;#412 补齐 opcode/DoQ 关停。
2. **#422 修复缓存 dump 覆盖**(#403):见「已知缺口」;改动集中在 `cache/mod.rs`、`cache/persistence.rs`、`plugin/registry/*`、`app.rs`、`app/bootstrap.rs`,与缓存系列 P1/P3/P4 高度重叠,cherry-pick 需人工合并。
3. **应用层生命周期重写**(`d11368b2` + #422):上游把启动/激活/拆除收敛为托管生命周期(`src/app/bootstrap.rs` 的 `start_application` / `stop_application`),重载语义为「先停旧代,装配失败再按旧配置重建」。本仓库的系列 7(staged reload,Unix 零拒连)建立在旧的 `prepare_assembly` / `activate_assembly` 钩子上,需按新基线重新设计,不能直接 cherry-pick。
4. **文档站点重构**(`b6818c4f`):插件参考迁到 `website/docs/`,英文版在 `website/i18n/en/docusaurus-plugin-content-docs/current/`,AI 指南迁到 `docs/ai/`;缓存系列的文档补丁需按新路径重放。
5. **EDNS0 前置重构合入**(#415 `343f9208`):`EdnsOption::code()` 公共 API 已进 main,EDNS0 系列 E1 完成;上游另以 #425 加入 `edns_filter`(请求 EDNS0 选项过滤)、#419 加入 `load_balance`,插件类型基线 65→68。
6. **其它上游增量**:#421 缓存应答 IP 过滤与批量删除(#7885a134)、#423 RouterOS 路由(#461d4f14)、社区 WebUI(#c72c60fe)、#433 配置保存错误处理等同样落在缓存/上游系列的重叠面上,出 PR 前需逐个核对差异面。
7. **hotpath CI 落地**(`c141fe96` + `ad2046d3`):基准报告迁入 GitHub Actions,新增回归门禁与 PR 自动评论(`hotpath.yml` + `hotpath-comment.yml`),基准文档在 `docs/benchmarks.md`;热路径改动的 PR 现在会自动带上基准对比。
