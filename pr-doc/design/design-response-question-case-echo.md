# 设计:响应回显当前请求的 question 大小写

状态:已实现,PR 分支 `fix/response-question-case-echo`(提交 `9e009eee`,基于 `origin/v1.8.0` @ `31a011af`,由原实现 `31240490` cherry-pick 得到并已强推替换旧 4 提交版本);别名分支 `fix/response-question-case-echo-v180` 指向同一提交
需求来源:代码审查自查(forward/cache 路径的响应 question 未回显客户端原始大小写)
基线:`origin/main` @ `b8d30262`(v1.7.1 之后)
范围:`src/plugin/server/request.rs`、`src/plugin/test_utils.rs`、`crates/proto/src/name.rs`(测试)、`src/plugin/executor/cache/mod.rs`(测试)
取代:无(首版)

## 0. 背景与诉求

`Name` 同时持有两份表示:wire 字节保留客户端/上游的原始大小写,规范 presentation
为小写(`crates/proto/src/name.rs:563`);`PartialEq`/`Hash` 基于 presentation
(`name.rs:618-630`),因此 DNS 名字比较大小写不敏感。这一模型本身正确,但出站
响应没有利用它回显请求:

- 缓存 key 使用 `question.name().normalized()`(小写)构造
  (`src/plugin/executor/cache/key.rs:114-128`),`WWW.Example.COM` 与
  `www.example.com` 共用同一条目;
- 缓存命中直接克隆入库时的 `Message`
  (`restore_cached_message` → `clone_with_id_and_record_ttl`,
  `src/plugin/executor/cache/mod.rs:816`),question 保留入库时的拼写;
- `forward` 单上游把上游响应原样 `set_response`
  (`src/plugin/executor/forward/single.rs:62`);
- 上游探测对 question 的校验是 `eq_ignore_ascii_case`
  (`src/infra/network/upstream/probe.rs:1182`),`classify_response` 的
  question 比较也经由大小写不敏感的 `Name`
  (`src/core/response.rs:63`),因此"上游把小写 qname 原样回显"能通过全部校验。

结果:客户端发送 `WWW.Example.COM.`,拿到的响应 question 可能是
`www.example.com.`。RFC 1035 §4.1.2 要求响应 question 区复制查询内容;按文本
比较 question 的客户端、诊断工具,以及自行随机化 qname 大小写(0x20 风格)的
客户端都会看到不一致。

诉求:**每个出站 DNS 响应都回显本次请求的 question 大小写**,覆盖所有传输
(UDP/TCP/DoT/DoH/DoQ)、forward 直连、缓存命中、插件合成响应与错误响应。

## 1. 核心决策

| 决策点 | 结论 | 理由 |
|---|---|---|
| 修复位置 | `RequestHandle::finalize_response`(server 出站唯一收口) | 所有传输与 executor 路径都经过它;一次修复覆盖全部来源 |
| 修复方式 | 按索引把响应 question 的 `Name` 替换为请求的 `Name` 句柄 | `Name` 是 `Arc<NameInner>`,克隆只加引用计数,无字节拷贝/分配 |
| 数量不一致 | 用请求的 question 区重建响应 question 区 | 上游可能完全省略 question;重建比逐条补洞简单且无歧义 |
| 名称语义不同 | 不覆盖,仅当 name 大小写等价且 qtype/qclass 相同时对齐 | 语义不同的 question 属于上游/插件异常,应保留证据交由既有校验处理 |
| 缓存层是否改动 | 不改存储、不改 key、不改命中路径 | 存储时的拼写无法预知后续查询;出站回显是唯一正确层次 |
| forward 层是否改动 | 不改 | 上游响应在 forward 内仍是原始证据;回显属于 server 出站职责 |
| 规范化方向 | 跟随请求,不强制小写 | 强制小写违反 RFC 回显要求,且会破坏 0x20 风格客户端 |
| `Name::to_fqdn` | 不改,保持小写规范表示 | matcher、缓存 key、日志都依赖规范小写;展示层不应承担回显职责 |
| 配置开关 | 无 | question 回显是协议正确性,不应可选 |
| 0x20 防伪 | 不在本次范围 | 本次只回显请求已有大小写,不生成随机大小写、也不校验上游回显 |

**被拒方案**

- *在缓存插入时把 question 规范化为小写*:命中时仍要按当次请求改写,等于把
  问题后移一层;且 forward 直连路径完全不受益。
