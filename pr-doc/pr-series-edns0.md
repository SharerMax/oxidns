# PR 系列：EDNS0 选项过滤与匹配（drop_edns0opt / edns0opt）

基线：分支 `fix/code-review-priorities`（HEAD `42eb9377`），相对 `origin/main`（`b15094d4`）领先 92 个提交。
本系列是分支末尾的 4 个提交；其余提交见 `pr-series-review-fixes.md`、`pr-series-semaphore-pools.md`、`pr-series-staged-reload.md`。

> **执行结果（2026-11-14）**：E1（`6e5574db` + `42eb9377`）已随 `refactor/edns0-option-code` 合入 main（PR #415）；E2 中的 matcher 已单独拆出 `feat/edns0opt-matcher`（基线 main，提交 `3a6bf04d`，PR 未建）；E2 中的 `drop_edns0opt`（`9ba81e58`）**丢弃**——上游 PR #425（`bfe29240`，`edns_filter` executor：preserve/strip/strip_unknown 策略 + allow/deny 列表）已覆盖该能力，重建会造成功能重复。

结论：

- 不应把整个领先区间合成一个 PR；本系列也不并入 review-fixes 的既有 5 个系列——它与缓存/上游/重载无共享行为面，独立评审成本最低。
- 本系列自身建议拆 **2 个 PR**：一个纯重构前置（可先合、不阻塞），一个成对插件功能（executor + matcher）。
- 若评审方希望减少往返，也可用 **1 个 PR** 携带全部 4 个提交（按分支原序），代价是约 1650 行 diff 与 refactor/feature 混审。

## 提交清单

| # | 提交 | 标题 | 性质 | 依赖 |
|---|---|---|---|---|
| 1 | `6e5574db` | feat(proto): add EdnsOption::code() helper | proto 增量 API（5 行） | 无 |
| 2 | `42eb9377` | refactor(plugin): use EdnsOption::code in forward_edns0opt | 纯重构，行为不变 | 1 |
| 3 | `9ba81e58` | feat(plugin): add drop_edns0opt to filter EDNS0 options | 新 executor 插件（含 WebUI/文档/i18n） | 无，但被 4 修改 |
| 4 | `6fa9d32e` | feat(plugin): add edns0opt matcher for EDNS0 option presence | 新 matcher 插件；顺带把 drop_edns0opt 改用 `code()`（5 行） | 1、3 |

## PR E1（前置纯重构）：`6e5574db` + `42eb9377`

建议标题：`refactor(proto): add EdnsOption::code and reuse it in forward_edns0opt`

**描述要点**

- 动机：`u16::from(EdnsCode::from(option))` 这类转换在 `forward_edns0opt` 内重复 5 处；新增 `EdnsOption::code()` 后统一走一个公开方法，后续 matcher / drop 插件复用。
- 方案：`crates/proto` 增加 `EdnsOption::code()`（委托既有的 `From<EdnsOption> for EdnsCode`），语义完全等价；`forward_edns0opt` 仅替换调用点，无逻辑改动。
- 影响面：proto 公开 API **增量**（无签名破坏）；运行时行为不变；无配置变化。
- 测试：既有 `forward_edns0opt` 单测原样通过；`cargo test --lib edns0` 37 项通过；`cargo +nightly fmt --all --check` 干净；`cargo clippy --lib --all-features -- -D warnings` 干净。
- 回退：两个提交均可独立 revert，不影响其它系列。

## PR E2（功能）：`9ba81e58` + `6fa9d32e`

建议标题：`feat(plugin): add drop_edns0opt executor and edns0opt matcher`

**描述要点**

- 动机：现有 `forward_edns0opt` 只能“把选中的请求 option 复制进最终响应”，缺少“移除/只保留指定 option”和“按 option 存在性路由”两件事，导致 ECS、COOKIE、NSID 等选项无法在 sequence 中按需裁剪或分流。
- `drop_edns0opt`（executor）：
  - `mode`：`blacklist`（默认，移除 `codes`）或 `whitelist`（只保留 `codes`）；
  - `skip_request_strip`（默认 `false`）：仅关闭请求侧处理，响应侧始终处理；
  - 双侧处理都保留 OPT 记录本身，EDNS version、flags（含 DO）、UDP payload size 不变；无匹配时不动报文，也不为裁剪而新建 OPT；
  - 空 `codes`：blacklist 为 no-op，whitelist 清空所有 option（保留空 OPT）。
