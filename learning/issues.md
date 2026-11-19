# OxiDNS 问题记录

> 用途：记录代码评审与实现分析中发现、但未随当次改动修复的问题，作为后续独立改动的输入。
> 基线：main v1.7.1（`ad2046d3`，2026-11-14）；涉及分支特有状态或历史基线的核查会单独注明。
> 约定：每条问题记录现象、证据、当前覆盖、影响、复现方式和建议方案；修复后保留条目并更新状态，不删除历史。

## 索引

| ID | 标题 | 级别 | 状态 |
|---|---|---|---|
| ISSUE-001 | 消息编解码失去第三方差分校验 | 中 | 待处理 |
| ISSUE-002 | 重载失败回滚时缓存 dump 被候选实例覆盖 | 高 | 已修复（上游） |
| ISSUE-003 | dynamic_domain_set 测试并行运行时全局 API 路由竞争 | 中 | 待处理 |
| ISSUE-004 | 集成测试并行运行时挂起（过时失败用例泄漏运行时所致） | 中 | 已定位并修复（随 PR-1 `7e9f7d13`） |
| ISSUE-005 | `task_center_restarts_scheduler_after_runtime_replacement` 偶发 Unavailable 导致 CI 失败 | 低 | 待处理（既有 flake，与 forward 改动无关） |

---

## ISSUE-001 消息编解码失去第三方差分校验

**级别**：中（不影响当前请求路径，但削弱 wire 格式回归防线）
**发现方式**：核对 `AGENTS.md` 与 `docs/ai/testing-strategy.md` 的测试矩阵时，发现文档引用的测试文件已不存在
**状态**：待处理（main v1.7.1 复核仍成立：无 hickory 引用、无外部字节 fixture；文档引用已在 `d69c5cd1` 修正，覆盖缺口未补）

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