- *只在缓存命中路径修复*:forward、DoH/DoQ 等共享 `handle_request` 的路径
  仍会漏;修复点应选在所有路径的交集。
- *在 `Message::init_response` / `Message::response` 修复*:上游响应由 wire
  解码得到,不经过这两个构造函数。
- *让 `Name::to_fqdn`/`Display` 保留原始大小写*:presentation 是匹配、缓存
  key、日志与 WebUI 的规范形式,改动影响面远大于本问题。
- *无条件用请求 question 覆盖响应*:会掩盖上游/插件的语义性 question 错误,
  也违背"只修大小写"的最小职责。
- *增加配置项*:没有合理的关闭场景。

## 2. 配置契约

无。无 `args`、无 WebUI 定义、无 metrics、无 provider 依赖;行为对所有部署
默认生效,不改变任何现有配置语义。

## 3. 后端设计

### 3.1 现状:Name 的双表示与编码

- wire 字节保留原始大小写;presentation 小写(`name.rs:563`),`as_str`/
  `to_fqdn`/`Display` 均返回小写规范形式(`name.rs:238-265`)。
- 编码器写出 wire label 字节(`crates/proto/src/wire/codec.rs:643`),
  压缩指针也按 wire 字节后缀匹配,因此替换 `Name` 句柄即改变最终报文字节,
  无需改动 codec/compression。
- `Question::set_name` 是公开 API(`crates/proto/src/question.rs:46-48`),
  `Message::questions_mut` 返回 `&mut Vec<Question>`
  (`crates/proto/src/message.rs:477`),可原地替换而不克隆整个 question 区。

### 3.2 出站收口(`src/plugin/server/request.rs`)

`finalize_response`(`request.rs:171`)在所有响应构建完成后、写入 socket 之前
调用新增的 `restore_question_echo`(`request.rs:193`):

```rust
fn restore_question_echo(request: &Message, response: &mut Message) {
    let request_questions = request.questions();
    let response_questions = response.questions_mut();

    if response_questions.len() != request_questions.len() {
        response_questions.clear();
        response_questions.extend(request_questions.iter().cloned());
        return;
    }

    for (response_question, request_question) in
        response_questions.iter_mut().zip(request_questions)
    {
        if response_question.name() == request_question.name()
            && response_question.qtype() == request_question.qtype()
            && response_question.qclass() == request_question.qclass()
        {
            response_question.set_name(request_question.name().clone());
        }
    }
}
```

- 常见路径(响应 question 与请求大小写等价)只做三次比较 + 一次 `Arc` 克隆,
  不分配、不加锁、不阻塞。
- `questions_mut` 是 `Vec` 的可变借用,不会整体 clone。
- 只有 question 数量不一致(上游省略 question 等异常)才 `clear` + `extend`,
  这是唯一分配路径。
- `qtype`/`qclass` 一并比较,避免覆盖插件为特殊语义生成的异型 question;
  名称语义不同的 question 原样保留,交由 `classify_response`/上游探测的既有
  校验处理。
- 该函数对已经是正确拼写的响应幂等,因此早退路径(非 Query opcode、BADVERS、
  Refused)由 `Message::response` 生成后再次经过它也没有副作用。

### 3.3 行为矩阵

| 输入状态 | 输出 |
|---|---|
| 响应 question 与请求仅大小写不同 | 回显请求的大小写 |
| 响应 question 缺失(0 条),请求 1 条 | 重建为请求的 question 区 |
| 响应 question 数量多于请求 | 截断为请求的 question 区 |
| 响应 question 名称语义不同 | 保持不变 |
| 响应 question 的 qtype/qclass 不同 | 保持不变 |
| 请求含多条 question | 按索引逐条对齐 |
| 非 Query opcode / 非法 ingress | 响应本就由 `Message::response` 克隆请求 question,结果不变 |
| 缓存命中 | 回显当次请求的大小写,而非入库时的拼写 |
| UDP / TCP / DoT / DoH / DoQ | 共享 `handle_request`,统一生效 |

### 3.4 DNS 语义影响

- **RFC 1035 §4.1.2**:响应 question 区回显查询内容,本修复补齐该要求。
- **DNSSEC**:question 区不参与 RRSIG 签名,AD 位与校验结果不受影响。
- **压缩**:编码发生在 finalize 之后,压缩表按最终 `Name` 重建,不存在悬空
  指针或字节长度不一致问题。
- **缓存**:条目内容、TTL、负缓存、懒刷新语义均不变;同一 key 的不同大小写
  查询各自回显本次请求的拼写。
