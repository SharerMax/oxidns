# PR:feat(forward): drop upstream answers matching bogus_answer_ips

> 目标分支:`svenshi/oxidns:main` · 对应 issue #441 · milestone v1.8.0

## 摘要

国内部分本地解析器会对特定域名返回 `NOERROR + A 0.0.0.0` 形式的污染应答。该应答在现有
`classify_response` 里属于完整正响应,会在 `balanced`、`prefer_positive`、`consensus` 等
选择策略中立即胜出,导致客户端收到无法连接的地址。

本 PR 为 `forward` 增加 opt-in 字段 `bogus_answer_ips`:命中规则的上游应答在分类与选择之前
被**整条丢弃**,不成为候选、投票或排名条目。单上游配置同样生效,命中后 `forward` 返回错误,
不带 response,可由 `fallback` 接管。未配置时行为与现状完全一致。

设计记录见 `pr-doc/design/design-forward-bogus-answer-drop.md`。

## 行为变化

- **直接丢弃,不降权**:保留段应答不具备可用性;降权策略在"无更好结果"时仍会把 `0.0.0.0`
  交给客户端,issue 的终端症状不变。
- **单上游也丢弃**:命中后 `forward` 返回错误、不写 response;客户端得到 SERVFAIL,更容易
  触发重试或交给 `fallback`。这是一次显式的 opt-in 行为,只有配置了该字段才会发生。
- **全部上游命中则失败**:`forward` 返回带丢弃数量的错误;未命中时仍返回上游原始报文,
  TTL / RCODE / EDNS / DNSSEC 都不变。
- **`fastest` 同样过滤**:丢弃发生在选择之前,`fastest` 不会把污染应答当成"第一个成功响应"。
- **缓存不受影响**:丢弃后没有 response,正缓存与负缓存都不会写入。
- **默认关闭**:`0.0.0.0/8` 也是部分上游的广告拦截应答,默认开启会静默翻转既有裁决语义。
  使用上游 `0.0.0.0` 做拦截的部署请不要启用,或不要把 `0.0.0.0/8` 放入列表。

## 实现

- `src/plugin/executor/forward/bogus.rs`:新增 `BogusAnswerFilter`,编译内联 IP/CIDR 与
  `&file` 规则,在 `init` 阶段解析 `$ip_set` provider 并校验 `supports_ip_matching()`;
  判定只查 response answer 区的 A/AAAA。
- `selection.rs`:`SelectionState` 在 `record_response` 之前先做 bogus 判定,命中不计候选、
  不计负向投票、不启动 100ms 负应答 grace;`fastest` 也跳过。`SelectionOutcome` 带
  `bogus_dropped`,供调用方计数与生成错误文案。
- `single.rs` / `concurrent.rs`:两条路径共用同一个过滤器;单上游命中记录上游成功延迟、
  插件错误和 bogus 指标后返回错误;多上游全部命中时返回
  `all responses contained bogus addresses (<n> dropped)`。
- `factory.rs`:构建一次过滤器注入两条路径,并通过 `get_dependency_specs` 声明
  `args.bogus_answer_ips[i]` → provider 的依赖边,保证启动图与类型校验一致。
- `metrics.rs`:新增插件级 counter `forward_bogus_answer_total{plugin_tag}`。
- 规则解析复用 `src/plugin/matcher/rules/` 的 `parse_ip_rules_and_set_tags` 等既有能力,
  并把 `parse_rule_list_value` 参数化为带字段名的 `parse_rule_list`,错误信息带
  `bogus_answer_ips` 与元素索引。

随机境外假 IP 静态不可判别,不在本 PR 范围;用户可把自己已知的伪造前缀加入规则列表。

## 配置影响

```yaml
- tag: forward_cn
  type: forward
  args:
    response_selection: balanced
    bogus_answer_ips:
      - "0.0.0.0/8"
      - "127.0.0.0/8"
      - "$reserved_ips"
```

- 新字段 `bogus_answer_ips`,类型 `string | array<string>`,必填:否,默认:关闭。
- 单个 IP、CIDR、`$ip_set` 引用、`&规则文件`;字符串内可按逗号/空白拆成多条。
- 字段存在但解析后既无内联规则也无 `$ip_set` 引用 → 启动失败。
- 非法 CIDR、缺失规则文件、不存在的 provider tag → 启动失败,错误信息带字段名与索引。
- 非 IP 能力的 provider 引用 → 启动失败。
- `quick_setup` 不提供该字段;无配置 schema 迁移,无持久化格式变化。

## 测试

- `src/plugin/executor/forward/bogus.rs`:内联规则、字符串/列表拆分、规则文件、answer 区限定、
  任一地址命中、provider 命中与能力校验、非法 CIDR、空规则、非字符串元素、依赖提取。
- `src/plugin/executor/forward/tests.rs`:单上游命中丢弃 / 未配置保留;多上游 `balanced`、
  `fastest`、`consensus` 跳过 bogus;全部 bogus 返回带丢弃数量的错误;配置解析与空规则校验;
  指标计数。
- `tests/plugin_integration.rs::test_forward_drops_provider_bogus_answer_and_falls_back`:真实
  UDP 上游 + `ip_set` provider,主分支返回 `0.0.0.0` 被丢弃后由备用分支返回真实地址。

已运行的命令:

```
cargo test
cargo check --no-default-features --features minimal
cargo check --no-default-features --features standard
cargo +nightly fmt --all --check
cargo +nightly clippy --all-targets --all-features
npm run typecheck   # webui
npm test            # webui, 269 passed
npm run lint        # webui
```

已知预先存在问题(与本次改动无关,main 上即可复现):`src/infra/network/dial.rs` 的 clippy
`needless_bool` 警告、未使用的 `tar` 依赖警告。

## 文档跟进

- 中英 `website/docs/plugin-reference/executor/resolution.mdx` 增加字段说明、选择策略与
  `fallback` / `0.0.0.0` 拦截冲突提示,并补充新指标。
- WebUI forward 卡片增加 `bogus_answer_ips` 高级字段与规则文件 / provider 引用选择器,
  同步中英 locale 与插件字段文档、指标标签。
- README 与 `config*.yaml` 不改:默认配置与能力摘要未变。
