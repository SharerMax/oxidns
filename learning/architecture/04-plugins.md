# 插件目录

> 基线:main v1.7.1(`ad2046d3`) · 2026-11-14

## 插件组合机制

注册:`#[plugin_factory("type")]` 过程宏在编译期把工厂提交进 `inventory` 自注册(少数有状态工厂用 `register_plugin_factory!`);运行时由 `PluginInitContext` 提供 `executor()` / `matcher_ref()` / `provider()` 做跨插件依赖解析;销毁严格按初始化逆序进行。

| 组合方式 | 语法 / 机制 | 说明 |
|----------|-------------|------|
| 实例声明 | `plugins: [{tag, type, args}]` | 全局命名实例;include 文件的插件排在主文件之前;tag 语法 `[A-Za-z0-9_.-]` ≤64,`qs.*` 前缀保留 |
| 引用命名实例 | `$tag` | sequence 的 matches/exec、server 的 `entry:`、dual_selector 的 `probe_executor` 等处复用已声明插件 |
| quick-setup 内联 | `type [param]` | 免声明的就地创建(如 exec 直接写 `forward remote`);合成 `qs.exec.<seq>.<n>.<type>` tag,生命周期归所属 sequence;嵌套依赖以 `owner -> quick_setup(type).field` 并入依赖图 |
| 规则链 | sequence:`matches: [expr]` + `exec` | matches 列表为"与",`any_match` 提供"或";指令按序推进,全部匹配通过才执行 |
| 内建指令 | accept / return / reject [rcode] / jump / goto / mark / set_mark | jump:子链 Stop 穿透结束父链,Next/Return 回父链继续;goto:子链结果即父链结果,剩余指令跳过;mark 追加,set_mark 多值整组替换 |
| 包裹式组合 | with_next 执行器 | cache / redirect / query_summary / metrics_collector 等"包裹"下游:先做前置逻辑,调用 next 继续链,返回后做后处理 |
| 跨类别引用 | matcher · provider 互引 | qname / cname / resp_ip / client_ip 引 domain_set / ip_set;ip_selector、learn_domain 等执行器内部消费 provider;所有 matcher 可加 `!` 前缀取反 |
| provider 选择器 | geosite / geoip 的 `selectors` | v2ray 类别选择语法,仅编译选中的条目(如 geosite.dat 中的 cn 子集),缩小内存占用 |
| 运行时覆盖 | matcher 三态 mode | 默认 / always_true / always_false,经 `POST /plugins/<tag>/mode` 热切换;覆盖先于谓词求值且保留 `!` 取反语义 |
| 启动依赖图 | DependencySpec 字段级边 | 从配置声明与 quick-setup 引用展开,Kahn 拓扑排序决定初始化顺序;无存活依赖者的 provider 跳过并告警 |

## Server ×4

`fn run(&self)`,监听在 `init` 内启动。

| 类型 | 功能 | 源文件 |
|------|------|--------|
| `udp_server` | UDP 监听;每报文独立任务;回包保留原目的地址 | `server/udp.rs` |
| `tcp_server` | TCP 与 DoT(TLS);2 字节长度前缀帧,每连接 mpsc 写者;TCP_NODELAY + keepalive | `server/tcp.rs` |
| `http_server` | DoH(RFC 8484,HTTP/1.1 + h2,可选 h3 与 ALT-SVC)及 JSON API 模式;HttpDispatcher 按 URL path 分发到不同入口 | `server/http/mod.rs` |
| `quic_server` | DoQ:QUIC 双向流,一流一查询 | `server/quic.rs` |

## Matcher ×21

`fn is_match(&mut DnsContext) -> bool`,支持 `!` 取反与 API 运行时覆盖。

| 类型 | 功能 | 源文件 |
|------|------|--------|
| `_true` / `_false` | 恒真 / 恒假 | `true_matcher.rs` / `false_matcher.rs` |
| `any_match` | 子表达式逻辑 OR | `any_match.rs` |
| `client_ip` | 客户端 IP 匹配 CIDR 规则 / ip_set 提供者 | `client_ip.rs` |
| `cname` | 响应 CNAME 目标匹配域名规则 | `cname.rs` |
| `env` | 环境变量条件 | `env.rs` |
| `has_cname` | 当前应答 answer 段包含 CNAME(不论请求 qtype 与响应 rcode) | `has_cname.rs` |
| `has_resp` | 已存在响应 | `has_resp.rs` |
| `has_wanted_ans` | 应答段包含请求的 RR 类型(基于响应分类) | `has_wanted_ans.rs` |
| `mark` | 上下文 mark 集合与配置值相交 | `mark.rs` |
| `ptr_ip` | PTR 查询名中解出的 IP 匹配 | `ptr_ip.rs` |
| `qclass` / `qtype` | 查询类 / 查询类型列表 | `qclass.rs` / `qtype.rs` |
| `qname` | 查询名匹配内联规则 + domain_set 提供者 | `qname.rs` |
| `question` | 提供者级 contains_question 成员判定 | `question.rs` |
| `random` | 概率匹配 | `random.rs` |
| `rate_limiter` | 按掩码客户端 IP 的令牌桶限速 | `rate_limiter.rs` |
| `rcode` | 响应 RCODE 列表 | `rcode.rs` |
| `resp_ip` | 应答 IP 匹配 IP 规则 / ip_set 提供者 | `resp_ip.rs` |
| `string_exp` | 对上下文取值做字符串运算 / 正则 | `string_exp.rs` |
| `time` | 墙钟时间窗匹配 | `time.rs` |

## Executor ×37

