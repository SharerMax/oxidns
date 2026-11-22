# 设计:query_recorder 存储预算与体积优化

状态:设计定稿,未实现
需求来源:无(存储受限设备部署实践,来源为插件实现分析)
基线:main @ `ad2046d3`(v1.7.1)
范围:`src/plugin/executor/query_recorder/{mod,model,backend,store,capture,payload,api,tests,v2_tests}.rs`、`webui/lib/plugin-definitions/executor.ts`、WebUI locale、`website/docs/plugin-reference/executor/observability.mdx` 及英文 i18n
取代:无(首版)

## 0. 背景与诉求

`query_recorder` 当前唯一的存储控制是时间维的 `retention_days`(默认 7 天):
周期清理按 `created_at_ms` 分批过期。这在存储受限设备(小型路由器、嵌入式网关、
低配 VPS)上存在三个缺口:

- **没有总量上限**。记录量随 QPS 线性增长,高流量下 7 天窗口足以写满磁盘。
  上限维度缺失是硬伤,不是参数调优问题。
- **payload 是体积大头,且无策略**。每条记录的答案/权威/附加/签名节 + EDNS
  全量进入 BLOB;DNSSEC 重应答、CNAME 链单条可达几十 KB,而成功查询的详细
  RR 数据恰恰是事后诊断价值最低的部分。
- **容量状态不可见**。`dropped_total`(队列满丢包)只存在于日志;
  数据库体积、记录数、WAL 峰值没有任何 API 端点,用户只能登机看文件。

诉求:在保持"热路径零 SQLite 开销、观察者语义、默认行为不变"三个既有约束的
前提下,为存储受限设备提供**总量上限、payload 分级、行格式瘦身**三层控制,
并让存储状态可观测。

## 1. 核心决策

| 决策点 | 结论 | 理由 |
|---|---|---|
| 上限形态 | `max_records` + `max_database_bytes` 双维度,均可选 | 条数约束行膨胀;字节约束含 WAL/字典/页开销的真实文件占用;两者可并存 |
| 上限执行点 | 周期清理任务内(writer 线程),不在 flush 路径 | 完整复用既有"分批删除 + WAL 观察 + checkpoint + incremental_vacuum"管线;热路径与写入循环保持零维护成本 |
| 上限语义 | 软上限:超限驱逐发生在下一个清理周期,窗口内允许短暂超限 | 每次 flush 后检查/回收会把 vacuum 成本放进写路径,违背热路径原则;`cleanup_interval_hours` 即上限延迟上界,文档明示 |
| record_mode 默认 | `full`(现状) | 默认行为逐字节不变;`errors_only` 作为存储受限设备推荐档 |
| 模式实现点 | 捕获决策在热路径(`execute_with_next`),裁剪执行在 writer | `counts_only`/`errors_only` 可在捕获处避免整份 `Message` 克隆,同时省 CPU 与队列内存 |
| payload 截断粒度 | 整节丢弃(签名 → 附加 → 权威 → 答案),不拆单条 RR | 半条 RR 无诊断价值;`answer_count` 等标量列保持真实,截断可检测 |
| schema 策略 | 阶段一/二零迁移(v2 revision 不动);行打包另起 v3 表族 | 遥测数据不值得重迁移;v2/v1"永不迁移"哲学延续 |
| 可观测性 | 新增 `GET /stats/space` + writer 侧计数器 | 容量上限、截断、丢包必须可见;`dropped_total` 目前只能翻日志 |
| WAL 绑定 | `PRAGMA journal_size_limit = 16 MiB`(常量) | 常态 auto-checkpoint(1000 页 ≈ 4 MiB)已足够;限值只封堵 reader 阻塞 checkpoint 时的异常增长 |

**被拒方案**

- *zstd 替代 zlib*:收益集中在"已决定保留"的 payload 上,而阶段二直接整节
  裁剪,压缩率优化被裁剪覆盖;新增依赖 + codec 分支不值。
- *按天分库 / 轮转文件*:跨天查询与统计断裂,文件管理复杂化,与单文件
  SQLite 契约冲突。
- *每次 flush 后做上限检查*:同"上限执行点"决策,回收成本进写路径。
- *仅建议用户调低 `retention_days`*:时间窗无法约束高 QPS 下的总量,
  缺的是维度而不是参数。
