# PR:refactor(proto): add EdnsOption::code and reuse it in forward_edns0opt

**目标分支**:svenshi/oxidns `release/v1.7.0`
**来源分支**:SharerMax/oxidns `refactor/edns0-option-code`(2 个提交,基于 `origin/release/v1.7.0` `871f8da3`)
**创建命令**:

```bash
gh pr create \
  --repo svenshi/oxidns \
  --head SharerMax:refactor/edns0-option-code \
  --base release/v1.7.0 \
  --title "refactor(proto): add EdnsOption::code and reuse it in forward_edns0opt" \
  --body-file pr-doc/pr-body-edns0-option-code.md
```

---

## 摘要

EDNS0 过滤与匹配系列的第一个 PR,纯重构,行为不变:

1. **新增 `EdnsOption::code()`**——在 `crates/proto` 的 `EdnsOption` 上暴露 option code,内部委托既有的 `From<EdnsOption> for EdnsCode`,不新增分配、不改变编解码路径。
2. **`forward_edns0opt` 改用该方法**——把 5 处 `u16::from(EdnsCode::from(option))` 等价转换替换为 `option.code()`,消除插件层的重复协议转换。

该 PR 为后续 E2(drop_edns0opt executor 与 edns0opt matcher)的前置:两个插件都需要从 `EdnsOption` 读取 code,把这一语义收敛到 proto 可避免在插件层各写一份。

## 合入基线

分支从 `origin/release/v1.7.0`(`871f8da3`)直接切出,不含 merge 提交;相对目标分支的 diff 仅为下列 2 个提交。基线已包含管理 API 系列(#401)、DNS 服务加固系列(#402)及上游 `learn_domain`/hosts 标准格式/query-recorder 改版,本次改动与这些内容无交集。

## 提交

| 提交(本分支) | 源分支提交 | 内容 |
|---|---|---|
| `3b3f5cd1` | `6e5574db` | feat(proto): add EdnsOption::code() helper |
| `d688ab01` | `42eb9377` | refactor(plugin): use EdnsOption::code in forward_edns0opt |

两个 cherry-pick 的 `patch-id` 与源提交逐一比对一致(`cc593d08…`、`48f54d3c…`),无冲突残留。

## 动机

- **协议语义归位**:option code 是 EDNS0 wire 语义,属于 `crates/proto`;插件层只应做策略判断,不应重复 `EdnsCode::from` 转换。
- **去重**:同一转换在 `forward_edns0opt` 内重复 5 处,E2 的两个插件还要各自实现;收敛为一个公开方法后只有一处实现。
- **可复用**:`edns0opt` matcher 的 code 存在性判断、`drop_edns0opt` 的双侧裁剪都直接调用 `code()`,与 `forward_edns0opt` 保持同一语义来源。

## 影响面

- **proto 公开 API**:增量新增 `EdnsOption::code()`,无签名或语义变更;workspace 内现有调用方无需改动。
- **运行时行为**:完全不变。方法体等价于原先内联转换,`forward_edns0opt` 的匹配结果、转发内容与响应构造一致。
- **热路径**:方法为一次字段枚举转换,无 clone/分配/锁;与替换前生成的代码等价。
- **配置与 WebUI**:无配置字段、无 schema、无 i18n、无 docs 变更;WebUI 插件注册表基线不变。

## 测试与校验

在 `refactor/edns0-option-code`(`d688ab01`)上执行:

- `cargo check --all-targets`:**通过**,`Finished dev profile`,1m02s。
- `cargo test --lib edns0`:**7 passed / 0 failed**(该分支只含既有 `forward_edns0opt` 用例;drop/matcher 用例属 E2)。
- `cargo +nightly fmt --all --check`:干净。
- `cargo clippy --lib --all-features -- -D warnings`:干净。
- WebUI `vitest run lib/plugin-definitions-advanced.test.ts`:**2/2 passed**——本 PR 未触碰 WebUI,该结果用于确认基线未漂移。
- 完整 `cargo test --all-features` 未在本分支重复执行;改动为等价重构,聚焦单测与全目标编译已覆盖其影响面。

## 与 E2 的依赖

- E2(`feat/edns0-plugins`,`9ba81e58` + `6fa9d32e`)调用 `EdnsOption::code()`;若 E2 先于本 PR 合入,`6fa9d32e` 无法编译,`42eb9377` 的替换也会回退成局部转换。**合入顺序必须为 E1 → E2**。
- E2 以 `origin/release/v1.7.0` 为基线 cherry-pick 时,在 `docs/**/plugin-reference/overview.md` 与 `webui/lib/i18n/locales/zh-CN/plugin-defined.ts` 出现了文档基线分叉冲突(与本次 proto/plugin 改动无关),将在 E2 的 PR 正文中单独说明处理方式。

## 已知事项与回退

- 本 PR 不携带源分支的其它提交:不含学习笔记、内部设计文档,以及缓存/上游/重载相关改动。
- 两个提交可独立 revert;回退后 `forward_edns0opt` 恢复内联转换,无状态迁移或持久化影响。