`execute(&mut DnsContext) -> ExecStep{Next, Stop, Return}`,with_next 型可包裹下游。

| 类型 | 功能 | 源文件 |
|------|------|--------|
| **解析与链路** | | |
| `sequence` | 规则链引擎:matches → exec 指令流;内建 jump / goto / mark;支持子序列 | `executor/sequence/mod.rs` |
| `forward` | 上游解析:单一上游或并发 fan-out + 选优模式 | `executor/forward/factory.rs` |
| `load_balance` | 多执行器按 round-robin(默认)/ random / weighted-random 择一执行 | `executor/load_balance/mod.rs` |
| `fallback` | 主执行器失败/超时后启用备选 | `executor/fallback.rs` |
| `cache` | 内存应答缓存:正/负缓存、lazy stale 兜底、去重后台刷新、持久化 | `executor/cache/mod.rs` |
| **应答合成** | | |
| `hosts` | 静态 hosts 本地应答(full/domain/keyword/regexp 规则) | `executor/hosts.rs` |
| `arbitrary` | zone 文件式静态记录应答 | `executor/arbitrary.rs` |
| `response` | 显式响应模板(answer/authority/additional) | `executor/response.rs` |
| `black_hole` | 黑洞应答:nxdomain / nodata / refused / null / 自定义 IP | `executor/black_hole.rs` |
| **改写与 EDNS** | | |
| `redirect` | 改写 qname → 下游解析 → 还原并前置 CNAME | `executor/redirect.rs` |
| `edns_filter` | 在 continuation 范围内按 policy(allowlist/denylist)过滤请求 EDNS0 选项,默认执行后整体还原请求 | `executor/edns_filter/mod.rs` |
| `ecs_handler` | 添加 / 剥离 / 限制 EDNS Client Subnet | `executor/ecs_handler.rs` |
| `forward_edns0opt` | 把选定 EDNS0 选项从请求透传到响应 | `executor/forward_edns0opt.rs` |
| `ttl` | 改写应答记录 TTL | `executor/ttl.rs` |
| `drop_resp` | 丢弃当前响应 | `executor/drop_resp.rs` |
| `prefer_ipv4` / `prefer_ipv6` | 双栈 A/AAAA 择优:非首选族应答触发首选族探测,可配 `probe_executor` 指定任意执行器代发探测 + 结果缓存(`cache`/`cache_ttl`) | `executor/dual_selector.rs` |
| `client_ip_from_ecs` | 用请求 ECS 选项替换请求侧客户端 IP | `executor/client_ip_from_ecs.rs` |
| **观测与数据** | | |
| `query_summary` | 下游执行后的紧凑查询日志 | `executor/query_summary.rs` |
| `debug_print` | 打印请求/响应对象 | `executor/debug_print.rs` |
| `metrics_collector` | 分段计数器与延迟直方图 | `executor/metrics_collector.rs` |
| `query_recorder` | SQLite 查询快照 + 执行路径事件录制 | `executor/query_recorder/mod.rs` |
| `reverse_lookup` | 应答 IP → 域名缓存,可选直接回答 PTR | `executor/reverse_lookup.rs` |
| `ip_selector` | 探测 / 打分择优 A/AAAA 记录 | `executor/ip_selector/mod.rs` |
| `learn_domain` | 把观察到的 qname 追加进 dynamic_domain_set | `executor/learn_domain/executor.rs` |
| **外部联动** | | |
| `script` | 每请求执行外部命令 | `executor/script.rs` |
| `http_request` | 出站 HTTP 回调副作用 | `executor/http_request.rs` |
| `ipset` / `nftset` | 应答 IP 同步进 Linux ipset / nftables(经 oxidns-ripset) | `executor/ipset.rs` / `executor/nftset.rs` |
| `ros_address_list` / `ros_route` | MikroTik RouterOS 地址表 / 路由同步 | `executor/ros_address_list/mod.rs` / `executor/ros_route/mod.rs` |
| `cron` | 定时触发其它执行器(离开请求路径) | `executor/cron.rs` |
| `sleep` | 人工延迟(测试用) | `executor/sleep.rs` |
| **运维** | | |
| `download` | 启动时预取远程规则文件(prepare_startup 钩子) | `executor/download.rs` |
| `reload` / `reload_provider` | 触发应用级重载 / provider 原地重载 | `executor/reload.rs` / `executor/reload_provider.rs` |
| `upgrade` | 从流水线触发自升级 | `executor/upgrade.rs` |

## Provider ×6

`contains_name / contains_question / contains_ip` + `reload()`。

| 类型 | 功能 | 源文件 |
|------|------|--------|
| `domain_set` | 高性能域名表达式集:精确哈希 / 后缀 trie / AC 自动机 / RegexSet 四引擎,按成本递增匹配 | `provider/domain_set.rs` |
| `ip_set` | IP/CIDR 集:v4/v6 排序区间快速匹配 | `provider/ip_set.rs` |
| `geosite` / `geoip` | V2Ray geosite.dat / geoip.dat protobuf 数据集 | `provider/geosite.rs` / `provider/geoip.rs` |
| `adguard_rule` | AdGuard Home DNS 过滤规则编译器 | `provider/adguard_rule/mod.rs` |
| `dynamic_domain_set` | 运行时可写域名集:热路径读 ArcSwap 快照,写入后台持久化 | `provider/dynamic_domain_set/mod.rs` |

> 部分插件按 feature 编译(mikrotik / ipset / query-recorder / cron / script / upgrade 等见 `Cargo.toml` 特性图);registry 在初始化计划中会跳过没有任何存活依赖者的 provider 并给出告警。
