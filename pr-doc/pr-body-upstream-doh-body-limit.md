# fix(upstream): cap DoH/H2/H3 response bodies at 64 KiB

> **状态:已合入上游(PR #431,merge `04bc9500`)。** 分支 `fix/upstream-doh-body-limit` 已删除(本地与 fork),提交以标签 `archive/upstream-doh-body-limit` @ `970e7612` 归档;上游 `origin/main` 含该改动。

## 背景

上游 DoH 连接按远端 `Content-Length` 预分配缓冲，并把流式响应体无上限地累加。恶意或被劫持的上游只要给出一个假的 `Content-Length`，就能让单个响应占用 GB 级内存。

resolver 侧的 DoH 客户端早已有 64 KiB 上限（`resolver/client/doh.rs`），upstream 侧漏掉了。修复前的证据位置：`upstream/conn/doh.rs`、`conn/h2.rs`、`conn/h3.rs` 的响应读取路径。

## 变更

- `src/infra/network/upstream/conn/doh.rs`
  - 新增 `MAX_DNS_MESSAGE_LEN = u16::MAX`（64 KiB）常量：DNS 报文在链路上不会超过该长度，更大的响应体（含伪造的 `Content-Length`）不得驱动缓冲；
  - `get_cap_buf_with_context_len` 在预分配前把 `Content-Length` 钳到上限；
  - 新增 `append_response_chunk`：`Content-Length` 只决定预分配，流式分块本身仍可能超出，因此每个分块都做边界检查（`checked_add` 防溢出，超限返回 `DnsError::protocol`）。
- `src/infra/network/upstream/conn/h2.rs`、`conn/h3.rs`
  - 分块累积统一走 `append_response_chunk`，不再直接 `put_slice` / `put`；
  - h3 侧先 `copy_to_bytes` 取出分块再交给边界检查函数。
- 单测 2 项：超长 `Content-Length` 的预分配被钳到 64 KiB；分块累积到上限时报错（既不越界也不溢出）。
- 附带清理：h2/h3 中 `bytes::BufMut` 不再使用，移除该 import（否则 `clippy -D warnings` 失败）。

## 影响面

- 仅上游 DoH / HTTP2 / HTTP3 连接层，无配置项、无协议语义变化；
- 正常 DNS over HTTPS 响应远小于 64 KiB；超过上限的响应按协议错误处理（等同于「上游返回了非法的大响应」），而不是继续缓冲到 OOM；
- resolver 侧（引导解析）本就有同款上限，两侧行为一致；
- 不涉及请求热路径的解析逻辑，只在响应体累积处增加一次长度检查。

## 测试

| 命令 | 结果 |
|------|------|
| `cargo check --all-targets` | 通过，0 warning |
| `cargo test --lib doh` | 13 passed / 0 failed |
| `cargo test --lib clamps_huge_content_length` | 1 passed |
| `cargo test --lib append_response_chunk_rejects_oversize_body` | 1 passed |
| `cargo +nightly fmt --all --check` | 通过 |
| `cargo +nightly clippy --all-targets --all-features -- -D warnings` | 见下方说明 |
| `cargo test` | 1410 passed / 0 failed / 4 ignored（lib 1290、integration 115、其余 5） |

> clippy 说明：本机为 Windows，`src/infra/network/dial.rs:483` 的 `needless_bool` 属上游既有 lint（仅 `cfg(not(unix))` 分支编译），该文件未被本 PR 修改，Linux CI 不编译该分支。

## 关联

- 本改动在内部分支上先以 `d08d26e7` 落地，再 cherry-pick 到 `origin/main` @ `a3dd95c0`；因附带移除未使用的 `BufMut` import，最终 patch 与内部提交略有差异（原提交改用 `put_slice`/`put`，现已统一走 `append_response_chunk`）；
- 上游当前没有同款限制：`git grep MAX_DNS_MESSAGE_LEN origin/main -- src/infra/network/upstream/conn` 无命中。