- **0x20**:上游探测使用大小写不敏感比较,本修复不增加上游回显校验,因此
  不提供 0x20 防伪能力;但客户端自带随机大小写时,响应会原样回显该大小写。
- **日志/指标**:无变化,日志仍打印规范小写形式。

### 3.5 热路径与资源

- 每响应一次开销为 O(question 数),实践中即常数 1:两个枚举/类比较 +
  一次 `Arc` 引用计数自增。
- 无堆分配(异常数量不一致除外)、无锁、无 IO,不影响 upstream 池与连接复用。
- 不触碰 answer/authority/additional 区,不改变响应大小(question 名称长度
  与请求一致,压缩后字节数与上游响应可能略有差异,属预期)。

## 4. 测试方案

| 测试 | 位置 | 覆盖 |
|---|---|---|
| `name_equality_and_hash_ignore_ascii_case` | `crates/proto/src/name.rs` | `Name` 相等性与 `HashSet` 查找大小写不敏感 |
| `test_handle_request_restores_request_question_case_from_upstream` | `src/plugin/server/request.rs` | 模拟上游返回小写 question,最终响应回显请求的 `WWW.Example.COM.` |
| `finalize_response_rebuilds_missing_question_section` | `src/plugin/server/request.rs` | 响应缺失 question 时按请求重建,并确认 RA 位 |
| `cache_hit_restores_current_request_question_case` | `src/plugin/executor/cache/mod.rs` | 入库 question 为小写、本次请求为混合大小写时命中并回显本次拼写 |

- 断言必须读取编码后的 wire 字节:`Name::to_fqdn`/`Display` 均折叠为小写,
  直接断言会得到假阴性。共享 helper `encoded_question_fqdn` 放在
  `src/plugin/test_utils.rs`,从报文第 12 字节起逐标签还原 qname。
- 现有 server 集成测试(`tests/plugin_integration.rs`,合入 v1.8.0 后 116 项)覆盖 UDP/TCP
  真实收发,未新增集成用例:本修复的输入输出已在 `handle_request` 单测中
  完整覆盖,集成层不引入额外分支。

验证命令与结果:

```text
cargo test                                                   # 全绿,含 116 项集成测试
cargo +nightly fmt --all -- --check                          # 通过
cargo +nightly clippy --all-targets --all-features -- -D warnings  # 通过
```

说明:合入上游 `v1.8.0`(含 #450 对 `dial.rs` 的修复)后,整仓 `clippy -D warnings`
与 `fmt --check` 均通过;此前 `main` 上的 `clippy::needless_bool` 阻塞已消除。

## 5. 同步面清单

| 表面 | 动作 |
|---|---|
| `config*.yaml` / 配置 schema | 无变更 |
| `webui/lib/plugin-definitions/` 与 locale | 无变更 |
| `website/docs/` | 无变更(属协议正确性,不是可配置行为) |
| `README*.md` / `src/build_info.rs` / `Cargo.toml` | 无变更 |
| `docs/ai/*` | 无变更(不改变架构边界或工作流) |
| `pr-doc/design/` | 新增本文档 |

## 6. 后续扩展(本次不做)

1. **0x20 编码校验**:对上游查询生成随机大小写并在响应中严格校验,失败按
   伪造处理。需要同时改动 upstream 查询构造、`probe.rs` 校验与缓存交互
   (缓存命中没有上游校验);与"回显请求大小写"需明确叠加顺序。
2. **缓存条目的 question 规范化**:可选地在入库时统一为小写以减少存储差异;
   出站回显仍不可省,收益有限。
3. **插件级 question 重写标记**:若将来出现合法的"响应 question 与请求不同"
   场景(如别名重写),需要显式标记以绕过对齐条件;当前仅在名称大小写等价
   且 qtype/qclass 相同时替换,已足够保守。

## 7. 决策记录

- 修复放在 server 出站层(`finalize_response`),而非 cache/forward/proto:
  单一收口,覆盖所有传输、所有 executor 与错误响应。
- 只对齐大小写差异;语义不同的 question 保留原样,避免掩盖真实错误。
- 不增加配置开关、metrics 或 WebUI 表面。
- 测试断言基于 wire 字节,而非小写化的 presentation。
- 实现提交:`31240490 fix(server): echo the request's question case in responses`;以 v1.8.0 为基准的 PR 分支由同一补丁 cherry-pick 得到 `9e009eee`。