`cargo test -p oxidns-proto` → **82 个测试函数**（main v1.7.1，`#[test]`/`#[tokio::test]` 计数；取证时基线为 74 passed），覆盖形态为：

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
cargo test -p oxidns-proto          # 全部自洽（main 上 82 个测试函数）
git show 38ce2436 --stat            # 被删除的差分测试
```

找一个自洽测试无法覆盖的例子：把 `encode_name` 的压缩指针偏移写成相对报文尾部的偏移（编码与解码同时按新约定实现），回环测试仍全绿，但外部解码器会读到错误名字。

### 6. 建议方案

按成本从低到高，三者可叠加：

1. **固化外部字节样本（推荐，成本最低）**：用 `dig`/`kdig`/Hickory 一次性生成覆盖上述风险点（压缩、EDNS、截断、SVCB、DNSSEC、未知类型）的报文，以十六进制常量或 `.bin` fixture 存入 `crates/proto/tests/fixtures/`，只做单向「外部字节 → 本仓库解码 → 字段断言」，不引入依赖。
2. **双向固化**：对同一批 fixture 增加「解码 → 再编码 → 与外部字节逐字节比对」用例。注意部分字段（如压缩方式、section 顺序）允许等价编码，需要按语义断言而非全字节比对。
3. **可选的差分测试 feature**：把 `hickory-proto` 放在可选 dev-feature 下，仅在专门的 CI job 跑，避免默认构建重新背上该依赖（这也是 `38ce2436` 移除它的直接动机）。

建议同时补一条 `docs/ai/testing-strategy.md` 的 wire 变更检查项：改动 `crates/proto` 的编解码时，必须新增或更新至少一条外部字节 fixture。

### 7. 关联

- 文档漂移已修复：`d69c5cd1`（`AGENTS.md`、`docs/ai/testing-strategy.md`、`docs/ai/change-impact-matrix.md` 中原引用已替换为 `cargo test -p oxidns-proto` 与 `crates/proto/src/wire/`）。
- 相关代码：`crates/proto/src/wire/codec.rs`、`crates/proto/src/wire/compression.rs`、`crates/proto/src/wire/rdata/`；main 上指针环检测已经过 O(1) 化（#432 `75d3e4ab`）与判定收紧（`83153730`）两次加固，但仍是自洽验证。
- 相关流程：`docs/ai/testing-strategy.md` 的 DNS Correctness Rules、`docs/ai/performance.md` 的请求热路径约束（wire 解码处于热路径，修复时不得引入每包分配）。

---

## ISSUE-002 重载失败回滚时缓存 dump 被候选实例覆盖

**级别**：高（一次失败重载即可清空数千条持久缓存，但不影响进程继续服务）
**发现方式**：核查上游 issue [#403](https://github.com/svenshi/oxidns/issues/403)，并在 `fix/cache-hardening` 分支复核代码路径
**状态**：已修复（上游 `1992c8f9` / PR #422，已进 main）；本条记录的证据取自修复前的历史基线 `871f8da3`，仅作存档

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

- 上游 issue：[svenshi/oxidns#403](https://github.com/svenshi/oxidns/issues/403)（1.7.0 milestone；截至 2026-11-11 仍显示 Open，项目看板为 “Ready for release”）。
- 核查分支：`fix/cache-hardening` @ `44c39246`；相关提交 `93797a12` / `c3078f54` 仅处理序列化线程与临时文件名。
- 相关代码：`src/plugin/executor/cache/mod.rs`、`src/plugin/executor/cache/persistence.rs`、`src/plugin/registry/runtime.rs`、`src/plugin/registry/mod.rs`、`src/app.rs`、`src/app/bootstrap.rs`。
- 相关约束：`docs/ai/performance.md` 的重载资源安全、`docs/ai/operations-runbook.md` 的重载与回滚流程。

### 9. 修复情况（上游）

上游已按本条第 7 节「持久化提交门」的思路修复：

- 提交：`1992c8f9` `fix(cache): preserve persisted entries across failed reloads`（PR #422，合并提交 `40c9718b`），2026-11-10 进入 `release/v1.7.0`，随发布准备提交 `56b7b10f` 出入 v1.7.0。
- 上游做法：激活前先恢复并裁剪 dump；不可读、非法或超时的 dump 受保护，不被自动覆盖；周期持久化只在运行时成功激活之后启动；启动、激活与拆除在托管生命周期中串行化，调用方取消不会跳过激活或清理。
- 上游回归测试：`failed_candidate_preserves_both_cache_dumps`、`failed_reload_restores_historical_cache`、`failed_api_start_preserves_cache_dump`。
- 本条的代码行号取自修复前基线 `871f8da3`；`src/app.rs` / `src/app/bootstrap.rs` 在 `d11368b2`（启动与生命周期函数改名）之后行号已变化。

遗留影响（本仓库侧）：

- `fix/cache-hardening` @ `770f51a0` 的基线是 `871f8da3`，**不含**该修复：`git merge-base --is-ancestor 1992c8f9 archive/cache-hardening` 返回非 0。该分支已于 2026-11-11 删除（本地与 fork），提交以标签 `archive/cache-hardening` 归档；缓存子 PR 一律从 main（当前 v1.7.1 `ad2046d3`）新建分支再 cherry-pick，避免重新引入 #403。
- 拆分方案与冲突预期见 `pr-doc/pr-split-cache-hardening.md`；与 #422 重叠的缓存子 PR（P1/P3/P4）出 PR 前需按 main 现状重放差异面。
- 相关约束路径已随文档站点重构迁移：`docs/ai/performance.md`、`docs/ai/operations-runbook.md`。

---

## ISSUE-003 dynamic_domain_set 测试并行运行时全局 API 路由竞争

**级别**：中（仅影响 `cargo test` 本地/CI 稳定性，不影响运行时行为；但会让质量门随机变红，掩盖真实回归）
**发现方式**：在 `feat/provider-degraded-startup` 上跑 `cargo test --lib provider` 时随机失败；stash 改动后在基线 `ad2046d3`（main v1.7.1）上以同过滤器复现，且每次失败的用例集不同，确认为与改动无关的预先存在问题
**状态**：待处理

### 1. 现象

`cargo test --lib plugin::provider`（或全量 `cargo test --lib`）中 `dynamic_domain_set` 的若干用例随机失败，报错形如：

```
thread 'plugin::provider::dynamic_domain_set::tests::dynamic_domain_set_appends_and_matches' panicked at tests.rs:97:27:
backend should start: Plugin("duplicate API route registered: GET /plugins/learned/rules")
```

单过滤器重跑（如 `cargo test --lib plugin::provider::dynamic_domain_set`）则全部通过；并行集合中失败集每次运行不同（`appends_and_matches`、`async_append_is_ordered_before_clear`、`rule_api_adds_removes_and_clears`、`sync_append_flushes_without_batch_or_tick` 等都出现过）。

### 2. 根因

三件事叠加：

1. **固定 tag**：`dynamic_domain_set/tests.rs` 的所有用例都用固定 tag `"learned"`（tests.rs 第 94、123、166、199、227、262、294、328、354、432 行等），backend 启动时经 `register_plugin_api!`（`src/plugin/provider/dynamic_domain_set/api.rs:173`）注册 `GET/POST/DELETE /plugins/learned/rules` 与 `POST /plugins/learned/rules/clear`。
2. **全局注册表**：`register_plugin_api!` 的实现（`src/api/macros.rs:13`）在 `global_api_register()` 非空时把路由注册进**当前安装的全局 hub**，而 `ApiHub::register_route`（`src/api/hub.rs:354`）对重复路由返回 `Err("duplicate API route registered: ...")`。
3. **缺少序列化护栏**：仓库已提供 `global_api_test_guard()`（`src/api/global.rs:43`，静态 tokio Mutex）专门用于序列化"触碰全局 API 状态"的测试，`api/tests.rs`、`plugin/registry/tests.rs`、`plugin/provider/mod.rs` 的相关用例都在用；但 `dynamic_domain_set/tests.rs` **没有获取该 guard**。

于是当并行测试中有其他测试恰好 `install_global_api`（runtime-control、registry、api 等测试），dynamic_domain_set 用例在护栏之外读到非空全局 register，把 `/plugins/learned/rules` 注册进**别人的 hub**；两个 dynamic_domain_set 用例同时运行时，第二个在同一 hub 上撞出重复路由，`backend should start` 断言失败。谁在时序上吃亏完全随机。

### 3. 影响

- 本地 `cargo test` 与 CI 质量门随机变红；重跑可通过，属于典型的 flaky test，长期会训练"重跑就行"的习惯，掩盖真实回归。
- 不影响生产运行时：`init_plugins_with_runtime_controls` 已拒绝重复 tag，生产环境同一 tag 不会重复注册路由。

### 4. 复现方式

```
cargo test --lib provider        # 多跑几次，失败集漂移
cargo test --lib plugin::provider::dynamic_domain_set   # 单独跑则稳定通过
```

基线核查：`git stash` 后在 `ad2046d3` 上同样失败（66 passed / 3 failed，失败用例集与带改动时不同），确认与 `feat/provider-degraded-startup` 无关。

**2026-11-15 基线复现记录**（`learning-doc` 分支，代码 = main `ad2046d3`，`cargo test --lib plugin::provider` 默认并行连跑 3 次，**3/3 复现**）：

| 运行 | 失败用例 | 结果 |
|---|---|---|
| RUN 1 | appends_and_matches、async_append_is_ordered_before_clear、rule_api_adds_removes_and_clears | 57 passed / 3 failed |
| RUN 2 | async_append_is_ordered_before_clear | 59 passed / 1 failed |
| RUN 3 | rule_api_adds_removes_and_clears、sync_append_flushes_without_batch_or_tick、appends_and_matches | 57 passed / 3 failed |

三次报错均为 `backend should start: Plugin("duplicate API route registered: GET /plugins/learned/rules")`，失败用例集每次漂移；单过滤器 `plugin::provider::dynamic_domain_set` 重跑则稳定通过（13 passed）。复现稳定，与负载/时序相关。

### 5. 建议方案

**最小修（推荐）**：`dynamic_domain_set/tests.rs` 的每个用例开头加 `let _guard = crate::api::global_api_test_guard().await;`，与 `plugin/registry/tests.rs`、`api/tests.rs` 的既有约定对齐。代价是这批用例串行，但它们都是轻量文件用例，串行开销可忽略。

**叠加项（可选，防御深度）**：

1. 用例内创建 backend 前 `clear_global_api()`，并在结束时恢复；使这批用例无论全局状态如何都不受污染（`register_plugin_api!` 对空全局直接跳过注册，测试仍能验证非 API 行为）。
2. tag 加用例名后缀（`format!("learned-{}", case)`），路由天然不冲突——但注意部分用例可能断言固定路由路径，需同步调整。
3. API 路由注册的辅助方法抽成带 `#[cfg(test)]` 的隔离注册表，属于更大的重构，不建议为本问题做。

