# PR:fix(server): ingress validation, framing, and drain hardening

**目标分支**:svenshi/oxidns `release/v1.7.0`
**来源分支**:SharerMax/oxidns `fix/server-hardening`(8 个提交,已合入 `release/v1.7.0` 基线)
**创建命令**:

```bash
gh pr create \
  --repo svenshi/oxidns \
  --head SharerMax:fix/server-hardening \
  --base release/v1.7.0 \
  --title "fix(server): ingress validation, framing, and drain hardening" \
  --body-file pr-body-server-hardening.md
```

---

## 摘要

DNS 服务面的八项加固与修复,均由代码评审发现,互相独立:

1. **TCP keepalive 补 `with_time`**——此前只设置 `with_interval`,`TCP_KEEPIDLE` 停留在系统默认(~2h),`idle_timeout` 只控制探测间隔,死连接迟迟不被回收;现在 keepalive 起始时间同样取 `idle_timeout`。
2. **DoQ / DoH3 任务纳入 `TaskTracker`**——每流/每请求任务原先用 `tokio::spawn` 脱离跟踪,关停或重载时插件注册表已销毁 executor,而这些任务仍在执行请求;现在按 TCP 的两阶段设计在 `run_server` 的 close+wait 中排空。
3. **入站 QR/opcode 预检**——`QR=1` 的伪造响应包与不支持的 opcode 原先会走完整执行链、写入缓存并占用上游容量;现在直接回 `REFUSED`,不进入流水线。
4. **恶意 UDP 报文日志降级**——按线速发送的垃圾报文原先每包一条 `warn`,可淹没日志;`DnsError::Protocol` 解码失败降为 `debug`,socket 级错误仍保留 `warn`。
5. **TCP 帧长硬校验**——2 字节前缀原用未经检查的 `as u16` 转换(release 下 `debug_assert` 不生效),超大消息会回绕前缀并使客户端流错位;现在越界直接返回协议错误。
6. **EDNS 版本回 `BADVERS`**——RFC 6891 §6.1.3 要求 `OPT` version 非 0 时回 `BADVERS`,此前会正常执行该查询;响应带 OPT,查询不进流水线。
7. **fallback `threshold: 0` 语义**——`0` 原被静默改写为 500ms,希望立即待命的配置无法生效;现在 500 作为 serde 默认值,显式 `0` 立即启动备用路径(文档同步)。备用路径闸门的 `select!` 加 `biased`,主路径已成功时不再多打一次会被中止的备用查询。
8. **UDP 预检前移到 spawn 之前**——第 3 项的校验原先在每包任务的闭包内,垃圾包洪泛仍要为每个数据报付任务创建成本;现在解码后立即丢弃,`handle_request` 保留同样的校验以覆盖流式服务器。

## 合入基线

分支已合入 `origin/release/v1.7.0`(`52ae666c`,合并提交 `337c3eed`),因此相对目标分支的 diff 只有下列 8 个提交。该基线已包含管理 API 系列(#401),不包含本系列;合并仅自动处理了 `docs/.../control-flow.mdx` 一处文档上下文,服务代码无重叠。

## 提交

| 提交 | 内容 |
|---|---|
| `c555180a` | fix(server): set TCP_KEEPIDLE so idle_timeout reaps dead peers |
| `a5f9b67b` | fix(server): track DoQ and DoH3 request tasks for shutdown drain |
| `2d90e6e6` | fix(server): refuse non-query ingress before the pipeline |
| `8785686c` | fix(server): log malformed UDP datagrams at debug level |
| `5ab8ac57` | fix(transport): reject oversized TCP frames instead of wrapping u16 |
| `4e927394` | fix(server): answer BADVERS for unsupported EDNS versions |
| `9591ba12` | fix(fallback): honor threshold 0 and prefer primary state on ties |
| `12cc5554` | perf(server): drop non-query UDP datagrams before spawning |

## 测试

- 新增:`test_handle_request_refuses_non_query_ingress`、`test_handle_request_answers_badvers_for_unsupported_edns_version`;均断言响应 rcode 且执行链未被调用。
- 全量 `cargo test --all-features`(合并基线后):1237(lib)+ 5 + 104(集成)= **1346 项通过**,0 失败,3 ignored。
- 门禁:`cargo +nightly fmt --all --check` 干净;`cargo +nightly clippy --all-targets --all-features -- -D warnings` 仅剩 `src/infra/network/dial.rs:483` 一处基线既有的 `needless_bool`(本系列触及文件无告警)。

## 影响面

- **行为变化**:`QR=1` 或非 query opcode 的入站包回 `REFUSED`(不再执行、不写缓存、不占上游);EDNS version>0 回 `BADVERS`;超过 65535 字节的 TCP 帧返回协议错误;`idle_timeout` 现在同时决定 keepalive 起始时间;UDP 解码失败不再逐包 `warn`;QUIC/H3 请求随关停/重载排空。
- **配置**:`fallback.threshold` 省略时默认仍为 500(与旧实际行为一致),显式 `0` 的含义由"被改写为 500"变为"立即待命";文档 `docs/docs/plugin-reference/executor/control-flow.mdx` 已同步。无其他 schema 变化。
- **热路径**:第 8 项减少垃圾流量下的任务创建;第 7 项的 `biased` 只影响就绪竞争时的偏好;其余为错误路径/关停路径。

## 已知事项

- 本系列不携带 `87cad1b2`(Windows nightly 的 rustfmt/clippy 修补)。合并基线后,Windows nightly clippy 只剩 `src/infra/network/dial.rs:483`(`needless_bool`)一处基线告警(原 `src/infra/upgrade/mod.rs:287` 的 unused import 已由基线修复),不影响 Linux CI。
