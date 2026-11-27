# PR:fix(server): echo the request's question case in responses

**目标分支**:svenshi/oxidns `v1.8.0`
**来源分支**:SharerMax/oxidns `fix/response-question-case-echo`(3 个提交,基于 `origin/v1.8.0` @ `31a011af`)
**创建命令**:

```bash
gh pr create \
  --repo svenshi/oxidns \
  --head SharerMax:fix/response-question-case-echo \
  --base v1.8.0 \
  --title "fix(server): echo the request's question case in responses" \
  --body-file pr-doc/pr-body/pr-body-response-question-case-echo.md
```

---

## 摘要

forward 与缓存路径不保证响应 question 区回显客户端原始大小写,违反 RFC 1035 §4.1.2:

- 缓存 key 使用小写化 qname(`src/plugin/executor/cache/key.rs:114-128`),混合大小写查询共用条目,命中时克隆入库时的 `Message`(`src/plugin/executor/cache/mod.rs:816`);
- `forward` 单上游直接 `set_response` 上游响应(`src/plugin/executor/forward/single.rs:62`);
- 上游探测用 `eq_ignore_ascii_case` 校验 question(`src/infra/network/upstream/probe.rs:1182`),`classify_response` 的 `Question` 比较也经大小写不敏感的 `Name`,因此小写回显能通过全部校验。

客户端发送 `WWW.Example.COM.` 却收到 `www.example.com.`,按文本比较 question 的客户端、诊断工具以及 0x20 风格客户端都会看到不一致。

本 PR 在 server 出站统一收口修复:

1. **入口快照隔离污染**——`DnsContext` 新增 `OriginalQuery` 定向快照,`finalize_response` 与 `build_base_response` 均以快照为准,不再读可能被下游改写/替换的 `context.request`(`replace_request` 是公开插件 API;`dual_selector` 的 preferred 探测改写 qtype 后经 `apply_subquery_result` 回流父 context,是树内真实污染源)。单问题快照零堆分配(`Question::clone` 仅 `Name` 的 `Arc` 自增)。
2. **回显请求大小写**——`RequestHandle::finalize_response` 调用 `restore_question_echo`,以快照 question 为源,按索引把响应 question 的 `Name` 替换为请求的 `Name` 句柄(`Arc` 克隆,无字节拷贝/分配),仅当 name 大小写等价且 qtype/qclass 相同时替换。
3. **重建缺失 question**——响应 question 数量与快照不一致时(上游省略 question 等),用快照 question 区重建。
4. **覆盖所有出站路径**——UDP/TCP/DoT/DoH/DoQ 共享 `handle_request`;forward 直连、缓存命中、插件合成响应与错误响应统一生效。

## 合入基线

分支已从原先基于 `origin/main` + v1.8.0 merge 的 4 提交版本**强制更新**为以 `origin/v1.8.0`(`31a011af`)为基准的版本(由原实现 `31240490` cherry-pick 得到)。先前版本中的 v1.8.0 合并提交、fmt 注释修正与 website `edns0opt` 目录补丁均已移出:其中 website 补丁已拆为独立 PR `fix/website-edns0opt-overview`。

在此单提交版本之上追加两个提交:`4015ef2f` 引入 ingress 快照,使回显/EDNS DO 位/基础响应免疫下游请求污染(响应"下游插件可能污染 request"的设计评审);`2e399646` 将快照改为定向捕获,消除 hotpath 基准门禁检出的整 `Message` 克隆分配回归。

## 提交

| 提交(本分支) | 内容 |
|---|---|
| `9e009eee` | fix(server): echo the request's question case in responses |
| `4015ef2f` | fix(server): restore responses from an ingress request snapshot |
| `2e399646` | perf(server): capture the ingress snapshot without heap allocation |

## 变更明细

- `src/core/context.rs`
  - 新增 `OriginalQuery`/`OriginalQuestions` 定向快照与 `DnsContext::original_query()`:入站捕获 id/opcode/RD/CD/EDNS DO 位与问题段;单问题走内联 `Single` 变体零分配,QDCOUNT>1 才退化为 `Vec`;
  - `copy_for_subquery` 克隆快照;`apply_subquery_result` 刻意不回写快照(附注释),保证客户端入口状态跨流水线不变。
- `src/plugin/server/request.rs`
  - `restore_question_echo` 消费快照 question 切片(`&[Question]`);数量不一致时 `clear` + `extend` 快照 question;数量一致时逐条比较 name/qtype/qclass 并替换 `Name` 句柄;语义不同的 question(非大小写差异)保持不变,交由既有上游校验与 `classify_response` 处理;
  - `finalize_response` 的 EDNS DO 位复制与 question 回显均以快照为准;
  - `build_base_response`(ServFail/空响应/ingress 拒绝)从快照原语重建 id/opcode/RD/CD/问题段——原实现基于可能被污染的 live request;
  - 测试 fixture 合并为单个 `RequestPollutingExecutor`(同时覆盖下游请求改写与上游小写回显两种场景)。