**不建议**让 `register_route` 对重复路由幂等覆盖：生产语义下重复注册意味着配置错误（tag 冲突已在 init 层拒绝，能到这里就是注册逻辑 bug），静默覆盖会掩盖问题。

### 6. 关联

- 相关代码：`src/plugin/provider/dynamic_domain_set/tests.rs`、`src/plugin/provider/dynamic_domain_set/api.rs`、`src/api/macros.rs`、`src/api/hub.rs`（`register_route`）、`src/api/global.rs`（`global_api_test_guard`）。
- 发现于：`feat/provider-degraded-startup`（PR-1，rule-file 降级）质量门验证期间；该 PR 不处理本条。

---

## ISSUE-004 集成测试并行运行时挂起（过时失败用例泄漏运行时所致）

**级别**：中（不影响运行时；整包 `cargo test --test plugin_integration` 并行模式下整批挂起，拖垮质量门）
**发现方式**：在 `feat/provider-degraded-startup` / `feat/provider-degraded-api` 上验证 PR-1/PR-2 时，整包并行运行两次均挂起；串行重跑全部通过
**状态**：已定位并修复（随 PR-1 的用例语义对齐提交 `7e9f7d13` 消除；见 §7）

### 1. 现象

`cargo test --test plugin_integration`（默认并行）运行时，首批用例正常完成后，`test_geo_provider_failure_paths_are_reported` 报 FAILED，随后**一波用例同时报 "has been running for over 60 seconds"**，整批无限期停滞，600 秒以上无进展。挂起波及的用例（dot 断连、ECS handler、edns_filter、geosite/geoip、hosts、has_cname、http_request 等约 15+ 个）单独运行均瞬间通过。

