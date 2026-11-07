# 方案:拆分 `fix/cache-hardening`

> **状态:待确认,尚未执行。** 本文只记录拆分方案,未建分支、未改写历史、未 cherry-pick、未推送、未提交。
> 确认项见文末「待定项」,确认后才开始分支操作。

- 源分支:`fix/cache-hardening`(tip `770f51a0`)
- 基线:`origin/release/v1.7.0`(`871f8da3`)

## 一、提交清单(基线..源分支)

共 11 个实质提交 + 1 个合并提交,按时间顺序:

| # | 哈希 | 性质 | 规模 | 触及面 |
|---|---|---|---|---|
| 1 | `c49fdc2b` | 修复(P0) | 189+/3- | `cache/key.rs`、`cache/mod.rs`(含 3 测试及测试辅助 `EcsInjectingExecutor`) |
| 2 | `959f558e` | 性能 | 29+/2- | `infra/cache/ttl.rs` |
| 3 | `93797a12` | 性能 | 24+/4- | `cache/persistence.rs` |
| 4 | `2c8c03b0` | 文档 | 2+ | `docs/docs/plugin-reference/executor/resolution.mdx` |
| 5 | `9ddc9540` | 性能/功能 | 253+/3- | `cache/mod.rs`(指标 + 2 单测) |
| 6 | `bc838851` | 文档(**混合 ECS 与 single-flight 两个主题**) | 3+ | `resolution.mdx` |
| 7 | `d20fa028` | 功能 | 287+/25- | `cache/mod.rs`、docs×4、WebUI i18n×4、WebUI defs×2、基线测试×1 |
| 8 | `4a6d62ef` | 合并 | 相对 release 为空 | 把上游 release 带入分支,供后续提交落盘;不含独立改动 |
| 9 | `ea831344` | 格式 | 4+/1- | `cache/mod.rs`,改的是 #1 新增测试的调用折行 |
| 10 | `44c39246` | 修复 | 116+/9- | `ecs_handler.rs`(含 2 单测) |
| 11 | `20f48f70` | 文档 | 168+/75- | `resolution.mdx` 中/英同步 |
| 12 | `770f51a0` | i18n | 12+/2- | WebUI 6 文件(`keys.ts`、`metrics.ts`、中/英 `plugin-defined.ts`、中/英 `webui.ts`) |

