# PR:feat(provider): degrade instead of aborting startup when rule files are missing

> 目标分支:`svenshi/oxidns:main` · 对应 issue #371 · milestone v1.8.0

## 摘要

依赖外部规则文件的 provider(geosite、geoip、adguard_rule、domain_set、ip_set)此前在规则文件缺失或不可读时会中止整个启动。本 PR 改为降级:失败以 `error` 级日志报出,provider 继续服务已加载的部分,并把降级文件路径暴露给管理层。文件补齐后通过 `POST /api/plugins/{tag}/reload` 重建快照即可恢复,无需重启。

对应 issue #371(文件缺失导致无法启动,是否可以改成报错但不阻止启动)。

## 行为变化

- **文件缺失不再阻止启动**。这是一次默认行为翻转:此前缺失文件是硬启动失败;现在受影响的规则不生效,DNS 服务继续运行。
- **多源 provider 按文件降级**:domain_set、ip_set、adguard_rule 只跳过打开失败的文件,内联规则和其余文件继续生效。
- **单文件 provider 整体降级**:geosite/geoip 没有内联兜底,仅在 dat 文件打不开(`DnsError::Io`)时降级为空快照并标记文件不可用。dat 内容/选择器错误(`DnsError::Plugin`)保持致命,不会伪装成文件缺失;线程 panic 与线程启动失败(`DnsError::Runtime`)同样保持致命。
- **reload 语义**:reload 时文件不可用返回 `Ok` 并记录降级状态,但不替换已发布快照——启动时快照为空,runtime reload 时继续服务上次正常规则;规则内容错误仍使 reload 失败并保留旧快照。
- 仍然致命:工厂 `create()` 阶段的 args/配置解析错误。
- **请求热路径零改动**:快照结构与成员查询完全不变,改动全部在 init/reload 编排层。

## 实现

- `TextSource::scan_tolerant`:打开失败的文件被跳过并以 `SkippedTextFile` 报告;内容错误与消费者错误仍中止扫描。
- `TextSource::open_replay_tolerant`:在 session 打开时固定可用文件集,保证多遍扫描(如 adguard 的 planning/compile 两遍)输入一致;指纹变化保护不变。
- `Provider::degraded_files()`:新增默认为空的 trait 方法;多源 provider 把权威列表保存在当前快照内,geosite/geoip 因需保留上次正常 matcher 而把 degraded 列表独立存放。API 接线(reload 响应扩展、provider status 端点、`plugin_degraded` gauge)在后续 PR 跟进。

## 配置影响

无 schema 变化,无新字段。现有配置自动获得新的降级行为。

## 测试

- `src/infra/io/text_source.rs`:容错扫描跳过缺失文件但保留其余源;消费者错误仍中止;容错 replay 在每一遍扫描中都排除缺失文件。
- 各 provider:文件缺失时降级(Ok + `degraded_files`),其余源继续匹配;adguard 的 reload 缺文件走降级,内容错误保留旧快照。
- geosite/geoip:dat 文件缺失后 reload 恢复匹配;选择器无效时 reload 返回 `Err` 且保留上次正常快照。
- `tests/plugin_integration.rs::test_provider_missing_rule_file_degrades_startup_and_recovers`:带缺失文件的配置启动成功,对应规则不匹配;文件补齐后 reload 恢复匹配,无需重启。

已运行的命令:

```
cargo +nightly fmt --all --check
cargo +nightly clippy --all-targets --all-features -- -D warnings
cargo test --lib provider
cargo test --test plugin_integration
```

已知预先存在问题(与本次改动无关,main 上即可复现):未使用的 `tar` 依赖警告、`dynamic_domain_set` 因全局 API 路由状态导致的并行测试 flakiness(已记录为 learning/issues.md ISSUE-003)。

## 文档跟进

- README 能力描述更新 + release note 显式标注默认行为翻转。
- website 插件参考的"文件缺失行为"说明(计划中)。