三次挂起观察中，该 FAILED 均先于挂起出现；修复该用例后并行运行不再挂起。

### 2. 取证（含 main 基线复核）

- 单测隔离运行：`test_dot_disconnect_preserves_cache_fill_and_shutdown_waits_for_requests` 单独跑 0.05s 通过；其他挂起用例同理。
- 串行全包：`cargo test --test plugin_integration -- --test-threads=1` → **116 passed / 0 failed，29.04s**（`feat/provider-degraded-api` @ `1b590a19`，含过时用例）。
- **main 基线并行复核**：`learning-doc`（代码 = main `ad2046d3`）→ **115 passed / 0 failed，29.68s，无挂起**。
- PR-1 分支（测试已修复）并行 → **116 passed，30.17s，无挂起**。
- PR-2 分支（未含修复，过时用例仍在）并行 → FAILED + 挂起（两次复现）。
- PR-2 分支合入 `7e9f7d13` 后并行 → **116 passed，29.89s，无挂起**。

结论：挂起不是预存的锁序竞争（main 基线干净），而是**过时失败用例的 panic 泄漏运行时状态**触发。

### 3. 根因

PR-1 翻转了默认行为：geoip selector 无匹配时 `plugin::init` 由"失败"改为"降级成功"。过时的 `test_geo_provider_failure_paths_are_reported` 仍断言 `expect_err`，于是：

1. `plugin::init` 成功返回，运行时已**激活**并注册为全局 current_runtime（含 providers、outbound、metrics 源等全局状态）；
2. 测试线程在 `expect_err` 处 panic，用例中止——**该运行时永远不会被 destroy**；
3. 泄漏的运行时污染后续并行用例共享的全局状态，后续走 `plugin::init` 的用例在测试串行化护栏（`enable_runtime_test_serialization`，plugin_integration.rs:60-61）上排队异常，表现为整批挂起。panic 与挂起之间的精确传播路径（哪一步阻塞）尚未逐帧定位，但"移除过时用例 panic ↔ 挂起消失"在 4 次对照运行中稳定成立。

### 4. 影响与复现

```
# 复现（需构造一个在 init 成功后 panic 的用例；旧语义用例配新代码即触发）
cargo test --test plugin_integration            # 过时用例存在时：FAILED + 整批挂起
cargo test --test plugin_integration -- --test-threads=1   # 串行：也会失败但不会拖垮全包
```

经验教训：**在 init 成功后断言失败的用例，panic 会跳过 destroy，泄漏全局运行时**。测试体中的 `expect_err` 应包裹在 `init` 之前的准备阶段，或对已激活的 runtime 先 `destroy().await` 再断言失败。

### 5. 修复

- PR-1 提交 `7e9f7d13`：将 `test_geo_provider_failure_paths_are_reported` 的第一个子场景改为断言降级语义（init 成功 + `degraded_files` 上报 + 匹配为空 + 显式 `registry.destroy()`），合入 PR-2 分支（`141ccd18`）后并行挂起消失。
- 类型不匹配等仍致命的子场景保持不变。

### 6. 关联

- 相关代码：`tests/plugin_integration.rs`（`test_geo_provider_failure_paths_are_reported`、`parse_config`）、`src/plugin/registry/runtime.rs`（`StartupSession`、测试串行化）。
- 发现于：PR-1/PR-2（rule-file 降级与 degraded API）的集成验证。
- 与 ISSUE-003 无关（那是 dynamic_domain_set 的全局 API 路由竞争，仍待处理）。