归类:功能 2(#5 兼性能、#7)、修复 2(#1、#10)、性能 2(#2、#3)、文档 3(#4、#6、#11)、i18n 1(#12)、格式 1(#9)、合并 1(#8)。

## 二、4-PR 拆分方案(推荐)

| 子 PR | 标题 | 提交(按序) | 依赖 | 规模 |
|---|---|---|---|---|
| P1 | `fix(cache): validate ECS scope at insert time` | `c49fdc2b` → `ea831344` → `20f48f70` | 无 | 约 360 行;3 个代码/文档文件 |
| P2 | `fix(ecs_handler): strip request ECS after downstream when not forwarded` | `44c39246` | 无(语义上与 P1 互补) | 125 行;单文件 |
| P3 | `perf(cache): rotate LRU sampling, off-thread dumps, and clarify lazy TTL` | `959f558e` → `93797a12` → `2c8c03b0` | 无 | 约 60 行;2 代码 + 1 文档 |
| P4 | `feat(cache): single-flight misses with leader re-election` | `9ddc9540` → `d20fa028` → `bc838851` → `770f51a0` | 需 P1 先合入 | 约 555 行;12+ 文件 |

顺序:P1、P2、P3 相互独立,可并行评审;P4 必须在 P1 之后合入。各子 PR 均以 `origin/release/v1.7.0` 为基线,逐提交 cherry-pick。

可选 3-PR 版本:P2 并入 P1,形成"ECS 正确性"单一 PR(约 480 行,但跨 `cache` 与 `ecs_handler` 两个插件目录);其余不变。

## 三、必须绑在一起的耦合点

1. `ea831344` 只是把 `c49fdc2b` 新增测试中的长调用折行;脱离 `c49fdc2b` 单独 cherry-pick 会因目标代码不存在而失败,必须紧跟其后。
2. `d20fa028` 在 `9ddc9540` 的 single-flight 结构上增加 leader 重选,二者不可拆;`single_flight_wait_ms`/`single_flight_max_rounds` 与 docs、WebUI schema、i18n、指标文档同处一个提交,拆开会出现"有配置无 UI"的中间态。
3. `bc838851` 同时写入 ECS scope 与 single-flight 两段文档;若要严格按主题归属,需拆成两个文档提交(改写历史)。
4. `770f51a0` 的 `plugin-defined.ts` 补丁上下文包含 `d20fa028` 才新增的 `cache_single_flight_wait_total` 行;脱离 `d20fa028` 会冲突。故与 `d20fa028` 同属 P4。
5. `770f51a0` 中的 `incomplete_answer` 是上游既有跳过原因(`cf13ad72` 引入),与本系列无关,属"补齐上游 i18n 欠账",跟随 P4 即可。
6. `bc838851`/`770f51a0` 涉及 `ecs_scope_mismatch`,语义上依赖 P1 先合入(P4 顺序已满足);代码编译不依赖。
7. WebUI 基线计数文件 `plugin-definitions-advanced.test.ts` 仅被 `d20fa028` 修改一次;上游 release 也改过该文件,是唯一需要人工合并计数的位置。
8. 代码文件(`cache/key.rs`、`cache/mod.rs`、`ttl.rs`、`persistence.rs`、`ecs_handler.rs`)在 release 合并中未被上游触碰,代码类 cherry-pick 预期无冲突;冲突集中在 docs 与 WebUI。

## 四、拆分代价

1. **必须 cherry-pick 重排,不能直接 rebase**:各子 PR 从 `origin/release/v1.7.0` 切分支,按上表顺序挑取;11 个提交哈希全部改变(patch-id 保持),提交标题可原样复用,无需改写提交信息。
2. **预期冲突位置**(均因文档/WebUI 在合并前基线分叉,非代码耦合):
   - P1:`20f48f70` 基于"含 #6/#7 文档改动"的树,单独挑到 release 时 `resolution.mdx` 中/英大概率冲突;解决原则是与上游 `<PluginAvailability>` 注解并存。
   - P3:`2c8c03b0` 与上游对 `resolution.mdx` 的 59 行改写重叠,可能冲突。
   - P4:`d20fa028` 触及 `docs/metrics.mdx`、`resolution.mdx`(中/英)、WebUI i18n、WebUI defs 与基线测试,全部与上游改动重叠,是本次最重的一组冲突;`770f51a0` 在 `d20fa028` 之后应可干净落盘。
   - 若 P1 未合入而先做 P4,虽不报编译错误,但 `770f51a0` 会本地化尚不存在的 `ecs_scope_mismatch`,`bc838851` 会描述未落地的拒绝逻辑,属语义倒置——故强制 P1 → P4。
3. **单独可编译性**:`c49fdc2b`、`959f558e`、`93797a12`、`9ddc9540`、`44c39246` 自包含(已静态核对,`9ddc9540` 未引用 ECS 新增符号);`ea831344` 依赖 `c49fdc2b`;`d20fa028` 依赖 `9ddc9540`;纯文档提交不参与编译。
4. **校验证据不可复用**:现有"全量 1369 passed / WebUI 6/6"是全系列末态 `44c39246` 的结果;拆分后每个分支需各自执行 `cargo check --all-targets`、相关单测、WebUI 测试、fmt/clippy,不能直接搬用。
5. **PR 正文需重写**:原 `pr-body-cache-hardening.md` 以 11 提交为范围,拆分后需改为单 PR 正文或系列索引;`gh pr create` 的 `--head`、提交表、影响面、测试证据都要按子 PR 调整。

## 五、待定项

1. **4-PR 还是 3-PR**:4-PR 粒度更细、可独立回退 ECS handler 改动;3-PR 少一轮评审,但 P1 约 480 行且跨两个插件目录。当前文档按 4-PR 编写。
2. **`bc838851`/`770f51a0` 是否整提交跟随 P4**:整提交跟随可避免拆分提交、减少改写成本,代价是 P4 夹带少量 ECS 文档/文案;严格拆分则需在 P1 与 P4 间重写这两个提交。默认建议整提交跟随 P4。

确认以上两项后,再建分支、cherry-pick 并逐 PR 处理冲突与校验。