- *records 表加 `payload_truncated` 布尔列*:可由"标量计数与 JSON 节长度
  不一致"无歧义推出,省一次 schema revision 变更。

## 2. 配置契约

```yaml
- tag: recorder_main
  type: query_recorder
  args:
    path: ./data/queries.sqlite
    # ---- 存储预算(全部可选,缺省 = 现状) ----
    record_mode: errors_only      # full(默认) | errors_only | counts_only
    max_payload_bytes: 4096       # 单条响应快照预算;缺省 = 不限制
    max_records: 200000           # records 行数上限;缺省 = 不限制
    max_database_bytes: 67108864  # 数据库文件+WAL 总量上限(64 MiB);缺省 = 不限制
```

- `record_mode`:
  - `full`:现状,全量快照。
  - `errors_only`:请求出错(`error` 非空)或响应 rcode 为 SERVFAIL 时保留完整
    快照;其余记录只保留标量列(rcode/flags/三节计数/耗时),RR 节与响应 EDNS
    为空。执行路径(traces)与问题字典不受模式影响,照常采集。
  - `counts_only`:所有记录都只保留标量列。统计端点不受影响(全部基于标量列
    与字典)。
- `max_payload_bytes`:仅约束 payload 快照(§3.3);`counts_only` 下无意义
  但允许配置。> 0。
- `max_records`:> 0。
- `max_database_bytes`:> 0 且 ≥ 1 MiB,低于下限启动失败(避免无意义配置)。
- 全部新字段缺省时,行为与现状完全一致;`quick_setup` 不提供这些字段。
- 推荐组合(文档给出,非内置默认):小型设备
  `record_mode: errors_only` + `max_database_bytes`(按磁盘余量)。

## 3. 后端设计

### 3.1 配置与 limits 结构(`mod.rs` / `model.rs`)

- `QueryRecorderConfig` 新增 `record_mode: Option<String>`、
  `max_payload_bytes: Option<usize>`、`max_records: Option<u64>`、
  `max_database_bytes: Option<u64>`;`resolve_config` 落成
  `ResolvedRecorderConfig` 新字段:
  - `record_mode: RecordMode`(enum,`FromStr` 解析,非法值启动失败)
  - `payload_budget: Option<usize>`
  - `limits: RecordLimits { max_records: Option<u64>, max_database_bytes: Option<u64> }`
- `RecordLimits` 克隆进 `RecorderBackend` 与 `WriterThreadContext`,周期清理
  任务与 writer 主循环都能读到。
- backend 新增 writer 侧原子计数:`payload_truncated_total`、
  `summary_only_total`(`dropped_total` 已存在,一并暴露)。

### 3.2 记录模式分级(`mod.rs` 热路径 + `capture.rs`)

`PendingRecord.response` 类型改为:

```rust
enum CapturedResponse {
    Full(Message),
    Summary(ResponseSummary),
}

struct ResponseSummary {
    present: bool,          // 响应是否存在(no_err 门控仍由 take_to_record 应用)
    rcode: Option<Rcode>,
    aa: bool, tc: bool, ra: bool, ad: bool, cd: bool,
    answers: u32, authorities: u32, additionals: u32,
}
```

`execute_with_next` 捕获决策(在 `continue_next!` 返回后,与现在的响应克隆
同位置):

```text
full        => CapturedResponse::Full(context.response.clone())
errors_only => 出错(result.is_err())或 response.rcode() == SERVFAIL
               => Full(context.response.clone())
               否则 => Summary(extract(context.response.as_ref()))
counts_only => Summary(extract(...))
```

- `extract` 只做字段读取(rcode/flags/`answers().len()` 等),无每 RR 工作、
  无额外分配;`full` 模式路径与现状完全一致(一次 `Option` 匹配)。
- `take_to_record`(`capture.rs`)按 `CapturedResponse` 分支:`Summary` 时
  跳过全部 `record_json`/`edns_json` 转换,直接填标量列;`no_err` 门控逻辑
  不变(有 error 时响应各节一律为空)。
- 模式信息同时用于 writer 侧 `summary_only_total` 计数。

### 3.3 payload 预算截断(`capture.rs` + `payload.rs`)

- 执行点:`take_to_record` 组装完 `RecordRow` 各节之后、`PreparedRecord`
  编码之前——DB、内存尾缓冲、SSE 天然一致。
