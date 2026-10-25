# PR 系列:评审修复(Review Fixes)

将分支 `fix/code-review-priorities` 上代码评审产生的修复提交,按子系统拆为五个可独立评审的系列。两个重构系列(staged-reload / semaphore-pools)见各自文档;本文覆盖其余全部提交。

全局依赖说明:

- `87cad1b2`(chore: satisfy rustfmt and clippy on windows nightly)横跨 api / upstream / upgrade 三个系列的文件(auth.rs 导入重排、dial.rs 死代码清理、h2/h3 BufMut、upgrade cfg 门控),**必须**在它依赖的提交之后 cherry-pick:api 系列的 `06b42ea1`、upstream 系列的 `4db4f6a0` 与 `d08d26e7`。建议放在这些系列中**最后合并的一个**里,或手工拆分。不携带它不影响 Linux CI(这些是 Windows nightly clippy 的 lint)。
- 推荐合并顺序:五个评审系列(互相独立)→ semaphore-pools(需 `3f14471d` 上下文)→ staged-reload(体量最大,评审周期最长)。
- 分支元数据提交共 4 个,不进入上游 PR:`150e615e`/`47d55ae6`(设计文档状态)与 `06f0e734`/`b6e7791e`(PR 系列拆分文档);设计文档本身(`design-*.md`)可随对应系列携带,也可不带。

---

## 系列 1:管理 API 加固(3 提交)

PR 标题建议:`fix(api): harden request body, auth, and config save`

| # | 提交 | 内容 |
|---|---|---|
| 1 | `4a47f389` | 请求体 4MiB 上限——`read_hyper_request` 原先在鉴权之前无限收集 body,未认证客户端可 OOM;返回 413 |
| 2 | `06b42ea1` | Basic auth 恒时比较(XOR 累积,消除前缀计时信号)+ scheme 大小写不敏感(RFC 7235)|
| 3 | `0d13fa54` | `PUT /config` 互斥串行化(TOCTOU)+ temp-file rename 原子写;版本号取自写入内容 |

cherry-pick 顺序即上表;三者互相独立。测试:新增恒时比较 4 项;既有 API 测试全绿。

## 系列 2:DNS 服务加固(8 提交)

PR 标题建议:`fix(server): ingress validation, framing, and drain hardening`

