# PR:feat(plugin): add edns0opt matcher for EDNS0 option presence

**目标分支**:svenshi/oxidns `main`
**来源分支**:SharerMax/oxidns `feat/edns0opt-matcher`(1 个提交,基于 `origin/main` @ `ad2046d3`)
**创建命令**:

```bash
gh pr create \
  --repo svenshi/oxidns \
  --head SharerMax:feat/edns0opt-matcher \
  --base main \
  --title "feat(plugin): add edns0opt matcher for EDNS0 option presence" \
  --body-file pr-doc/pr-body-edns0opt-matcher.md
```

---

## 摘要

新增 `edns0opt` matcher 插件,按 OPT 记录中 EDNS0 option code 的存在性匹配:

1. **匹配语义**——请求侧(`resp: false`,默认)读请求 OPT,`resp: true` 读响应 OPT(上下文尚无响应时返回 `false`)。多个 code 时 `require: any`(默认)/`all`。只读匹配,不 clone `Message`,不检查 option data,也不做 code 别名。
2. **配置解析拒绝空 `codes` 列表**,在初始化期直接报错;支持 quick-setup 形式 `edns0opt all resp 8,12`。
3. **配套**:matcher 注册(`src/plugin/matcher/{edns0opt.rs,mod.rs}`)、WebUI 插件定义、zh-CN/en-US i18n 文案(`plugin-defined.ts` / `docs.ts`)、中英文 matcher 参考文档。

命名与既有 `forward_edns0opt` executor 命名族保持一致,code 读取复用 proto 的 `EdnsOption::code()`。

## 合入基线

分支从 `origin/main`(`ad2046d3`)直接切出,不含 merge 提交;相对目标分支的 diff 仅为下列 1 个提交。

## 提交

| 提交(本分支) | 源分支提交 | 内容 |
|---|---|---|
| `3a6bf04d` | `6fa9d32e` | feat(plugin): add edns0opt matcher for EDNS0 option presence |

## cherry-pick 适配(相对源提交的偏差)

源提交 `6fa9d32e` 原基于不同的上游基线。以 main 为基线 cherry-pick 时出现 3 处冲突,处理如下:

1. **main 上不存在的 executor 文件(modify/delete)→ 保持删除**:源提交对该文件顺带的 5 行改动只是文档换行重排与改用 `option.code()`(main 的 proto 已具备),matcher 本身不依赖该文件,故不带入。
2. **`website/docs/plugin-reference/matcher.mdx`(中英两份)→ 双方并集**:main 侧新增的 `has_cname` 与本提交新增的 `edns0opt` 同时保留,锚点 span 补入 `<span id="edns0opt">`。
3. **`webui/lib/plugin-definitions-advanced.test.ts` → 计数 68 → 69**:main 基线为 68(含 `has_cname`),加本 matcher 后为 69;源提交侧的 67 是其旧基线 66 + 1,不适用。

适配后 `cargo test --lib` 全量通过,WebUI 注册表完整性测试通过,无冲突残留标记。

## 影响面

- **配置与 WebUI**:新增 matcher 类型 `edns0opt`;matcher 类别不做特性门控,所有 bundle 可用(与 `qname`、`has_cname` 同级,见 `Cargo.toml` minimal 注释)。WebUI 插件注册表 68 → 69,相关定义与中英文案同步。
- **热路径**:匹配为对 OPT options 的线性扫描 + `AHashSet` 查询,初始化期把 `codes` 收敛为 set,无 per-request 分配/clone/锁;无 OPT 或无响应时直接短路返回。
- **运行时行为**:纯谓词,无副作用;未配置该 matcher 时对默认行为零影响。
- **文档**:中英 `matcher.mdx` 目录页与 `request.mdx` 新增 `edns0opt` 章节;文档中引用的插件均以 main 现有插件为限。

## 测试与校验

在 `feat/edns0opt-matcher`(`3a6bf04d`)上执行:

| 命令 | 结果 |
|------|------|
| `cargo test`(lib + integration) | **1416 passed / 0 failed / 4 ignored**(oxidns lib 1301 含 edns0opt 11 项、integration 115) |
| `cargo +nightly fmt --all --check` | 通过 |
| `cargo +nightly clippy --all-targets --all-features -- -D warnings` | 见下方说明 |
| `webui`: `vitest run lib` | **20 文件 / 146 passed**(含 advanced 注册表计数 69) |

> clippy 说明:本机为 Windows,唯一告警是 `src/infra/network/dial.rs:483` 的 `needless_bool`——**main 上既有**(`cfg(not(unix))` 分支,该文件未被本 PR 修改),Linux CI 不触发。

## 已知事项与回退

- 回退:matcher 为显式配置才生效,可在注册表(`matcher/mod.rs`)与 WebUI 定义中整体摘除,无状态迁移或持久化影响。
