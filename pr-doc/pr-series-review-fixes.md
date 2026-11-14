# PR 系列:评审修复(Review Fixes)

将分支 `fix/code-review-priorities` 上代码评审产生的修复提交,按子系统拆为可独立评审的系列。两个重构系列(staged-reload / semaphore-pools)见各自文档;本文覆盖其余全部提交。

> **现状(2026-11)**
> - 系列 1(PR #401)与系列 2(PR #402)已合入 `release/v1.7.0`(merge `52ae666c` / `12f6de71`);上游在 #402 合入后又补了 opcode/DoQ 修复(#412, `4daa4f2c`),本系列无需再动。
> - 分支已合并 `origin/main`(`488237ff`,+6 提交:docker ARMv7、hotpath-cloud 配置);**未**合并 release。
> - 系列 3 的原 PR 分支 `fork/fix/cache-hardening` **已删除**(本地与 fork),提交以标签 `archive/cache-hardening` @ `770f51a0` 归档;该分支基线为修复前的 `871f8da3`,不含上游 #422,直接出 PR 会重新引入 #403。系列 3 已拆为 P1–P4 四个子 PR,方案见 `pr-split-cache-hardening.md`;其中 P2 的全量剥离版(链后剥离全部请求 ECS)与上游 #426(`afc23736`)的链前剥离语义冲突,已放弃并删除分支(归档标签 `archive/ecs-request-strip`),窄化版(只清理**内部合成**的 ECS)另立分支 `fix/ecs-synthesized-strip`,基线 `origin/main` @ `a3dd95c0`,PR **未建**,正文 `pr-body-ecs-synthesized-strip.md`;旧正文 `pr-body-cache-hardening.md` 同样仅作历史参考。
> - 系列 4/5、semaphore、staged 均**未建 PR**;系列 4 的 `d08d26e7`(上游 DoH 响应体上限)与 `202a8403`(压缩指针环检测 O(1))已分别合入上游(PR #431 / PR #432),对应分支已删除,提交以标签 `archive/upstream-doh-body-limit`、`archive/proto-compression-pointer-loop` 归档;`21ff3d0c`(bootstrap 池改 `load_full`)与 `71d699d8`(probe 走有界阻塞池)已分别单独成 PR,分支 `perf/upstream-bootstrap-load-full` 与 `perf/probe-blocking-pool`,基线均为 `origin/main`,正文 `pr-doc/pr-body-upstream-bootstrap-load-full.md` 与 `pr-doc/pr-body-probe-blocking-pool.md`。
> - **分支累计 92 个提交**(此前本文按 79 计数,实际同步点为 80,已修正)。新增 EDNS0 系列 4 个提交(`9ba81e58`、`6e5574db`、`6fa9d32e`、`42eb9377`),拆分方案见 `pr-series-edns0.md`。

全局依赖说明:

- `87cad1b2`(chore: satisfy rustfmt and clippy on windows nightly)横跨 api / upstream / upgrade 三个系列的文件,**必须**在它依赖的提交之后 cherry-pick:api 系列的 `06b42ea1`(已随 #401 合入)、upstream 系列的 `4db4f6a0` 与 `d08d26e7`。建议随**最后合并**的系列携带,或手工拆分;不携带不影响 Linux CI。系列 1/2 已合入且未携带。
- 合并顺序建议:系列 3(先发,P0)→ semaphore(需 `3f14471d` 上下文)→ staged-reload(体量最大)。
- 分支元数据提交共 34 个(含 1 个与 `origin/main` 的合并),不进入上游 PR;其中学习笔记性质的子集只作研究素材。

---

## 系列 1:管理 API 加固(3 提交)— ✅ 已合入(#401)

PR 标题:`fix(api): harden request body, auth, and config save`

| # | 提交 | 内容 |
|---|---|---|
| 1 | `4a47f389` | 请求体 4MiB 上限;超出返回 413 |
| 2 | `06b42ea1` | Basic auth 恒时比较 + scheme 大小写不敏感 |
| 3 | `0d13fa54` | `PUT /config` 互斥串行化 + temp-file rename 原子写 |

测试:恒时比较 4 项;既有 API 测试全绿。已合入。

## 系列 2:DNS 服务加固(8 提交)— ✅ 已合入(#402)

PR 标题:`fix(server): ingress validation, framing, and drain hardening`

| # | 提交 | 内容 |
|---|---|---|
| 1 | `359b9a35` | TCP keepalive 补 `with_time`(TCP_KEEPIDLE) |
| 2 | `3786db4b` | DoQ/DoH3 任务纳入 TaskTracker,关停排水对齐 |
| 3 | `be372447` | 入站 QR/opcode 预检,回答 REFUSED |
| 4 | `9c82d833` | 恶意 UDP 包 warn 降为 debug |
| 5 | `7316413c` | TCP 帧长 u16 硬校验 |
| 6 | `b1fc8490` | EDNS version>0 回 BADVERS |
| 7 | `3d00fd4f` | fallback `threshold: 0` 尊重 + select `biased` |
| 8 | `f9055c07` | UDP 预检前移到任务创建前 |

已合入;上游 #412(`4daa4f2c`,opcode 响应修正 + DoQ 关停解堵)已兜底。

## 系列 3:缓存正确性与性能(7 提交)

PR 标题:`fix(cache): ECS scope validation, single-flight, and persistence hardening`

| # | 提交 | 内容 |
|---|---|---|
| 1 | `cc079fc7` | **P0**:insert/lazy 刷新校验 ECS scope 一致 + 拒绝 scope>0 回显——修复跨客户端缓存污染;`ecs_scope_mismatch` 计数 |
| 2 | `53f80b16` | 采样 LRU 旋转游标 |
| 3 | `c3078f54` | dump 序列化移入 `spawn_blocking`;staging 唯一名(pid+序号) |
| 4 | `62594797` | lazy 无 stale 窗口语义文档化 |
| 5 | `4c4cb1dc` | 冷/过期 miss single-flight;`enable()`+复查;2 项测试 |
| 6 | `659dcf15` | 失败后有界重新选举;`single_flight_wait_ms`/`single_flight_max_rounds` 配置化与校验;指标文档与 WebUI schema/本地化 |
| 7 | `e23a1870` | ecs_handler 请求侧剥离——`forward=false` 时 post-stage 同时移除请求 ECS;与 #1 语义配套 |
| 8 | `64407aac` | 缓存跳过原因 WebUI 本地化(中/英 `webui.ts` 文案 + `metrics.ts` 键),无 Rust 行为变化;分支上为 rebase 后的 `770f51a0` |

按表序;#5 依赖 #1 的同文件上下文;#6 依赖 #5;#8 独立。

已删除分支 `fork/fix/cache-hardening`(归档标签 `archive/cache-hardening` @ `770f51a0`)相对 release 的 diff 为 12 个提交:8 系列 + `bc838851`(`62594797` 之后附加的缓存文档说明,自 `7e2d66c2` 拆出)+ `ea831344`(nightly fmt 绕行)+ `20f48f70`(缓存 ECS 文档中/英同步,源提交 `38aec719`)+ 1 个 release 合并。按文档顺序 cherry-pick 时,先取 1–8;`e23a1870` 与 #1 配套、单独引入;配套提交均不带额外行为。拆分后的子 PR 与新基线见 `pr-split-cache-hardening.md`。

验证(分支最终态):`cargo check --locked --all-targets --all-features` 干净;聚焦 `ecs_handler` 9/9 通过;`fmt` 干净;分支此前一次全量 1368 项通过。

## 系列 4:上游与协议健壮性(7 提交)— 未建 PR(其中 #2、#6 已合入上游,#3 已单独成 PR)

PR 标题:`fix(upstream): bound responses, async resolution, and unpredictable IDs`

| # | 提交 | 内容 |
|---|---|---|
| 1 | `4db4f6a0` | 系统 DNS 解析移入 `spawn_blocking`;deadline 可取消 |
| 2 | `d08d26e7` | 上游 DoH/H2/H3 响应体 64KiB 硬上限 —— **已合入上游 PR #431**:分支 `fix/upstream-doh-body-limit` 已删除,归档标签 `archive/upstream-doh-body-limit` @ `970e7612`,正文见 `pr-doc/pr-body-upstream-doh-body-limit.md` |
| 3 | `21ff3d0c` | bootstrap 查询改 `load_full` —— **已单独成 PR**:分支 `perf/upstream-bootstrap-load-full`(基线 `origin/main` @ `ad2046d3`),提交 `66f1f56f`(评审后由 `load_full()` 改为「克隆 `Arc` 并立即释放 `Guard`」,提交信息按 arc-swap 文档措辞修正),正文见 `pr-doc/pr-body-upstream-bootstrap-load-full.md` |
| 4 | `fc424339` | 上游事务 ID 随机起点 + 奇数步长 |
| 5 | `1f828545` | style:rustfmt 十六进制字面量(#4 的格式尾随) |
| 6 | `202a8403` | 压缩指针环检测 O(n²)→O(1),强制前指 —— **已合入上游 PR #432**:分支 `perf/proto-compression-pointer-loop` 已删除,归档标签 `archive/proto-compression-pointer-loop` @ `75d3e4ab`(上游随后以 `83153730` 进一步收紧判定基准),正文见 `pr-doc/pr-body-proto-compression-pointer.md` |
| 7 | `71d699d8` | probe 阻塞操作走有界阻塞池 —— **已单独成 PR**:分支 `perf/probe-blocking-pool`(基线 `origin/main` @ `ad2046d3`),提交 `e8569e95`(评审后补上 CLI 的 `shutdown_background()`,避免卡住的解析阻塞命令退出),正文见 `pr-doc/pr-body-probe-blocking-pool.md` |

按表序;#4+#5 可合并;#6 在 `crates/proto`,可单独成 PR;#2、#6 已合入上游(#431 / #432),分支已删除并以归档标签保留;#3 已单独成 PR(基线 `origin/main`)。

## 系列 5:平台与升级(3 提交)— 未建 PR

PR 标题:`fix(service,upgrade): graceful SCM stop and upgrade hardening`

| # | 提交 | 内容 |
|---|---|---|
| 1 | `0145cf63` | Windows SCM Stop 优雅关停(oneshot 排水,≤20s) |
| 2 | `d10edfa7` | 服务错误路径 exit(1) |
| 3 | `2c026e91` | 下载总超时 30s→300s;回滚失败改 error 日志 |

按表序;#2 依赖 #1。

---

## 汇总

| 项 | 提交数 | 状态 |
|---|---|---|
| 系列 1 API 加固 | 3 | ✅ 已合入 release/v1.7.0(#401) |
| 系列 2 DNS 服务加固 | 8 | ✅ 已合入(#402) |
| 系列 3 缓存正确性与性能 | 8 | PR 分支已推送,**未建 PR**;建议标注 P0 |
| 系列 4 上游与协议健壮性 | 7 | #2、#6 已合入上游(#431 DoH 响应体上限、#432 压缩指针环检测);#3、#7 已单独成 PR(bootstrap load_full、probe 有界阻塞池,基线 main);其余未建 PR;#1/#2 触及建连热路径 |
| 系列 5 平台与升级 | 3 | 未建 PR;建议 Windows 环境实测 |
| 共享 lint `87cad1b2` | 1 | 未携带;随最后合并的系列或手拆 |
| semaphore-pools | 5 | 未建 PR;见 `pr-series-semaphore-pools.md`(6 提交含应急 `3f14471d`) |
| staged-reload | 5 | 未建 PR;见 `pr-series-staged-reload.md`(3 个 PR) |
| EDNS0 过滤与匹配 | 4 | 未建 PR;见 `pr-series-edns0.md`(建议 2 个 PR:纯重构 + 插件功能) |
| 其他清理(`0e43f157`、`31f20124`、`b1d81777`) | 3 | 未归组,可随 staged-reload 或弃 |
| 已撤销 disable_ecs(`fc15334e` + `3122c1af`) | 2 | net-zero,不进任何上游 PR |
| 文档/学习笔记/PR 正文等(分支元数据) | 42 | 不进上游 PR |
| 合并 `origin/main`(`488237ff`) | 1 | — |
| **合计** | **92** | 五个修复系列 29 + 共享 lint 1 + 重构系列 10 + 清理未归组/已撤销 5 + EDNS0 4 + 元数据/合并 43 |

> 设计文档本身(`design-*.md`)可随对应系列携带,也可不带。