### 7. 状态

已修复。若后续再出现无 FAILED 前导的并行挂起，再按原假设（锁序）重启排查；届时先采集线程栈。

---

## ISSUE-005 `task_center_restarts_scheduler_after_runtime_replacement` 偶发 Unavailable 导致 CI 失败

**级别**：低（不影响运行时与请求路径；仅测试时序敏感，CI 偶发红）
**发现方式**：`feat/forward-bogus-answers` 的 CI job "Test bundles (minimal / standard)" 报该用例 FAILED；本地无法复现。
**状态**：待处理（既有 flake，已确认与本次 forward 改动无关）

### 1. 现象

CI（GitHub Actions，Linux）在 minimal / standard bundle 的 `cargo test --lib` 中：

```
---- infra::task::tests::task_center_restarts_scheduler_after_runtime_replacement stdout ----
thread '...' panicked at src/infra/task.rs:1222:22:
expected started outcome, got Unavailable
stack backtrace:
   ... started_run_id (src/infra/task.rs:1222)
   ... task_center_restarts_scheduler_after_runtime_replacement::{closure#1} (src/infra/task.rs:2378)
test result: FAILED. 799 passed; 1 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.82s
```

### 2. 取证（含 main 基线与本地 bundle 复核）

- **改动无关性**：`git diff main -- src/infra/task.rs` 为空。forward 改动只在 `src/plugin/executor/forward/*`、`src/plugin/matcher/rules/{config,mod}.rs`、`webui/*`、`website/*`，不触碰 scheduler / runtime / task 机制。
- **本地 bundle 复核**：minimal `cargo test --no-default-features --features minimal --lib` 连跑 3 次（801 passed / 0 failed）；standard 连跑 3 次（1180 passed / 0 failed）；单独重复该用例 5 次均通过——本地 Windows 无法复现。
- **历史**：该文件最近相关提交 `2ac5c5ee fix(tests): improve scheduler yield handling for slower platforms`，说明此测试本就因时序敏感被专门修过一次（现有 `wait_until` 里加了 1ms sleep 帮助较慢平台的线程唤醒）。
- 结论：CI Linux 偶发的 scheduler 重建时序问题，属既有 flake，非本次改动引入。

### 3. 根因

测试流程：`TaskCenter::new()` 在第一个 runtime 里创建 → drop 第一个 runtime → 在第二个 runtime 里 `spawn_fixed` 后立即 `handle.trigger()`（`src/infra/task.rs:2354-2385`）。

- `ManagedTaskHandle::trigger()`（`task.rs:614-624`）在 `try_send` 失败或 oneshot 关闭时返回 `TriggerOutcome::Unavailable`；scheduler 在 runtime 替换后尚未完成重启时即处于该状态。
- 测试用 `started_run_id()`（`task.rs:1219-1224`）对 trigger 结果直接断言，拿到 `Unavailable` 就 `panic!("expected started outcome, got {other:?}")`，**没有重试**。
- 本地平台调度足够快，trigger 到达时 scheduler 已就绪；CI 较慢时偶发落在这个窗口。

### 4. 影响与复现

- 影响：仅 CI 偶发红，阻塞质量门；线上 runtime 行为不受影响。
- 复现：在负载较高 / 调度较慢的 Linux runner 上运行 `cargo test --locked --no-default-features --features minimal --verbose`；本地单次或重复运行通常通过，难以稳定复现。

### 5. 建议方案

- 首选：直接 rerun 该 CI job（flake 通常重跑即过）。
- 根治（单独改动，不并入 forward PR）：在 `task_center_restarts_scheduler_after_runtime_replacement` 中，对 `handle.trigger()` 返回 `Unavailable` 的情况加有限次等待重试（复用现有 `wait_until` 风格），再断言 `Started { run_id: 1 }`；不要改 `ManagedTaskHandle::trigger()` 的生产语义。

### 6. 关联

- 相关代码：`src/infra/task.rs`（`ManagedTaskHandle::trigger`、`started_run_id`、该测试用例）、`.github/workflows/rust-ci.yml`（`test-bundles` job，minimal / standard）。
- 发现于：PR「forward 丢弃虚假地址应答（issue #441）」的 CI。
- 与 ISSUE-003 / ISSUE-004 无关（分别为全局 API 路由竞争与过时失败用例泄漏运行时）。