- 算法:整节为粒度,丢弃优先级 `signature → additionals → authorities →
  answers`(最后才动 answers;全部 RR 节清空后仍超预算才丢 `resp_edns_json`;
  `req_edns_json` 永不丢)。按节序列化尺寸估算,不拆单条 RR。
- 标量列(`answer_count`/`authority_count`/`additional_count`)在截断前
  定格,**保持真实**。
- 截断检测契约:任一 RR 节 `json.len() < 对应 count` 即视为截断
  (`capture.rs` 提供 `fn payload_truncated(&RecordRow) -> bool`);命中时
  `payload_truncated_total += 1`。
- 编码阶段不变:截断后尺寸变小,zlib 阈值(2 KiB / 80% 收益)自然生效。

### 3.4 记录上限与尺寸驱逐(`backend.rs` / `store.rs`)

周期清理任务路径不变(`cutoff = now - retention_days`),`run_cleanup`
在既有保留循环之后追加两个阶段,**最后统一走一次 `reclaim_database_space`**:

```text
阶段 1  保留过期:delete_batch(Some(cutoff)) 循环(现状)
阶段 2  条数上限:while count > max_records { delete_batch(None) }
        (delete_batch 按 created_at_ms,id 升序选批,天然驱逐最老记录;
         count 一次性 SELECT COUNT(*) 后逐批递减)
阶段 3  字节上限:while total_bytes > max_database_bytes {
            delete_batch(None) → run_incremental_vacuum → 重新测量
         }
        (删除本身不缩文件,必须逐轮 vacuum 才能观察真实进度;
         删除 0 条或 vacuum 无进展时终止)
```

- **上限优先级:字节 > 条数 > 时间**。尺寸驱逐不看年龄——这正是容量上限的
  语义(与 retention 并存:先过期,再补删)。
- 阶段 3 终止后若仍超限(全部记录已删、字典已回收,文件被页开销/字典残量
  托住),记 `warn!` 并正常返回,不视为错误。
- `CleanupResult` 新增 `deleted_by_retention` 与 `deleted_by_size`,
  `log_space_reclaim` 日志字段同步拆分。
- 阶段一/三共用的 WAL 观察 + `checkpoint(TRUNCATE)` 节奏保持现状;阶段三的
  逐轮 incremental_vacuum 使最终 reclaim 大部分为 no-op,`freelist == 0` 等
  进度校验不变。

### 3.5 WAL 绑定与 `/stats/space`(`store.rs` / `api.rs`)

- `open_writer_database` / `open_reader_database` 增加常量
  `PRAGMA journal_size_limit=16777216`(16 MiB),注释说明:常态
  auto-checkpoint 已把 WAL 控制在 ~4 MiB,限值仅封堵"活跃 reader 长时间
  阻塞 checkpoint"时的无界增长;对 cleanup 的 `checkpoint(TRUNCATE)` 无影响。
- 新增 `GET /stats/space`(读路径,走 `run_reader_query` 信号量 + 只读连接):

```json
{
  "ok": true,
  "record_count": 123456,
  "oldest_record_ms": 1735000000000,
  "database_bytes": 48234496,
  "wal_bytes": 1048576,
  "limits": { "max_records": null, "max_database_bytes": 67108864 },
  "counters": {
    "dropped_total": 0,
    "payload_truncated_total": 42,
    "summary_only_total": 118000
  }
}
```

  `record_count` 为 `SELECT COUNT(*)`(一次性运维端点,不走统计采样);
  `oldest_record_ms` 走 `MIN(created_at_ms)`(已有 created_at 索引)。

### 3.6 行格式打包(v3 表族,可选阶段)

**触发条件:阶段一/二上线后,在目标设备实测体积仍不达标才实施。**
payload 是体积大头,行打包只作用于标量行(约 100-200 字节/行),预估节省
30-60 字节/行,优先级最低。

- 表族前缀升为 `qr_{tag}_{hash}_v3_`,meta 内 `schema_revision = '1'`
  (版本由表族名携带,与 v1/v2 隔离策略一致);v3 创建成功时**丢弃 v2 表族**
  并在日志中输出被丢弃的 records 计数。
