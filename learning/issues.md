# OxiDNS 问题记录

> 用途：记录代码评审与实现分析中发现、但未随当次改动修复的问题，作为后续独立改动的输入。
> 基线：分支 `fix/code-review-priorities`，HEAD `2e9f0c8d`（2026-11-05）；跨分支核查会单独注明目标分支。
> 约定：每条问题记录现象、证据、当前覆盖、影响、复现方式和建议方案；修复后保留条目并更新状态，不删除历史。

## 索引

| ID | 标题 | 级别 | 状态 |
|---|---|---|---|
| ISSUE-001 | 消息编解码失去第三方差分校验 | 中 | 待处理 |
| ISSUE-002 | 重载失败回滚时缓存 dump 被候选实例覆盖 | 高 | 待处理 |

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

---

## ISSUE-002 重载失败回滚时缓存 dump 被候选实例覆盖

**级别**：高（一次失败重载即可清空数千条持久缓存，但不影响进程继续服务）
**发现方式**：核查上游 issue [#403](https://github.com/svenshi/oxidns/issues/403)，并在 `fix/cache-hardening` 分支复核代码路径
**状态**：待处理（核查基线 `fix/cache-hardening` @ `44c39246`，该分支未修复）

### 1. 现象

配置重载时，候选配置的插件初始化失败。旧代停止时已把完整缓存写入 `dump_file`；随后候选代中已 `init` 的 cache 实例被销毁，`destroy()` 再次把**空或只加载了一部分的缓存**写回同一个文件，覆盖掉旧代的完整 dump。回滚装配旧配置时只能从被覆盖后的文件加载，历史缓存丢失。

上游报告中的关键日志（`size: 4096`、`lazy_cache_ttl: 8640000`、`dump_interval: 3600`）：

```text
# 旧代正常 dump
destroy: dumping cache tag=cache_foreign len_before=3602 file=./cache_foreign.dump
dump_cache_to_bytes total_in_map=3602 written=3602 skipped=0

# 候选配置失败
Configuration reload failed: Plugin error: failed to parse forward plugin config: invalid IP address syntax

# 回滚时销毁刚初始化的候选 cache，同一个文件被 0/97 条覆盖
Destroying 4 plugins in reverse order
destroy: dumping cache tag=cache_foreign len_before=0 file=./cache_foreign.dump
dump_cache_to_bytes total_in_map=0 written=0 skipped=0
destroy: dumping cache tag=cache_domestic len_before=90 file=./cache_domestic.dump
dump_cache_to_bytes total_in_map=143 written=97 skipped=46

# 之后只能加载到被覆盖后的条目
Loaded 97 cache entries from ./cache_domestic.dump
```

### 2. 证据

| 位置 | 行为 |
|---|---|
| `src/plugin/executor/cache/mod.rs:1450-1476` | `Cache::destroy()` 只要配置了 `dump_file` 就无条件调用 `dump_cache_to_file`，不区分正常关停、已提交重载、候选中止或回滚 |
| `src/plugin/executor/cache/mod.rs:1402-1426` | `init()` 启动异步 `spawn_load_task` 与周期 dump 任务；`init()` 返回时加载可能尚未完成 |
| `src/plugin/executor/cache/mod.rs:555-580` | 加载任务由裸 `tokio::spawn` 执行，不归 `destroy()` 管理；销毁时仍可能继续向 `cache_map` 插入 |
| `src/plugin/registry/runtime.rs:135-142` | 候选 registry 初始化失败时调用 `candidate.destroy()`，触发上述 dump |
| `src/plugin/registry/mod.rs:631-663` | `PluginRegistry::destroy()` 按 `init_order` 逆序调用每个插件的 `destroy()`，没有销毁原因参数 |
| `src/app.rs:454-482` | 非 Unix(含 Windows)重载先 `bootstrap::stop` 旧代，再 `assemble` 候选；候选失败进入回滚 |
| `src/app.rs:485-516` | `finish_rollback` 用旧配置重新 `assemble`，此时读到的是被候选覆盖后的 dump |
| `src/plugin/executor/cache/persistence.rs:49-73` | 临时文件名唯一，但最终 `fs::rename(tmp, dump_path)` 仍无条件覆盖正式文件 |

### 3. 触发链

1. `POST /reload` 或 reload 执行器触发重载。
2. 非 Unix 路径先 `bootstrap::stop` 旧 assembly，旧 cache 的 `destroy()` 把完整缓存 dump 到 `dump_file`。
3. `assemble(candidate)`：cache 插件 `init()` 成功，启动异步加载和周期 dump 任务。
4. 后续某个插件(如 `forward` 的非法 IP)初始化失败，`build_registry` 调用 `candidate.destroy()`。
5. 候选 cache 的 `destroy()` 把当前 map(空，或加载到 90/143 条)写回同一个 `dump_file`，`fs::rename` 覆盖旧 dump。
6. `finish_rollback` 用旧配置重新装配；新 cache 从被覆盖的文件加载，历史条目永久丢失。

Unix staged reload 也不能幸免：候选在旧代仍运行时构建，候选失败同样会 `candidate.destroy()` 并覆盖共享的 `dump_file`。旧代内存里的缓存还在，但磁盘上的保险已经失效；若此时进程崩溃或重启，缓存仍会丢失。

### 4. 为什么 `fix/cache-hardening` 没有修复

- 分支上的 `93797a12`（同 `c3078f54`）只做了两件事：把序列化移入 `spawn_blocking`，以及给 `.tmp` 临时文件加 `pid + 序号` 避免并发写交错。`fs::rename` 仍然覆盖正式文件，且 `destroy()` 仍无条件 dump。
- `git diff origin/release/v1.7.0...fix/cache-hardening -- src/plugin/executor/cache/mod.rs` 没有改动 `destroy()` / dump 触发条件。
- ECS scope 校验、single-flight、采样 LRU 轮转与持久化生命周期无关。
- 分支合并的 staged reload 只保证“候选失败不替换运行中的 assembly”，没有保证候选失败不写磁盘。

### 5. 影响

- 持久缓存(正缓存与负缓存)丢失；配置了 `lazy_cache_ttl` 的长窗口缓存可能积累数周，一次失败重载即清零。
- 回滚后 DNS 服务仍可用，但冷启动会向上游放大查询量，热门域名尤其明显。
- 失败重载越频繁、缓存越大，损失越显著；该问题不影响协议正确性，因此普通回环测试不会发现。
- 加载任务未被跟踪，销毁与加载并发时还可能把“加载到一半”的快照写盘，使覆盖结果不确定。

### 6. 复现

最小配置：两个带 `dump_file` 的 `cache`，后接一个初始化必然失败的插件（例如非法 `forward` 地址）：

```yaml
plugins:
  - tag: cache_domestic
    type: cache
    args:
      size: 4096
      short_circuit: true
      dump_file: ./cache_domestic.dump
      dump_interval: 3600
  - tag: forward_bad
    type: forward
    args: 1.1.1.1
```

先正常运行积累缓存并正常重载一次，确认 dump 为完整条目；再把 `forward_bad` 改成非法 IP 并触发重载。观察日志中旧代 `len_before=N`、候选 `len_before=0/部分`，以及随后 `Loaded` 的条目数。

自动化回归建议：

1. `Cache` 单测：`persist_enabled = false` 时 `destroy()` 不创建/不修改 dump。
2. 集成测试：预置 dump 文件 → 构造候选插件失败 → 断言 dump 字节完全不变。
3. 集成测试：成功重载仍正常 dump，回滚后旧配置加载的条目数与重载前一致。
4. Unix 专项：staged 候选失败时，运行中旧代的 dump 文件不被修改。

### 7. 建议方案

**主方案：给持久化加“提交门”**，让只有真正投入运行的 assembly 才能写 dump。

1. 在 `Plugin` trait 增加默认空实现的激活/中止钩子，例如 `async fn on_activate(&self) -> Result<()> { Ok(()) }`，避免修改现有几十个 `destroy()` 覆写。
2. `Cache` 增加 `persist_enabled: AtomicBool`，`init()` 时置 `false`；周期 dump 与 `destroy()` 都先检查该标志。
3. 在 `activate_assembly` 完整成功（API 已启动）后，由 runtime 对所有插件调用 `on_activate()`；`Cache` 在此置 `true`。
4. 候选失败、回滚、启动中止走普通 `destroy()`，因 `persist_enabled == false` 而不写文件；旧代在正常重载/关停时已是 `true`，仍会正常落盘。
5. 同时把 `spawn_load_task` 纳入 `TaskTracker`/`ManagedTaskHandle`，`destroy()` 先停加载任务再决定是否 dump，避免“加载到一半”的快照。

**兜底方案（可与主方案叠加）**：停止旧代前把现有 dump 备份为 `.bak`；回滚装配前先恢复备份。该方案只能缩小窗口，无法解决 Unix staged 候选失败时旧代仍在运行时文件被覆盖的问题，因此不应单独使用。

**不建议**用“map 为空就跳过 dump”或“map 比文件小就跳过”的判断：合法空缓存与合法淘汰都会误伤，且无法覆盖部分加载的快照。

### 8. 关联

- 上游 issue：[svenshi/oxidns#403](https://github.com/svenshi/oxidns/issues/403)（1.7.0 milestone，Open）。
- 核查分支：`fix/cache-hardening` @ `44c39246`；相关提交 `93797a12` / `c3078f54` 仅处理序列化线程与临时文件名。
- 相关代码：`src/plugin/executor/cache/mod.rs`、`src/plugin/executor/cache/persistence.rs`、`src/plugin/registry/runtime.rs`、`src/plugin/registry/mod.rs`、`src/app.rs`、`src/app/bootstrap.rs`。
- 相关约束：`ai/performance.md` 的重载资源安全、`ai/operations-runbook.md` 的重载与回滚流程。