| # | 提交 | 内容 |
|---|---|---|
| 1 | `359b9a35` | TCP keepalive 补 `with_time`——TCP_KEEPIDLE 原为系统默认 ~2h,`idle_timeout` 形同虚设 |
| 2 | `3786db4b` | DoQ/DoH3 每流/每请求任务纳入 TaskTracker,关停排水与 UDP/TCP 对齐 |
| 3 | `be372447` | 入站 QR/opcode 预检——QR=1 伪造包不再进流水线,直接 REFUSED |
| 4 | `9c82d833` | 恶意 UDP 包逐包 warn 降为 debug(防日志洪水);socket 级错误保留 warn |
| 5 | `7316413c` | TCP 帧长 u16 截断由 debug_assert 改硬校验(防流错位) |
| 6 | `b1fc8490` | EDNS version>0 回 BADVERS(RFC 6891 6.1.3)|
| 7 | `3d00fd4f` | fallback:`threshold: 0` 尊重为立即待命(serde 默认 500,文档同步);备链闸 select 加 `biased` |
| 8 | `f9055c07` | UDP 预检前移到任务创建之前——垃圾包零任务成本(与 #3 语义配套,#3 先合)|

按表序 cherry-pick;#8 依赖 #3(同一校验语义的 task 前移)。

## 系列 3:缓存正确性与性能(5 提交)

PR 标题建议:`fix(cache): ECS scope validation, single-flight, and persistence hardening`

| # | 提交 | 内容 |
|---|---|---|
| 1 | `cc079fc7` | **P0**:insert/lazy 刷新时校验请求当前 ECS scope 与 key 一致 + 拒绝 scope>0 回显答案——修复跨客户端缓存污染;新增 `ecs_scope_mismatch` 计数与 3 项测试 |
| 2 | `53f80b16` | 采样 LRU 加旋转游标——后段分片不再免于淘汰 |
| 3 | `c3078f54` | dump 序列化移入 `spawn_blocking`;staging 文件名加 pid+序号(重载期新旧实例不交错)|
| 4 | `62594797` | lazy 无 stale 窗口语义文档化(`lazy_cache_ttl < ttl` 时无 stale 窗口;未开 short_circuit 不回写)|
| 5 | `4c4cb1dc` | 冷/过期 miss single-flight——leader/follower 去重,消除热门域名过期风暴;`enable()`+复查防唤醒丢失;`cache_single_flight_wait_total` 计数;2 项新测试 |

按表序;#5 依赖 #1 的同文件上下文。

## 系列 4:上游与协议健壮性(7 提交)

PR 标题建议:`fix(upstream): bound responses, async resolution, and unpredictable IDs`

| # | 提交 | 内容 |
|---|---|---|
| 1 | `4db4f6a0` | 系统 DNS 解析移入 `spawn_blocking`(connect_udp 改异步)——系统解析器卡顿不再占满 tokio worker;deadline 可取消 |
| 2 | `d08d26e7` | 上游 DoH/H2/H3 响应体 64KiB 硬上限(Content-Length 钳制 + 分块校验)——恶意上游不再能触发 GB 级分配 |
| 3 | `21ff3d0c` | bootstrap 查询改 `load_full`——不再跨整程持有 ArcSwap Guard |
| 4 | `fc424339` | 上游事务 ID 随机起点 + 随机奇数步长(封 Kaminsky 可预测性);配套测试更新 |
| 5 | `1f828545` | style:上游 rustfmt 十六进制字面量(#4 的格式尾随,可 squash 进 #4)|
| 6 | `202a8403` | 压缩指针环检测 O(n²)→O(1)——强制指针严格前指(RFC 1035),封死 ~1000× CPU 放大 |
| 7 | `71d699d8` | probe 阻塞操作改走 tokio 有界阻塞池(封无界线程创建)|

按表序;#4+#5 合并处理;#6 在 `crates/proto`(若上游按 crate 拆分评审可单独成 PR)。

## 系列 5:平台(Windows)与升级(3 提交)

PR 标题建议:`fix(service,upgrade): graceful SCM stop and upgrade hardening`

| # | 提交 | 内容 |
|---|---|---|
| 1 | `0145cf63` | Windows SCM Stop 优雅关停——oneshot 通知应用排水(≤20s),替代立即 exit(0);Restart 编舞不变 |
| 2 | `d10edfa7` | 服务错误路径 exit(1)——SCM OnFailure 重启策略恢复生效 |
| 3 | `2c026e91` | 升级:下载总超时 30s→300s;二进制回滚失败从 `let _ =` 吞掉改为 error 日志 |

按表序;#2 依赖 #1 的同文件上下文。

---

## 汇总

| 系列 | 提交数 | 风险 | 建议 |
|---|---|---|---|
| 1 API 加固 | 3 | 低 | 可立即发 PR |
| 2 DNS 服务加固 | 8 | 低 | 可立即发 PR |
| 3 缓存正确性与性能 | 5 | 低(P0 修复建议优先评审)| 可立即发 PR,建议标注 P0 |
| 4 上游与协议健壮性 | 7 | 低(#1/#2 触及建连热路径)| 可立即发 PR |
| 5 平台与升级 | 3 | 中(Windows 生命周期行为)| 建议 Windows 环境实测后发 |
| 共享 lint | `87cad1b2` | 无 | 随最后合并的系列携带 |
| semaphore-pools | 5 | 中 | 见 `pr-series-semaphore-pools.md` |
| staged-reload | 3 | 中 | 见 `pr-series-staged-reload.md`,建议最后合并 |

分支总提交 39 = 五个修复系列 26 + 共享 lint 1 + semaphore 5 + staged-reload 3 + 分支元数据 4。