- 内容:
  - 8 个布尔列(`req_rd/cd/ad` + `resp_aa/tc/ra/ad/cd`)合并为
    `flags INTEGER` 位掩码(装配时还原);
  - `client_ip` 改 INTEGER(IPv4 u32)/ 16 字节 BLOB(IPv6),装配时格式化
    回字符串;
  - `question_sets.questions_json` 移除,装配改由 `question_items` 重建
    (items 增加 `qclass` 列,约 3 字节/问题);
  - records 其余列、traces/trace_steps、索引集不变。
- **升级丢历史是接受的代价**:recorder 是带 retention 语义的遥测数据,
  为它做重迁移违背"存储受限"的初衷;v2 表若保留则在新表族下永远不可读,
  变成纯死重。Release note 显式 call out;回滚到旧二进制会重建空的 v2
  表族,服务正常。

### 3.7 热路径

- `full` 模式(默认):热路径新增一次 `RecordMode` 匹配,无分配,无 IO。
- `errors_only`/`counts_only`:热路径**减少**工作——非保留记录跳过整份
  `Message` 克隆,替换为若干字段读取。
- payload 截断、尺寸驱逐、计数器全部在 writer 线程;`/stats/space` 走既有
  读信号量路径。

## 4. 行为矩阵

| 场景 | 结果 |
|---|---|
| 未配置任何新字段 | 与现状逐字节一致 |
| `counts_only` + 统计端点 | 全部统计(top/分布/延迟/时间序列/插件统计)结果不变 |
| `counts_only` + `/records` 列表 | 记录存在,RR 节为空,rcode/flags/counts/耗时真实 |
| `errors_only` + 成功查询 | 同 `counts_only` 展示 |
| `errors_only` + SERVFAIL 或请求错误 | 完整快照(与 `full` 相同) |
| `max_payload_bytes` + 巨型 DNSSEC 应答 | 预算内保留 answers,其余节整节置空,counts 真实,`payload_truncated_total` 递增 |
| `max_records` 超限 + 清理周期到 | 最老记录逐批驱逐至条数上限 |
| `max_database_bytes` 超限 + 清理周期到 | 删除 + 逐轮 incremental_vacuum 直到 ≤ 上限或无可删 |
| retention 与 caps 同时配置 | 先按时间过期,再补删条数,再补删字节 |
| 全部记录删尽仍超字节上限 | `warn!` 日志,返回成功(页开销托底) |
| 阶段三后回滚旧二进制 | v2 表族被重建为空,服务正常,v2 历史不可恢复(已文档化) |

## 5. 测试计划

配置(`mod.rs`):

- `record_mode` 三值解析与非法值报错;`max_payload_bytes`/`max_records`
  零值拒绝;`max_database_bytes` 零值与 < 1 MiB 拒绝;全部缺省 = 现状。

模式分级(`tests.rs`):

- `counts_only`:入队成功查询 → flush 后 payload 全空、标量列与统计端点
  结果与 `full` 完全一致;`summary_only_total` 正确;
- `errors_only`:SERVFAIL 保留快照、NOERROR 落为摘要、请求错误保留快照
  (error 存在时响应节为空的既有门控不变);
- `full`:回归——入队内容与现状基线逐字段一致。

payload 截断(`tests.rs` / `payload.rs`):

- 构造多节大应答:预算砍掉 signature/additionals 而保留 answers;
- 预算小于单节:全部 RR 节为空、resp_edns 保留与否符合丢弃顺序;
- counts 与节长度不一致检测命中 `payload_truncated_total`;
- 小应答不截断;`counts_only` 下预算不生效。

上限与驱逐(`tests.rs` / `v2_tests.rs`):

- `max_records`:批量入队超过上限 → 直接调 `backend.cleanup(cutoff)` 后
  records 计数 ≤ 上限,且最老记录先被驱逐(created_at 断言);
- `max_database_bytes`:种子大体积记录 → cleanup 后
  `database_bytes + wal_bytes ≤ 上限`,freelist == 0;
- 同时配置三者:过期 → 条数 → 字节顺序生效,`deleted_by_retention` /
  `deleted_by_size` 拆分正确;
- 无可删仍超限:返回成功 + warn 日志,writer 不中断(沿用清理失败不影响
  writer 的既有测试形态);
- 清理期间队列满时上限检查不被跳过(沿用既有 Cleanup 命令排队测试形态)。

WAL 与新端点(`store.rs` / `api.rs`):

- 两个 PRAGMA 断言扩展覆盖 `journal_size_limit`;
- `/stats/space`:record_count/oldest/bytes 与手工 SQL 一致,limits 原样
  回显,非法参数(无)与读信号量路径复用既有测试形态。

