# PR:fix(server): echo the request's question case in responses

> **状态:别名分支,已由 `fix/response-question-case-echo` 取代。** 该分支内容已强制推送到原分支名,请以 `pr-doc/pr-body/pr-body-response-question-case-echo.md` 为准开 PR。

**目标分支**:svenshi/oxidns `v1.8.0`
**来源分支**:SharerMax/oxidns `fix/response-question-case-echo-v180`(1 个提交 `9e009eee`,基于 `origin/v1.8.0` @ `31a011af`)
**创建命令**:

```bash
gh pr create \
  --repo svenshi/oxidns \
  --head SharerMax:fix/response-question-case-echo-v180 \
  --base v1.8.0 \
  --title "fix(server): echo the request's question case in responses" \
  --body-file pr-doc/pr-body/pr-body-response-question-case-echo-v180.md
```

---

## 摘要

forward 与缓存路径不保证响应 question 区回显客户端原始大小写,违反 RFC 1035 §4.1.2:

- 缓存 key 使用小写化 qname(`src/plugin/executor/cache/key.rs:114-128`),混合大小写查询共用条目,命中时克隆入库时的 `Message`(`src/plugin/executor/cache/mod.rs:816`);
- `forward` 单上游直接 `set_response` 上游响应(`src/plugin/executor/forward/single.rs:62`);
- 上游探测用 `eq_ignore_ascii_case` 校验 question(`src/infra/network/upstream/probe.rs:1182`),`classify_response` 的 `Question` 比较也经大小写不敏感的 `Name`,因此小写回显能通过全部校验。

客户端发送 `WWW.Example.COM.` 却收到 `www.example.com.`,按文本比较 question 的客户端、诊断工具以及 0x20 风格客户端都会看到不一致。

本 PR 在 server 出站统一收口修复:

1. **回显请求大小写**——`RequestHandle::finalize_response` 调用新增的 `restore_question_echo`,按索引把响应 question 的 `Name` 替换为请求的 `Name` 句柄(`Arc` 克隆,无字节拷贝/分配),仅当 name 大小写等价且 qtype/qclass 相同时替换。
2. **重建缺失 question**——响应 question 数量与请求不一致时(上游省略 question 等),用请求的 question 区重建。
3. **覆盖所有出站路径**——UDP/TCP/DoT/DoH/DoQ 共享 `handle_request`;forward 直连、缓存命中、插件合成响应与错误响应统一生效。

## 合入基线

分支从 `origin/v1.8.0`(`31a011af`)直接切出,只 cherry-pick 修复提交 `9e009eee`,不含 merge 提交。先前基于 `origin/main` 的分支 `fix/response-question-case-echo` 含 v1.8.0 合并提交与两个基线补丁(fmt 注释换行、website `edns0opt` 目录),已由本分支取代:那些是 v1.8.0 基线问题,与本修复无关。

## 提交

| 提交(本分支) | 内容 |
|---|---|
| `9e009eee` | fix(server): echo the request's question case in responses |

## 变更明细

- `src/plugin/server/request.rs`
  - `finalize_response` 末尾调用 `restore_question_echo`;
  - 数量不一致时 `clear` + `extend` 请求 question;数量一致时逐条比较 name/qtype/qclass 并替换 `Name` 句柄;
  - 语义不同的 question(非大小写差异)保持不变,交由既有上游校验与 `classify_response` 处理。
- `src/plugin/test_utils.rs`:新增共享测试助手 `encoded_question_fqdn`,从编码后的 wire 字节取回 question 原始大小写(`Name::to_fqdn`/`Display` 会折叠为小写,不能直接断言)。
- `crates/proto/src/name.rs`:新增 `Name` 大小写不敏感相等性与 `HashSet` 查找测试。
- `src/plugin/executor/cache/mod.rs`:新增缓存命中回显测试。

## 影响面

- **配置与 WebUI**:无。无 `args`、无 schema、无插件定义、无 i18n、无 metrics;本 PR 不涉及 `website/**`。
- **协议语义**:响应 question 区严格回显本次请求;DNSSEC question 不参与签名,AD 位与校验结果不变;编码与压缩在 finalize 之后重建,无悬空指针。
- **缓存**:条目内容、TTL、负缓存与懒刷新语义不变;同一 key 的不同大小写查询各自回显本次拼写。
- **热路径**:每响应一次为 O(question 数)(实践即 1):三次比较 + 一次 `Arc` 引用计数自增,无堆分配、无锁、无 IO;仅数量不一致的异常响应才分配。
- **0x20**:本 PR 只回显请求已有大小写,不生成随机大小写、也不校验上游回显大小写,因此不提供 0x20 防伪能力。

## 测试

| 命令 | 结果 |
|------|------|
| `cargo test` | 1429 passed / 0 failed / 4 ignored(lib 1308、feature_gating 5、plugin_integration 116) |
| `cargo +nightly clippy --all-targets --all-features -- -D warnings` | 通过(v1.8.0 已含上游 #450 对 `dial.rs` 的修复) |

新增用例:

| 用例 | 位置 |
|---|---|
| `name_equality_and_hash_ignore_ascii_case` | `crates/proto/src/name.rs` |
| `test_handle_request_restores_request_question_case_from_upstream` | `src/plugin/server/request.rs` |
| `finalize_response_rebuilds_missing_question_section` | `src/plugin/server/request.rs` |
| `cache_hit_restores_current_request_question_case` | `src/plugin/executor/cache/mod.rs` |

> 门禁说明:本 PR 不涉及 `website/**`,`docs-ci` 的 `npm run build` 不会触发;v1.8.0 的 `edns0opt` overview 缺口由独立 PR `fix/website-edns0opt-overview` 处理。本机 nightly 的 `fmt --check` 在 v1.8.0 基线文件 `pool/reuse.rs` 与 `matcher/edns0opt.rs` 上有注释换行差异,本 PR 未改动这两个文件,也未把基线 fmt 修正带入 diff;`rust-ci` 的 `pull_request` 仅覆盖 `main` 与 `release/*`,对 `v1.8.0` 不触发。`cargo::unused_dependencies` 对 `tar` 的 warning 亦为既有问题。

## 关联

- 设计文档:`pr-doc/design/design-response-question-case-echo.md`(`learning-doc` 分支),含决策表、行为矩阵、被拒方案与后续 0x20 扩展。
- 未新增/修改 `config*.yaml`、WebUI、website 文档与 `README*.md`:question 回显属协议正确性,不是可配置行为。