- `src/plugin/test_utils.rs`:新增共享测试助手 `encoded_question_fqdn`,从编码后的 wire 字节取回 question 原始大小写(`Name::to_fqdn`/`Display` 会折叠为小写,不能直接断言)。
- `crates/proto/src/name.rs`:新增 `Name` 大小写不敏感相等性与 `HashSet` 查找测试。
- `src/plugin/executor/cache/mod.rs`:新增缓存命中回显测试。

## 影响面

- **配置与 WebUI**:无。无 `args`、无 schema、无插件定义、无 i18n、无 metrics;本 PR 不涉及 `website/**`。
- **协议语义**:响应 question 区严格回显本次请求;DNSSEC question 不参与签名,AD 位与校验结果不变;编码与压缩在 finalize 之后重建,无悬空指针。
- **缓存**:条目内容、TTL、负缓存与懒刷新语义不变;同一 key 的不同大小写查询各自回显本次拼写。
- **热路径**:每请求一次定向快照捕获:6 个标量字段读取 + 一次 `Arc` 自增(单问题零堆分配;初版整 `Message` 克隆曾被 hotpath 门禁检出 `plugin_hosts/execute/large_domain` +11.66% instructions/op,`2e399646` 已消除,本地 criterion 交替复测回落到基准水平:725 ns vs base 721 ns,+0.6%);每响应一次回显为 O(question 数)(实践即 1):三次比较 + 一次 `Arc` 自增,无堆分配、无锁、无 IO;仅数量不一致的异常响应与 QDCOUNT>1 才分配。
- **0x20**:本 PR 只回显请求已有大小写,不生成随机大小写、也不校验上游回显大小写,因此不提供 0x20 防伪能力。

## 测试

| 命令 | 结果 |
|------|------|
| `cargo test --lib` | 1308 passed / 0 failed / 4 ignored(`2e399646` 上复跑;新增 1 用例、合并 1 用例,总数与 `9e009eee` 持平) |
| `cargo fmt --check`(stable) | 通过 |
| `cargo clippy --lib -- -D warnings`(stable) | 通过 |
| `cargo +nightly clippy --all-targets --all-features -- -D warnings` | `9e009eee` 时通过;新增提交后待复验 |
| plugin_integration | 沙箱环境下全量并行在 provider/init 类测试挂死:对照 `9e009eee`(无本 PR 改动)复现同一现象且隔离运行全部通过,判定为基线/环境问题而非本 PR 引入,待 CI 与本机复验 |

新增用例:

| 用例 | 位置 |
|---|---|
| `name_equality_and_hash_ignore_ascii_case` | `crates/proto/src/name.rs` |
| `finalize_response_rebuilds_missing_question_section` | `src/plugin/server/request.rs` |
| `test_handle_request_echoes_case_after_downstream_request_rewrite` | `src/plugin/server/request.rs`(替代 `9e009eee` 的 `test_handle_request_restores_request_question_case_from_upstream`,fixture 合并为 `RequestPollutingExecutor`,同时覆盖下游请求改写与上游小写回显) |
| `cache_hit_restores_current_request_question_case` | `src/plugin/executor/cache/mod.rs` |

> 门禁说明:本 PR 不涉及 `website/**`,`docs-ci` 的 `npm run build` 不会触发;v1.8.0 的 `edns0opt` overview 缺口由独立 PR `fix/website-edns0opt-overview` 处理。本机 nightly 的 `fmt --check` 在 v1.8.0 基线文件 `pool/reuse.rs` 与 `matcher/edns0opt.rs` 上有注释换行差异,本 PR 未改动这两个文件;`rust-ci` 的 `pull_request` 仅覆盖 `main` 与 `release/*`,对 `v1.8.0` 不触发。`cargo::unused_dependencies` 对 `tar` 的 warning 亦为既有问题。hotpath 基准门禁(`hotpath-comment.yml`)在 `4015ef2f`(整 `Message` 克隆)上检出 hosts/redirect 四项 instructions/op 回归(+5.9%~+11.66%、large_domain B/op +16 字节),`2e399646` 的零分配捕获已消除;Instructions/op 计数以 CI 门禁在该提交上的复测为准。

## 关联

- 设计文档:`pr-doc/design/design-response-question-case-echo.md`(`learning-doc` 分支),含决策表、行为矩阵、被拒方案与后续 0x20 扩展。
- 未新增/修改 `config*.yaml`、WebUI、website 文档与 `README*.md`:question 回显属协议正确性,不是可配置行为。