- `edns0opt`（matcher）：
  - 按请求侧（`resp: false`，默认）或响应侧（`resp: true`）OPT 中的 option code 存在性匹配；
  - `codes` 必填且非空，空列表在配置解析期直接报错；
  - `require`：`any`（默认）/`all`；
  - 只读匹配，不 clone `Message`，不匹配 option data，也不做 code 别名。
- 组合注意：同一 sequence 中若先 `drop_edns0opt` 再 `edns0opt`，被移除的 code 将不再命中；文档已写明该顺序语义，与 `forward_edns0opt` 保留/转发语义形成对照。
- 配套：`src/plugin/{executor,matcher}/mod.rs` 注册、WebUI 插件定义与中英文案（`plugin-defined.ts` / `docs.ts`）、插件参考文档与序列文档入口；WebUI 插件注册表基线 65 → 66 → 67。
- 测试：`cargo test --lib edns0` 37 项通过（含 `drop_edns0opt` 19 项、`edns0opt` 11 项与既有 forward 用例）；`webui` 下 `vitest run lib/plugin-definitions-advanced.test.ts` 2/2 通过；fmt 与 `clippy --lib --all-features` 干净。
- 回退：两个插件均为显式配置才生效，未配置时对默认行为零影响；可分别在注册表与 WebUI 定义中整体摘除。

## 合并顺序与耦合点

1. **proto 前置**：`6fa9d32e` 与 `42eb9377` 都调用 `EdnsOption::code()`，必须在 `6e5574db` 之后。若 proto 改动被要求单独评审，E2 不能先于 E1 合入。
2. **matcher 依赖 executor**：`6fa9d32e` 修改了 `9ba81e58` 新建的 `src/plugin/executor/drop_edns0opt.rs`（换用 `code()` + 一处注释换行），cherry-pick 时 `9ba81e58` 必须先落。若把 E2 再拆成“executor PR / matcher PR”，matcher PR 必须基于已合入的 executor PR，否则该 5 行改动无处落。
3. **WebUI 基线同线冲突**：`webui/lib/plugin-definitions-advanced.test.ts` 中 `pluginKindDefinitions` 长度断言是同一条语句（65→66→67）。两个插件若拆成并行 PR，合并时必冲突；E2 把两者放在同一 PR 内按序提交即可避免。
4. **注册与 i18n 文件重叠**：`plugin-defined.ts`、`docs.ts`、`plugin-definitions/{executor,matcher}.ts`、文档索引在两条提交中各改一次；同样要求按序合入或同 PR。
5. **与其它系列无耦合**：本系列不触碰 `cache`、上游连接池、reload 路径，也不依赖 `87cad1b2` 的 Windows nightly lint 修复；可独立排期。唯一共享是 proto crate，但 `code()` 为增量方法，不影响 semaphore/reload 系列。
6. **单 PR 备选**：若选择 1 个 PR，保持分支原序 `9ba81e58 → 6e5574db → 6fa9d32e → 42eb9377` 即可（该顺序已在本分支验证通过）；不要改成先 refactor 后插件，否则 `6fa9d32e` 对 `drop_edns0opt.rs` 的上下文仍成立，但 WebUI 基线会多一次无意义冲突。

## cherry-pick 操作序列（供执行，尚未运行）

```bash
# PR E1：纯重构前置
git checkout -b refactor/edns0-option-code origin/main
git cherry-pick 6e5574db 42eb9377
git push fork refactor/edns0-option-code

# PR E2：成对插件（基于 E1 合入后的 main，或直接基于 E1 分支）
git checkout -b feat/edns0-plugins refactor/edns0-option-code
git cherry-pick 9ba81e58 6fa9d32e
git push fork feat/edns0-plugins
```

单 PR 备选：

```bash
git checkout -b feat/edns0-plugins origin/main
git cherry-pick 9ba81e58 6e5574db 6fa9d32e 42eb9377
git push fork feat/edns0-plugins
```

## 与既有系列文档的关系

- `pr-series-review-fixes.md` 的“汇总”已按当前 92 个提交同步，并新增本系列一行。
- `pr-series-review-fixes.md` 中系列 3 追加了 `64407aac`（缓存跳过原因 WebUI 本地化），该提交以 rebase 后的 `770f51a0` 落在已删除的 `fork/fix/cache-hardening` 分支上，现以标签 `archive/cache-hardening` 归档；`pr-body-cache-hardening.md` 保留为历史参考，实际出 PR 走 `pr-split-cache-hardening.md` 的 P1–P4。
- 文档/学习笔记类提交（`learning/`、`pr-doc/`、架构页）仍作分支元数据，不进入上游 PR。
