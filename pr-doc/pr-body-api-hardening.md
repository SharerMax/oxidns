# PR:fix(api): harden request body, auth, and config save

**目标分支**:svenshi/oxidns `main`
**来源分支**:SharerMax/oxidns `fix/api-hardening`(3 个提交)
**创建命令**:

```bash
gh pr create \
  --repo svenshi/oxidns \
  --head SharerMax:fix/api-hardening \
  --base main \
  --title "fix(api): harden request body, auth, and config save" \
  --body-file pr-body-api-hardening.md
```

---

## 摘要

管理 API 的三项加固,均由代码评审发现、相互独立:

1. **请求体上限(未认证 OOM)**——`read_hyper_request` 是每个管理 API 请求的第一步,且在 `/api` 前缀剥离与鉴权之前执行,原先无上限收集 body:未认证客户端向任意路径流式发送数据即可无界增长内存。现在统一上限 4 MiB,超出返回 413(与 DNS 面 DoH 服务的既有 64 KiB 限制模式一致;管理面上限更宽以容纳 `PUT /api/config` 携带完整 YAML)。
2. **Basic auth 恒时比较**——解码后的 `user:pass` 原先用 `==` 逐字节短路比较,泄露前缀匹配计时信号;改为 XOR 累积、最后判零。顺带按 RFC 7235 使 scheme 匹配大小写不敏感(原先只接受 `Basic `)。
3. **`PUT /config` 串行化 + 原子写**——两个并发认证 PUT 可能都通过同一 `base_version` 校验后互相覆盖(静默 last-write-win);`fs::write` 非原子,崩溃/磁盘满会在线路径留下截断的配置文件。现在 check-then-write 序列由互斥锁串行化,发布经同目录临时文件 + rename,保存版本号直接取自写入内容(不再读回)。

## 提交

| 提交 | 内容 |
|---|---|
| `7e10d7dd` | fix(api): bound request body collection before auth |
| `c3845f77` | fix(api): compare basic-auth credentials without short-circuiting |
| `668667ac` | fix(api): serialize config saves and write atomically(含 nightly fmt 的测试导入顺序尾随)|

## 测试

- 新增:auth 恒时比较/大小写/畸形头 4 项测试;`constant_time_eq` 单测。
- 既有:全部 api 测试(84 项)通过;全量 `cargo test` 1322 项通过(基于完整分支;本系列单独验证 api 套件 + fmt + clippy)。
- 门禁:`cargo +nightly fmt --all --check` 干净;`cargo +nightly clippy --all-targets --all-features -- -D warnings` 对 `src/api/*` 干净。

## 影响面

- **行为变化**:超 4 MiB 的 `/api` 请求体现在返回 413(此前会尝试缓冲直至 OOM);Basic scheme 大小写不敏感;并发配置保存从"静默覆盖"变为后到者收到正常成功、但版本检查窗口内互斥(仍 last-write-win 语义,只是不再有校验竞态)。
- **兼容性**:对外 API 契约不变(无新端点/字段)。

## 已知事项

- Windows nightly clippy 在 `src/infra/upgrade/mod.rs:287` 与 `src/infra/network/dial.rs:483` 有 **main 上既有**的 lint(由后续系列携带的 `87cad1b2` 修复),与本系列无关;`src/api/*` 无任何告警。
