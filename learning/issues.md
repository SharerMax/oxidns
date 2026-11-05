# OxiDNS 问题记录

> 用途：记录代码评审与实现分析中发现、但未随当次改动修复的问题，作为后续独立改动的输入。
> 基线：分支 `fix/code-review-priorities`，HEAD `d69c5cd1`（2026-11-05）。
> 约定：每条问题记录现象、证据、当前覆盖、影响、复现方式和建议方案；修复后保留条目并更新状态，不删除历史。

## 索引

| ID | 标题 | 级别 | 状态 |
|---|---|---|---|
| ISSUE-001 | 消息编解码失去第三方差分校验 | 中 | 待处理 |

---

## ISSUE-001 消息编解码失去第三方差分校验

**级别**：中（不影响当前请求路径，但削弱 wire 格式回归防线）
**发现方式**：核对 `AGENTS.md` 与 `ai/testing-strategy.md` 的测试矩阵时，发现文档引用的测试文件已不存在
**状态**：待处理（文档引用已在 `d69c5cd1` 修正，覆盖缺口未补）

### 1. 现象

仓库不再有跨实现的消息编解码校验。`crates/proto` 的 wire 测试全部是自洽性验证：用本仓库的编码器生成字节、再用本仓库的解码器读回。编码器与解码器共享同一套对协议的理解，因此**对称性错误无法被发现**。

### 2. 证据

原差分测试由上游提交 `38ce2436`（2026-04-18，`test: remove hickory-proto compatibility test dependency`）删除：

| 项 | 变更 |
|---|---|
| `tests/message_hickory_compat.rs` | 删除（663 行） |
| `Cargo.toml` | 移除 `[dev-dependencies] hickory-proto = "0.25.2"` |
| `Cargo.lock` | 清理 60 行相关条目 |

被删文件包含 3 个用例，均以 snapshot 脚手架做双向差分：

1. `forgedns_fixture_roundtrip_matches_hickory_decode`——本仓库构造报文，第三方实现解码后逐字段比对；
2. `hickory_fixture_roundtrip_matches_forgedns_decode`——第三方实现构造报文，本仓库解码后逐字段比对；
3. `hickory_fixture_reencode_preserves_semantics`——第三方报文经本仓库解码再编码，语义不变。

当前 `Cargo.toml`、`crates/proto/Cargo.toml`、`Cargo.lock` 中已无任何 `hickory` 引用。

### 3. 当前覆盖

`cargo test -p oxidns-proto` → **74 passed**，覆盖形态为：

- `wire::codec::tests::message_roundtrip_matrix_covers_common_layouts`——查询/响应/带 EDNS/多 section 的回环；
- `wire::codec::tests::decode_message_rejects_invalid_wire_matrix`——短头、尾部垃圾、重复 OPT 等畸形报文拒绝；
- `wire::codec::tests::limited_encode_*`——UDP 截断与上限行为；
- `wire::rdata::tests::*_rdata_parse_encode_roundtrip` + `*_rejects_invalid_wire_matrix`——按 RR 类型族的回环与畸形输入；
- DNSSEC 类型位图、SVCB 键排序、EDNS option 长度等单点约束。

其中只有畸形输入矩阵包含**手工写死的原始字节**，正向路径几乎全部由本仓库编码器生成输入。

### 4. 影响与风险

DNS 是跨实现协议，真实对端是 `dig`、Unbound、BIND、Windows stub resolver、各类 DoH 客户端。以下缺陷在自洽测试下全部为绿，但对端会解析失败或语义偏移：

- 名字压缩：指针环、前向指针、跨 section 复用、大小写保留；
- EDNS(0)：OPT 伪 RR 的 TTL/class 字段语义、option 长度与截断；
- 截断边界：`TC` 置位时 OPT 是否保留、section 计数与载荷长度的一致性；
- 未知 RR 类型透传、TXT 多段长度、SVCB/HTTPS 参数顺序；
- 扩展 RCODE（>15）与 EDNS 的耦合。

这类问题通常不会在本地或 CI 暴露，而是在用户侧以「某个域名/某种记录解析不出来」的形式出现，定位成本高。

### 5. 复现

```bash
# 证明当前没有任何第三方实现参与校验
grep -ri hickory --include=Cargo.toml --include=Cargo.lock .
cargo test -p oxidns-proto          # 74 passed，全部自洽
git show 38ce2436 --stat            # 被删除的差分测试
```

找一个自洽测试无法覆盖的例子：把 `encode_name` 的压缩指针偏移写成相对报文尾部的偏移（编码与解码同时按新约定实现），回环测试仍全绿，但外部解码器会读到错误名字。

### 6. 建议方案

按成本从低到高，三者可叠加：

1. **固化外部字节样本（推荐，成本最低）**：用 `dig`/`kdig`/Hickory 一次性生成覆盖上述风险点（压缩、EDNS、截断、SVCB、DNSSEC、未知类型）的报文，以十六进制常量或 `.bin` fixture 存入 `crates/proto/tests/fixtures/`，只做单向「外部字节 → 本仓库解码 → 字段断言」，不引入依赖。
2. **双向固化**：对同一批 fixture 增加「解码 → 再编码 → 与外部字节逐字节比对」用例。注意部分字段（如压缩方式、section 顺序）允许等价编码，需要按语义断言而非全字节比对。
3. **可选的差分测试 feature**：把 `hickory-proto` 放在可选 dev-feature 下，仅在专门的 CI job 跑，避免默认构建重新背上该依赖（这也是 `38ce2436` 移除它的直接动机）。

建议同时补一条 `ai/testing-strategy.md` 的 wire 变更检查项：改动 `crates/proto` 的编解码时，必须新增或更新至少一条外部字节 fixture。

### 7. 关联

- 文档漂移已修复：`d69c5cd1`（`AGENTS.md`、`ai/testing-strategy.md`、`ai/change-impact-matrix.md` 中原引用已替换为 `cargo test -p oxidns-proto` 与 `crates/proto/src/wire/`）。
- 相关代码：`crates/proto/src/wire/codec.rs`、`crates/proto/src/wire/compression.rs`、`crates/proto/src/wire/rdata/`。
- 相关流程：`ai/testing-strategy.md` 的 DNS Correctness Rules、`ai/performance.md` 的请求热路径约束（wire 解码处于热路径，修复时不得引入每包分配）。
