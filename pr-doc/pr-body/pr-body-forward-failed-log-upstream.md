# PR:feat(forward): include upstream tag and addr in query failed logs

**目标分支**:svenshi/oxidns `main`
**来源分支**:SharerMax/oxidns `feat/forward-failed-log-upstream`(2 个提交,基于 `origin/main` @ `ad2046d3`)
**创建命令**:

```bash
gh pr create \
  --repo svenshi/oxidns \
  --head SharerMax:feat/forward-failed-log-upstream \
  --base main \
  --title "feat(forward): include upstream tag and addr in query failed logs" \
  --body-file pr-doc/pr-body/pr-body-forward-failed-log-upstream.md
```

---

## 摘要

forward 插件的 `DNS query failed` 失败日志增加上游标识 `upstream: <tag>(<addr>)`,便于在多上游场景下直接定位故障源:

1. **标识规则**——上游配置了 `tag` 时显示 `tag(addr)`,未配置时仅显示 `addr`。`addr` 为配置原文(`ConnectionInfo.raw_addr`,即 YAML `addr` 字段原样内容),协议前缀原样展示,如 `upstream: google(udp://1.1.1.1:53)` / `upstream: udp://1.1.1.1:53`。
2. **覆盖两条失败路径**——单上游(`SingleDnsForwarder`)在原日志消息中前插 `upstream` 字段;并发多上游(`ConcurrentForwarder`)的失败日志从 selection 聚合处移至 spawn 任务内失败发生处(该处持有 upstream,可直接读取 tag),每次 attempt 失败仍只 warn 一次。`SelectionState::record_error` 保留超时分类与 `last_error` 记录职责,仅去掉重复打日志。
3. **新增模块级助手 `forward::upstream_log_name`** 统一两处展示格式(仅失败路径调用,非热路径),并配套单元测试验证 tag/addr 组合与回退行为。

## 合入基线

分支从 `origin/main`(`ad2046d3`)直接切出,不含 merge 提交;相对目标分支的 diff 为下列 2 个提交。

## 提交

| 提交(本分支) | 内容 |
|---|---|
| `5158fb86` | style(network): satisfy clippy needless_bool in is_connect_in_progress |
| `8ae266bb` | feat(forward): include upstream tag and addr in query failed logs |

> 第 1 个提交是 main 上既有 clippy `needless_bool` 告警的修复(`src/infra/network/dial.rs` 的 `is_connect_in_progress`,与 dev 分支 `c76e129b` 为同一处修改),为本机 `--all-features` clippy gate 通过所需;该文件与本 PR 功能无关,如不需要可单独还原。

## 影响面

- **配置与 WebUI**:无。纯日志改动,不涉及配置 schema、插件定义、i18n 与 API。
- **热路径**:成功路径零改动;日志仅在查询失败分支触发。`upstream_log_name` 对 `ConnectionInfo` 做一次 `Option` 判别与字符串拼接(仅失败时一次小分配),不触碰请求执行管线,无新增锁/clone/网络操作。
- **运行时行为**:`DNS query failed` 日志前缀保留,新增 `upstream` 字段;并发路径的失败日志时间点从"join 汇聚时"提前到"失败发生时",日志 target 由 `forward::selection` 变为 `forward::concurrent`。依赖解析该日志原文的自动化工具需同步。
- **文档**:日志格式非配置契约,README/WebUI/插件参考无需更新。

## 测试与校验

在 `feat/forward-failed-log-upstream`(`8ae266bb`)上执行:

| 命令 | 结果 |
|------|------|
| `cargo test`(lib + integration) | **1411 passed / 0 failed / 4 ignored**(oxidns lib 1291 含新增 `upstream_log_name_combines_tag_and_addr`、feature_gating 5、integration 115) |
| `cargo +nightly fmt --all --check` | 通过 |
| `cargo +nightly clippy --all-targets --all-features -- -D warnings` | 通过(dial.rs `needless_bool` 已由本分支第 1 个提交修复;Cargo.toml `tar` unused dependency 警告为 main 既有 manifest 警告,非 clippy 失败项) |

## 已知事项与回退

- 失败日志为人类可读输出,无持久化格式契约;回退仅涉及还原 2 个提交,无状态迁移或配置影响。
