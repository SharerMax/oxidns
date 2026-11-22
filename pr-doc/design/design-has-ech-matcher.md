# 设计:`has_ech` 响应阶段 ECH matcher

状态:设计定稿,未实现
需求来源:用户提出(禁用 ECH / ECH 感知路由场景的检测侧能力)
基线:分支 `main` @ `ad2046d3`
范围:`crates/proto/src/message.rs`、`crates/proto/src/rdata/service.rs`、`src/plugin/matcher/has_ech.rs`(新增)、`src/plugin/matcher/mod.rs`、`tests/plugin_integration.rs`、`webui/lib/plugin-definitions/matcher.ts`、`website/docs/plugin-reference/matcher/response.mdx` 及英文 i18n
取代:无(首版)

## 0. 背景与诉求

ECH(Encrypted Client Hello)配置通过 DNS HTTPS/SVCB 记录分发:`SvcParam`
key 5(`ech`)承载 ECHConfigList,定义于 [RFC 9460 §7.3](https://www.rfc-editor.org/info/rfc9460),
`ech` SvcParamKey 的完整语义由 [RFC 9848](https://www.rfc-editor.org/info/rfc9848) 规定。

OxiDNS 当前只能通过 `qtype` matcher 感知"客户端查询了 HTTPS 记录",无法感知
"响应里真的携带了可用的 ECH 配置"。该能力是若干运维场景的检测侧前提:

- **禁用 ECH**:检测到响应携带 ECH 配置后,交给(二期)`strip_ech` executor
  剥离 ech 参数,客户端回退明文 SNI——监听侧过滤/审计环境的标准做法;
- **ECH 感知路由/打标**:对返回 ECH 配置的域名走特定上游、打 mark 或记录;
- **诊断**:与 `rdata_json` 已有的 `ech_base64` 输出互为补充。

诉求:新增一个零配置、响应阶段的布尔 matcher,语义为"当前响应的 answer 区
是否存在携带可用 ECH 配置的 HTTPS/SVCB 记录"。

## 1. 核心决策

| 决策点 | 结论 | 理由 |
|---|---|---|
| 类型名 | `has_ech` | 与 `has_resp`/`has_cname`/`has_wanted_ans` 零配置响应存在性 matcher 命名家族一致 |
| 匹配阶段 | 响应阶段,无响应返回 `false` | ECH 配置只存在于响应;与 `rcode`/`resp_ip`/`has_resp` 先例一致 |
| 扫描范围 | 仅 answer 区 | 与 `resp_ip` 先例一致;CNAME 链的最终响应会把 HTTPS 记录放进 answer 区 |
| 记录类型 | HTTPS(65) 与 SVCB(64) 都匹配 | RFC 9460 中 HTTPS 是 SVCB 的特化,两者都承载 ech |
| ech 值空判定 | 空值(key 5 存在但零长度)判 `false` | RFC 9848 规定空 ECHConfigList 表示"ECH 明确禁用",客户端视同不支持;"曾经有 ECH"信号若需匹配,改动仅一处 `is_empty()`(§4 决策记录) |
| AliasMode(priority=0) | 统一按 presence 处理,不特判 | RFC 9460 下 AliasMode 记录的 SvcParams 客户端不消费,但真实世界 AliasMode 带 ech 极罕见;严格化只需一行 `priority != 0`,v1 换 simplicity,doc comment 写明 |
| 配置形态 | 零配置,`args`/`quick_setup` 参数一律拒绝 | 对齐 `has_resp` 先例;取反走 `MatcherRef` 的 `!` 前缀;未来加 rules 再扩展 |
| 谓词位置 | proto 层新增 `Message::has_answer_ech()` | 对齐 `has_answer_ip`(message.rs)的 `has_answer_*` 家族;matcher 极薄,二期 `strip_ech` 与诊断路径可复用 |
| feature gate | 无 | 无新 Cargo 依赖,proto 恒编译,小谓词;符合 plugin-dev.md 免门禁标准 |

**被拒方案**

- *类型名 `ech`*:短名先例(`rcode`/`cname`)成立,但本插件与 `has_resp`
  一样是零参数存在性谓词,`has_ech` 命名家族更准确,避免将来若引入"值级
  ECH 匹配"时占用短名。
- *`resp_ech`*:`resp_ip` 的 `resp_` 前缀用于与 `client_ip` 区分,这里无
  歧义,前缀冗余。
- *rules 化 args(如 `present`/`empty` token)*:negation 已由 `!` 前缀覆盖,
  空 ech 语义已定死为 `false`,v1 无第二维度,token 是过度设计。
- *matcher 内自持匹配逻辑*:放弃 proto `has_answer_*` 家族先例,二期
  `strip_ech` 需要重复实现判定,违背就近复用。
- *同时扫 additional 区*:与 `resp_ip` 先例不一致;ECH 配置的实际消费场景
  在 answer 区,additional 命中噪声大于价值。

## 2. 配置契约

```yaml
plugins:
  - tag: has_ech_main
    type: has_ech

sequence:
  - tag: seq_main
    type: sequence
    exec:
      - forward_main
      - [has_ech_main, log_ech]    # 可复用 tag 形式
      # - has_ech                  # 快捷语法:裸 `has_ech` 走 quick_setup,无参数
      # - "!has_ech_main"          # 取反:响应不含可用 ECH 配置
```

- `type: has_ech`,无 `args` 字段;`args` 非空、quick_setup 携带非空参数均
  启动失败(对齐 `has_resp` 的错误路径)。
- 无运行时状态、无依赖 provider、无 metrics。

## 3. 后端设计

### 3.1 proto 层:`Message::has_answer_ech()`(`crates/proto/src/message.rs`)

- 与 `has_answer_ip` 并列新增,语义:answer 区任一记录 `rr_type` 为
  SVCB/HTTPS,且其 SvcParams 含 key 5 且值非空。
- key 常量:在 `rdata/service.rs` 定义 `pub const SVCB_KEY_ECH: u16 = 5;`
  (当前 `from_wire` 的 match arm 用字面量 `5`,常量落地后该 arm 引用常量)。
- 判定实现(模块内私有 free fn,`has_answer_ech` 与二期 `strip_ech` 共用):

```rust
fn svcb_has_ech(svcb: &SVCB) -> bool {
    svcb.params().iter().any(|p| p.key() == SVCB_KEY_ECH && !p.value().is_empty())
}
```

- 依赖现状:`SvcParamValue::from_wire` 对 key 5 不做空值校验
  (`service.rs:300`),空值解析为 `Ech([])`,空与非空的区分在本谓词完成,
  proto 解析行为不变。
- 不读取 `SvcParam::parsed()` 而读 `key()` + `value()`:判定只需要"key 存在
  且非空",与 `parsed()` 解析结果(空 ech 也是 `Ech`,不会落 `Unknown`)
  等价,但语义直白且不依赖枚举形状。

### 3.2 matcher 插件(`src/plugin/matcher/has_ech.rs`)

- 骨架对齐 `has_resp.rs`:SPDX 头、模块 doc(用途/配置形态/依赖预期/生命周期/
  热路径行为)、`#[plugin_factory("has_ech")]` 的 `HasEchFactory`、
  `Plugin`(tag/init/destroy 全空实现)、`Matcher` 实现:

```rust
impl Matcher for HasEchMatcher {
    #[hotpath::measure]
    fn is_match(&self, context: &mut DnsContext) -> bool {
        context.response().is_some_and(Message::has_answer_ech)
    }
}
```

- 工厂错误路径:`create` 时 `args` 非空报
  `"has_ech does not accept arguments"`;`quick_setup` 携带非空参数报
  `"has_ech does not accept parameters"`(文案对齐 `has_resp`)。
- 热路径:wire 解码时 SvcParams 已结构化,`is_match` 是纯内存扫描、命中即
  退出,无分配/锁/阻塞 I/O;`#[hotpath::measure]` 标注。
- 注册:`src/plugin/matcher/mod.rs` 加 `pub mod has_ech;`(无 cfg 门禁)。

### 3.3 语义矩阵(实现即文档,测试逐一对应)

| 输入状态 | 结果 |
|---|---|
| 无响应(`response` 为 None) | `false` |
| answer 区 HTTPS(65) 记录,ech 值非空 | `true` |
| answer 区 SVCB(64) 记录,ech 值非空 | `true` |
| ech key 存在但值空(`Ech([])`) | `false` |
| HTTPS 记录仅 alpn/ipv4hint 等,无 ech key | `false` |
| A/AAAA/其他记录类型 | `false` |
| ech 仅出现在 additional 区 | `false` |
| CNAME-only 响应(无 HTTPS 记录) | `false` |
| AliasMode(priority=0) 带 ech | `true`(v1 不特判,见 §1) |

### 3.4 DNS 语义影响

- 匹配是纯读操作,不触碰 TTL、负缓存、DNSSEC;响应字节不变。
- CNAME chase:`forward` 的最终响应中 HTTPS 记录随链进入 answer 区,matcher
  在 sequence 中位于 forward 之后即可命中,无需自行 chase。
- 缓存交互:matcher 在缓存命中路径同样有效(缓存的是完整 Message);二期
  `strip_ech` 才需要关注"剥离后是否回写缓存"的顺序问题。

## 4. 测试方案

- **proto 单元测试**(`message.rs` / `service.rs` 测试区):
  - `has_answer_ech` 矩阵(§3.3 前 7 行,AliasMode 行单独标注);
  - SVCB/HTTPS 编解码 roundtrip 保留 ech 原始字节(含空 ech 值 roundtrip)。
- **matcher 单元测试**(`has_ech.rs` 内 `#[cfg(test)]`,对齐 `resp_ip.rs` 风格):
  - 无响应 → false;HTTPS 带 ech → true;SVCB 带 ech → true;
  - 空 ech 值(key 5,零长度)→ false——手工构造 wire 数据,验证空值不被
    proto 落到 `Unknown` 且谓词判 false;
  - 工厂层:`create` 带 args 报错、`quick_setup` 带参数报错。
- **集成测试**(`tests/plugin_integration.rs`,无 feature gate):
  - `has_ech` 注册 + quick-setup 解析 + sequence 内分支行为。

## 5. 同步面清单

| 表面 | 动作 |
|---|---|
| `webui/lib/plugin-definitions/matcher.ts` | 新增 entry:`kind: "has_ech"`,`configSchema: []`,结构镜像 `has_resp` 条目 |
| `website/docs/plugin-reference/matcher/response.mdx` + i18n en 对应页 | 新增小节:行为、语义矩阵、示例(含 `!` 取反) |
| `Cargo.toml` / `src/build_info.rs` / `README*.md` / `config*.yaml` | 无变更 |

## 6. 二期扩展(本次不做,设计留口)

1. **`strip_ech` executor**:基于 `answers_mut()` 移除 key 5 param,复用
   §3.1 的 `svcb_has_ech` 定位逻辑。需评估:DNSSEC(剥离破坏 HTTPS 记录
   RRSIG 语义,文档明确 AD 位含义)、缓存交互顺序(剥离应在缓存写入前/读出
   后统一)、TCP 大记录重编码。
2. **值级匹配**:按 ECHConfigList 内层结构(`version + length-prefixed` 的
   ECHConfig)匹配 `public_name`/`config_id`,需新增内层解析,v1 明确不支持。
3. **兄弟 key**:key 8(ohttp)在 `service.rs` 已解析为 `SvcParamValue::Ohttp`,
   将来按同样模式加 `has_ohttp`,本设计不预设泛化抽象。

## 7. 决策记录

- 类型名定为 `has_ech`(用户确认,2026-11-23;备选 `ech`/`resp_ech` 被拒,
  理由见 §1)。
- 空 ech 值判 `false`、answer-only、谓词放 proto `has_answer_ech()` 均按
  设计推荐执行。
