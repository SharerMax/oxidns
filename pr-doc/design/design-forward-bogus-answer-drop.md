# 设计:forward 丢弃命中虚假地址的上游应答

状态:设计定稿,未实现(对齐 milestone v1.8.0)
需求来源:[issue #441](https://github.com/svenshi/oxidns/issues/441)
基线:分支 `learning-doc` @ `33971b3f`(v1.7.1)
范围:`src/plugin/executor/forward/{bogus,config,factory,single,concurrent,selection,metrics,tests,mod}.rs`、`src/plugin/matcher/rules/{config,mod}.rs`、`webui/lib/plugin-definitions/executor.ts`、WebUI locale、`website/docs/plugin-reference/executor/resolution.mdx` 及英文 i18n
取代:无(首版)

## 0. 背景与诉求

国内部分运营商本地解析器会对"反盗版"类域名返回 `NOERROR + A 0.0.0.0`
(issue 实测:浙江联通 `221.12.1.227` 对 `pan5.net`)。当该解析器作为 `$cn`
的并发上游之一时:

- `src/core/response.rs::classify_response` 看到请求类型存在于 answer 区,判为
  `CompletePositive`;
- `src/plugin/executor/forward/selection.rs` 里 `balanced` / `prefer_positive` /
  `consensus` 都让完整正应答立即胜出,consensus 的负向投票对"正应答形态的污染"
  没有否决权;
- 结果:污染应答被采纳,客户端批量 `ConnectException to 0.0.0.0`。

诉求:**命中虚假地址的上游应答直接丢弃,不参与选择**;单上游配置同样丢弃。
随机境外假 IP(`2001::9df0:xxx` 等)不可静态判别,不在本设计范围。

## 1. 核心决策

| 决策点 | 结论 | 理由 |
|---|---|---|
| 处理位置 | `forward` 插件内部 | 可疑/污染判定是插件业务语义,与 `classify_response`("响应是否回答了问题")解耦;与 issue 讨论结论一致 |
| 处置方式 | **直接丢弃**,不降权、不新增 disposition | 保留段应答不具备可用价值;降权在"无更好结果"时仍会把 `0.0.0.0` 交给客户端 |
| 单上游 | 同样丢弃,`forward` 返回错误 | 单上游配置下污染应答同样无法使用;错误比 `0.0.0.0` 更容易被客户端重试或由 `fallback` 接管 |
| 配置位置 | forward 级 `bogus_answer_ips` | 与上游 tag 解耦,一份规则覆盖该 forward 的全部上游 |
| 配置形态 | 标准 IP 规则列表(字符串或列表,支持 IP/CIDR/`$ip_set`/`&file`) | 复用既有解析与 provider 依赖机制,用户无需学第二套语法 |
| 默认值 | 不配置 = 关闭(opt-in) | `0.0.0.0/8` 也是部分上游的广告拦截应答;默认开启会静默翻转"拦截上游 + 普通上游"的既有裁决 |
| 判定范围 | answer 区任一 A/AAAA 命中即整条丢弃 | 通过 `Message::has_answer_ip` 只检查 answer 区;污染报文通常只有一条地址 |
| `fastest` | 同样过滤 | 丢弃发生在选择之前,`fastest` 也不应把污染应答当"第一个成功响应" |
| 计量 | 插件级 `forward_bogus_answer_total` + 上游级 `forward_upstream_bogus_total{upstream}` | 既能看总量,也能定位持续返回污染地址的上游 |

**被拒方案**

- *降权(排名放到负应答之后)*:实现更小,但无更好结果时仍返回 `0.0.0.0`,issue
  的终端症状不变。
- *新增 `ResponseDisposition::Bogus`*:把污染语义塞进 core 分类器,影响所有
  response 消费者,职责错位。
- *只改 `balanced`*:`prefer_positive` / `consensus` 同样会被正应答形态的污染
  秒赢,问题不会真正消失。

## 2. 配置契约

```yaml
- tag: forward_cn
  type: forward
  args:
    response_selection: balanced
    # 字符串形式:逗号/空白分隔
    bogus_answer_ips: "0.0.0.0/8, 127.0.0.0/8"

    # 或列表形式,支持 provider 引用与规则文件
    # bogus_answer_ips:
    #   - 0.0.0.0/8
    #   - 127.0.0.0/8
    #   - $reserved_ips
    #   - "&/etc/oxidns/bogus.txt"
```

- 类型:`string | array<string>`;必填:否。
- 支持元素:
  - 单个 IP
  - CIDR
  - `$ip_set` 引用
  - `&规则文件`(空行与 `#` 注释由 `TextSource` 处理)
  - 字符串内可再按逗号/空白拆成多条
- 缺省 → 关闭;字段存在但解析后既无内联规则也无 `$tag` → 启动失败,
  文案为 "forward plugin 'bogus_answer_ips' requires at least one IP rule or ip_set tag"。
- 非法 CIDR / 缺失规则文件 / 不存在的 provider tag → 启动失败,错误信息带
  `bogus_answer_ips` 字段名与元素索引。
- 单上游与多上游都解析、都生效;`quick_setup` 不提供该配置。
- 推荐集(文档给出,非内置默认):`0.0.0.0/8`、`127.0.0.0/8`、`::/128`、`::1/128`。
  使用上游 `0.0.0.0` 做广告拦截的部署不要启用,或不要把 `0.0.0.0/8` 放入列表。

## 3. 后端设计

### 3.1 判定组件(`forward/bogus.rs`,新增)

```rust
pub(super) struct BogusAnswerFilter {
    ip_rules: IpPrefixMatcher,
    ip_set_tags: Vec<String>,
    ip_sets: Vec<Arc<dyn Provider>>,
}

impl BogusAnswerFilter {
    /// 启动期解析 inline / `&file` 规则;`None` 返回禁用态。
    pub(super) fn from_value(value: Option<&serde_yaml_ng::Value>) -> Result<Self>;
    /// init 阶段解析 `$tag`,并校验 supports_ip_matching()。
    pub(super) fn init(&mut self, context: &PluginInitContext<'_>, tag: &str) -> Result<()>;
    #[inline]
    pub(super) fn is_bogus(&self, response: &Message) -> bool;
}
```

- `is_bogus`:`response.has_answer_ip(|ip| ip_rules.contains_ip(ip) || ip_sets.iter().any(|s| s.contains_ip(ip)))`,
  只检查 answer 区 A/AAAA,不修改报文。
- `IpPrefixMatcher` 在 `create()` 阶段 `finalize_compact()`,热路径只做整数比较。
- 复用 `src/plugin/matcher/rules/`(`reload_provider.rs` 已有跨类目复用先例):
  - `parse_ip_rules_and_set_tags`、`split_rule_sources`、`provider_tags_from_rules`
  - `provider_dependency_specs`、`resolve_provider_tags`
  - 新增 `pub(crate) parse_rule_list(field, Value)`:把现有 `parse_rule_list_value`
    参数化为带字段名的入口,`parse_rules_from_value` 以 `"matcher args"` 调用,
    forward 以 `"bogus_answer_ips"` 调用。

### 3.2 多上游 selection(`forward/selection.rs`)

- `SelectionState` 新增 `filter: Option<&BogusAnswerFilter>` 与 `bogus_dropped: usize`。
- `record_response` 改为返回 `Option<ResponseClass>`:判定顺序为
  **先过滤、后分类**;命中即 `bogus_dropped += 1` 并返回 `None`,不计正/负票、
  不触发 grace、不参与排名。`completed` 仍照常递增,保证"全部上游已响应"的
  收敛条件不变。
- `select_fastest` 增加同一过滤:命中则继续等待下一个上游,不 `abort_all`。
- `select_response` 返回值从三元组改为:

```rust
pub(super) struct SelectionOutcome {
    pub(super) selected: Option<SelectedResponse>,
    pub(super) last_error: Option<String>,
    pub(super) timed_out: bool,
    pub(super) bogus_dropped: usize,
}
```

- 全部已启动上游都被丢弃时,`selected == None` 且无其他错误:
  `concurrent.rs` 返回
  `forward plugin '<tag>' failed across all concurrent upstreams: all responses contained bogus addresses (<n> dropped)`。
- 现有排名、负应答 grace、consensus 计票逻辑均不改动。

### 3.3 单上游(`forward/single.rs`)

- `SingleDnsForwarder` 持有同一个 `BogusAnswerFilter`。
- `execute`:`upstream.query` 返回 `Ok` 后先判定:
  - 命中:记录上游成功延迟(上游确实响应了)+ `record_bogus_answer()` +
    `record_error()` + 返回
    `DnsError::plugin("forward plugin '<tag>' discarded bogus answer from upstream '<addr>'")`,
    不写 `context.response`;
  - 未命中:沿用现有成功路径。
- `short_circuit` 无影响(无 response)。

### 3.4 装配与依赖

| 文件 | 改动 |
|---|---|
| `config.rs` | `ForwardConfig` 新增 `bogus_answer_ips: Option<serde_yaml_ng::Value>`;`parse_forward_config` 保持返回 `ForwardConfig` |
| `factory.rs` | `create` 构建一次 `BogusAnswerFilter`,按上游数量注入两条路径;`quick_setup` 注入禁用态 |
| `factory.rs` | 新增 `get_dependency_specs`:从 `args.bogus_answer_ips` 提取 `$tag`,用 `provider_dependency_specs("args.bogus_answer_ips", tags)` 声明依赖边 |
| `single.rs` / `concurrent.rs` | `init` 调用 `resolve_providers`,再注册指标 |
| `concurrent.rs` | 把 `&self.bogus` 传入 `select_response`,读取 `SelectionOutcome`,记录丢弃计数与错误 |
| `mod.rs` | 声明 `mod bogus;` |

`supports_ip_matching()` 的校验错误由 forward 自己给出,避免复用 matcher
"matcher '...' requires provider" 的文案。

### 3.5 指标(`forward/metrics.rs`)

新增插件级 counter:

```
forward_bogus_answer_total{plugin_tag}  "Total upstream responses discarded because an answer address matched bogus_answer_ips."
```

新增上游级 counter(标签与既有 `forward_upstream_*` 一致,启动期固定、低基数):

```
forward_upstream_bogus_total{plugin_tag, upstream}  "Total responses from this upstream discarded because an answer address matched bogus_answer_ips."
```

- 多上游:`outcome.bogus_dropped` 累加插件级计数;子任务把 `selected_idx` 随查询结果返回,`SelectionState::discard_if_bogus(idx, ...)` 同时按 `idx` 记上游级计数;
- 单上游:每次命中插件级 +1,并按 `idx = 0` 记上游级 +1;
- 判定仍在 selection 入口收口,不下移到并发子任务:`fastest` / `balanced` / `prefer_positive` / `consensus` 共享同一过滤语义,不污染上游成功/错误指标,也不改变 `completed` 收敛;
- WebUI `plugin-definitions/executor.ts` 的 `metricLabels` / `metricHelp` 同步。

### 3.5.1 诊断日志

- 逐条丢弃(并发路径)用 `debug!`,带插件 tag、上游索引、qname、qtype、命中的 answer IP;默认 info 级别不输出,需要排查时调低日志级别即可看到每条被丢弃的地址。
- 请求真正失败时保留 `warn!`:单上游命中,以及多上游全部命中时的汇总错误。若全 bogus,汇总文案带丢弃数量、qname、qtype 与首个命中的 answer IP;单上游错误同样带这些信息。
- qname/qtype 取自 `request.first_question()`(`normalized()` 不带尾部点),缺失时为 `<none>`;只在丢弃/失败分支计算,不进成功热路径。

### 3.6 缓存与报文语义

- 丢弃后不产生 `response`,正/负缓存都不会写入;
- 未命中时返回的仍是上游原始报文,TTL、RCODE、EDNS、DNSSEC 等不变;
- `bogus_answer_ips` 只影响候选筛选,不修改任何 RR。

### 3.7 热路径

- 未配置:仅一次 `Option` 判空 / `is_enabled()` 布尔判断。
- 已配置:对每个候选应答遍历 answer 区 IP 做前缀匹配,`IpPrefixMatcher` 已编译,
  无分配;`$ip_set` provider 走既有 `contains_ip`。
- 全部在并发选择等待路径上,不在请求解析/编码路径上。

## 4. 行为矩阵

| 场景 | 结果 |
|---|---|
| 存在真实完整正应答 | 真实应答胜出(`fastest` 也跳过 bogus) |
| 其余上游只有确定负应答 | 按现有规则返回负应答(`NXDOMAIN`/`NODATA` 压过被丢弃项) |
| bogus + `NXDOMAIN` | 返回 `NXDOMAIN`(bogus 不参与排名,也不计入负共识) |
| 所有已启动上游都命中 | `forward` 返回错误,客户端 SERVFAIL,`fallback` 可接管 |
| 单上游命中 | `forward` 返回错误,无 response |
| 未配置 `bogus_answer_ips` | 所有行为与现状完全一致 |

模式细节:

- `fastest`:丢弃后继续等待下一个上游的 Message;全部丢弃则失败。
- `balanced`:丢弃不启动负应答 grace;其他上游仍按原有时序竞争。
- `prefer_positive` / `consensus`:丢弃既不算正票也不算负票,不会提前结束或
  触发负共识;等全部已启动上游结束后按现有排名选择。

## 5. 测试计划

单元(`forward/bogus.rs`):

- `0.0.0.0`、`127.0.0.5`、`::`、`::1` 命中;`192.0.2.1` 不命中;
- answer 区之外的地址(additional/authority)不命中;
- 字符串形式、列表形式、逗号/空白拆分、`&file`、`$tag`;
- 非法 CIDR、空字段、缺失规则文件、非 IP provider 的报错。

配置与依赖:

- `parse_forward_config` 缺省 / 合法 / 空值;
- `get_dependency_specs` 产出 `args.bogus_answer_ips[i]`,缺失 tag 在依赖图阶段报错。

运行(`forward/tests.rs`,扩展 `MockUpstream` 支持指定应答 IP):

- 单上游:命中 → `execute` 返回 `Err` 且 `context.response()` 为空;
- 单上游:未配置 → 原样返回 `0.0.0.0`;
- 单上游 + `$ip_set` provider → 命中;
- 多上游 `balanced`:0ms bogus + 20ms 真实 A → 真实 A;
- 多上游 `fastest`:0ms bogus + 20ms 真实 A → 真实 A;
- 多上游 `consensus`:bogus + `NXDOMAIN` → `NXDOMAIN`,且无负共识提前返回;
- 多上游全 bogus → `Err`,错误信息包含 bogus 与丢弃数量、qname/qtype 与命中地址;
- `forward_bogus_answer_total` 计数正确,且按上游的 `forward_upstream_bogus_total` 归属正确(快 bogus + 慢真实:索引 0 记 1,索引 1 记 0;全部 bogus:各记 1);
- `bogus_answer_ip()` 返回 answer 区首个命中地址,未命中返回 `None`;
- 指标导出测试:验证 `forward_upstream_bogus_total` 的 `upstream` 标签与数值。

集成(可选):

- `tests/plugin_integration.rs`:两个本地 UDP 上游(一个回 `0.0.0.0`,一个回
  真实 A)+ `balanced` → 返回真实 A;`fallback` 主分支全 bogus → 备用接管。

## 6. 文档与 WebUI 同步

- `website/docs/plugin-reference/executor/resolution.mdx` 与
  `website/i18n/en/.../executor/resolution.mdx`:
  - 新增 `bogus_answer_ips` 小节(语法、provider 引用、默认关闭、单/多上游都生效);
  - `response_selection` 各模式的"完整正应答立即胜出"改为"非可疑完整正应答";
  - 补充 `fallback` 与 `0.0.0.0` 广告拦截的说明。
- WebUI:
  - `webui/lib/plugin-definitions/executor.ts`:用 `stringArrayField` +
    `providerReferenceArrayItem` / `inputArrayItem`,`advanced: true`;
  - `webui/lib/plugin-definitions-advanced.test.ts` 的 `EXPECTED_ADVANCED_FIELDS` 同步;
  - `webui/lib/i18n/locales/{zh-CN,en-US}/{plugin-defined,docs}.ts` 补充字段与文档文案;
  - forward 指标标签补充 `forward_bogus_answer_total` 与 `forward_upstream_bogus_total`;
  - 中英 `resolution.mdx` 的 per-upstream 指标列表同步补充。
- `README*.md`、`config*.yaml` 不改(默认配置与能力摘要未变)。
- Release note call out:opt-in 新字段;启用后单上游命中即 SERVFAIL;与上游
  `0.0.0.0` 拦截语义的冲突。

## 7. 风险与边界

- **与广告拦截上游冲突**:`0.0.0.0/8` 是常见拦截应答。通过 opt-in + 文档推荐集
  划清语义;需要保留拦截的用户不配置,或不把该段放入列表。
- **单上游直接失败**:可用的替代行为只有 SERVFAIL;文档引导配合 `fallback`。
- **随机境外假 IP**:静态不可判别,本设计不处理;用户可自行把已知伪造前缀加入
  `bogus_answer_ips`(规则列表支持任意 CIDR / provider 列表)。
- **指标上游归属**:已通过 `forward_upstream_bogus_total{upstream}` 补齐,可在不改动选择逻辑的前提下定位持续返回污染地址的上游;判定仍集中在 selection 入口。
- **负缓存语义**:丢弃不产生 response,不会把污染结果写入负缓存;不会改变
  现有 NXDOMAIN/NODATA 缓存行为。

## 8. 实施拆分

单 PR 即可,建议提交顺序:

1. `feat(forward): add bogus_answer_ips rule parsing and provider deps`
2. `feat(forward): drop bogus upstream answers before selection`
3. `feat(forward): apply bogus filtering to single-upstream forwarding`
4. `feat(forward): count discarded bogus answers`(插件级指标)
5. `docs(forward): document bogus_answer_ips`(含 WebUI 定义与 locale)
6. `feat(forward): add per-upstream bogus answer metric`(上游级指标)
7. `log(forward): enrich bogus answer drop diagnostics`(qname/qtype/命中地址;逐条 drop 用 `debug!`)

## 9. 回退

- 删除配置即回到现状;关闭开关 = 不写 `bogus_answer_ips`;
- 代码集中在 `bogus.rs` 与 `selection.rs` / `single.rs` 两个调用点,回退无需迁移;
- 无持久化格式变化,无配置 schema 迁移。
