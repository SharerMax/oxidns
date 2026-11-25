# PR:docs(website): list edns0opt in matcher overview catalogs

**目标分支**:svenshi/oxidns `v1.8.0`
**来源分支**:SharerMax/oxidns `fix/website-edns0opt-overview`(1 个提交 `1feba871`,基于 `origin/v1.8.0` @ `31a011af`)
**创建命令**:

```bash
gh pr create \
  --repo svenshi/oxidns \
  --head SharerMax:fix/website-edns0opt-overview \
  --base v1.8.0 \
  --title "docs(website): list edns0opt in matcher overview catalogs" \
  --body-file pr-doc/pr-body/pr-body-website-edns0opt-overview.md
```

---

## 摘要

上游 `v1.8.0` 合入 `edns0opt` matcher 时新增了 reference 页(`matcher/request.mdx`)与 landing 目录(`matcher.mdx`),但两份 overview 目录遗漏了该插件。`website/scripts/check-content.mjs` 要求每个插件在 overview / landing / reference 三处齐全且中英一致,导致 `npm run build` 失败:

```text
Documentation content checks failed:
- zh-Hans matcher overview: missing edns0opt
- en matcher overview: missing edns0opt
```

本 PR 在两份 overview 的「请求维度 / Request dimensions」表中补上 `edns0opt` 行,链接 `matcher/request.mdx#edns0opt`,内置版本标为 `minimal`(插件无 feature gate),与 landing 页和 reference 页保持一致。

## 合入基线

分支从 `origin/v1.8.0`(`31a011af`)直接切出,只 cherry-pick 修复提交 `1feba871`,不含 merge 提交。该补丁原先附带在 `fix/response-question-case-echo` 的 `e43e2c8a` 中,现拆出为独立 PR。

## 提交

| 提交(本分支) | 内容 |
|---|---|
| `1feba871` | docs(website): list edns0opt in matcher overview catalogs |

## 变更明细

| 文件 | 变更 |
|---|---|
| `website/docs/plugin-reference/overview.md` | 请求维度表新增 `edns0opt` 条目 |
| `website/i18n/en/docusaurus-plugin-content-docs/current/plugin-reference/overview.md` | 英文请求维度表新增对应条目 |

无 Rust 代码、配置、WebUI 组件或 i18n key 变更。

## 影响面

- 仅文档目录页的中英各一行;不改变插件注册、配置契约与运行时行为。
- `edns0opt` 的字段说明仍以 `matcher/request.mdx#edns0opt` 为准,本 PR 只补目录入口。
- 目标分支选 `v1.8.0`:`main` 当前尚无 `edns0opt` reference 页,向 `main` 补该条目会触发内容检查的 unexpected 分支。

## 测试

| 命令 | 结果 |
|------|------|
| `node scripts/check-content.mjs` | 通过(`Documentation content checks passed.`) |
| `npm run build` | 未在本机跑完整 Docusaurus 构建(`website/node_modules` 未安装);失败的 content-check 阶段已单独验证通过 |

> 门禁说明:`docs-ci` 的 `pull_request` 仅覆盖 `main` 与 `release/*`,本 PR 以 `v1.8.0` 为基准,不会触发该 workflow;但补齐目录是 `v1.8.0` 自身内容检查通过的前提。

## 关联

- 拆出来源:`fix/response-question-case-echo` 上的 `e43e2c8a`,已由本独立分支取代。
- 另 PR:`fix/response-question-case-echo-v180`(question 大小写回显修复),与本 PR 无代码依赖。