阶段三(若实施,`v2_tests.rs`):

- v3 表族创建成功 → v2 表族被 DROP 且日志计数正确;
- flags 位掩码往返、IPv4/IPv6 存储与格式化还原、
  question_items 重建 questions(含 qclass)与旧装配逐字段一致;
- v2 代码遇到 v3 表族(或反之)的行为与现有 revision 校验语义一致。

## 6. 文档与 WebUI 同步

- `website/docs/plugin-reference/executor/observability.mdx` 与英文 i18n:
  新增"存储预算"小节(四个新字段、软上限语义、模式对照表、推荐组合);
  `/stats/space` 端点加入端点列表。
- WebUI `webui/lib/plugin-definitions/executor.ts` 的 `query_recorder`
  configSchema:`record_mode`(select,默认 `full`)、`max_payload_bytes`、
  `max_records`、`max_database_bytes`(number,`advanced: true`);
  对应 `webui/lib/i18n/locales/{zh-CN,en-US}` 文案;记录详情视图对
  "标量计数 > RR 节长度"显示"快照被预算截断"提示,对 `errors_only`/
  `counts_only` 的空节显示模式提示而非报错。
- `learning/plugin/executor/oxidns-plugin-query-recorder.md`:§11 配置表
  追加新字段,§8 流程图补尺寸驱逐分支。
- `README*.md`、`config*.yaml` 不改(默认行为与能力摘要未变,新字段全部
  opt-in)。
- Release note call out:opt-in 新字段;`errors_only`/`counts_only` 下
  记录详情无 RR 数据属预期;阶段三(若实施)升级丢弃 v2 历史并不可恢复。

## 7. 风险与边界

- **软上限窗口**:上限在清理周期生效,`cleanup_interval_hours` 内允许
  超限;最坏超限增量 ≈ 一个周期的写入量。文档明示"软上限",用户可缩短
  周期收紧。
- **字节上限含非记录开销**:WAL、页开销、字典残量计入
  `max_database_bytes`;上限应理解为"文件占用上限"而非"数据量上限"。
- **`errors_only` 的语义边界**:仅 error 与 SERVFAIL 视为"值得保留";
  REFUSED 等异常 rcode 落为摘要。与既有"error 门控"一致,不做配置化扩展。
- **截断检测契约**:依赖"标量计数与 JSON 节长度不一致";WebUI 与文档
  按此契约展示,不新增列。
- **COUNT(*) 成本**:`/stats/space` 每次调用全表计数;运维端点低频调用,
  可接受,不做缓存。
- **阶段三升级丢历史**:明确的取舍而非遗漏——遥测数据 + retention 语义
  下,重迁移与死重都比丢弃更差;回滚路径(重建空 v2)可接受。
- **默认行为零变化**:全部新字段缺省关闭,既有部署不受影响;`full` 模式
  热路径仅增加一次枚举匹配。

## 8. 实施拆分

单分支系列 PR,建议顺序:

1. `feat(query-recorder): add record_mode capture modes`(配置 + `CapturedResponse` 捕获 + `take_to_record` 分支)
2. `feat(query-recorder): cap response payload with max_payload_bytes`(整节截断 + `payload_truncated_total`)
3. `feat(query-recorder): enforce record limits with size-driven eviction`(`RecordLimits` + 三阶段清理 + `CleanupResult` 拆分)
4. `feat(query-recorder): bound WAL size and expose /stats/space`(`journal_size_limit` + 端点 + 计数器暴露)
5. `perf(query-recorder): pack record rows in v3 table family`(**可选**,按阶段一/二实测收益决定)
6. `docs(query-recorder): document storage budget`(文档站、WebUI 定义、locale、learning 文档)

阶段一~四为零 schema 迁移,可独立发布;阶段五单独评审。

## 9. 回退

- 每个能力由配置缺失关闭;删除配置即回到现状,无需数据迁移;
- 阶段一~四不触碰 v2 schema(`schema_revision = '1'` 校验不变),
  回退无持久化格式变化;
- 阶段五回滚:旧二进制重建空 v2 表族,服务正常;v2 历史(升级时已丢弃)
  本就不存在,无额外损失;
- `/stats/space` 与计数器为纯增量端点,移除无副作用。
