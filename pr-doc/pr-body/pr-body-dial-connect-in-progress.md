# refactor(network): simplify connect-in-progress check

## 背景

`is_connect_in_progress`（`src/infra/network/dial.rs`）以 `if err.kind() == ErrorKind::WouldBlock { return true; }` 开头，后面按 `cfg(unix)` 分两个分支：

- Unix：返回 `err.raw_os_error() == Some(libc::EINPROGRESS)`；
- 非 Unix：返回字面量 `false`。

在非 Unix（Windows）构建里，整个函数等价于 `if cond { return true; } false`，Clippy 的 `needless_bool` 在 `-D warnings` 下变成错误——这是本地与 Windows CI 的既有门禁阻断项，此前多个 PR 都不得不备注「clippy 仅剩该既有 lint」。

## 变更

- Unix：`err.kind() == ErrorKind::WouldBlock || err.raw_os_error() == Some(libc::EINPROGRESS)`；
- 非 Unix：`err.kind() == ErrorKind::WouldBlock`；
- 语义不变：`WouldBlock` 的判定与 Unix 上的 `EINPROGRESS` 判定都与改动前一致，只是把早返回合并进布尔表达式。
- 单文件，+2/−6，不涉及其它函数。

## 影响面

- 纯门禁/风格修复，无行为变化、无配置项与 API 契约变化；
- 解决的是 Windows 上 `cargo +nightly clippy --all-targets --all-features -- -D warnings` 的既有失败。

## 测试

| 命令 | 结果 |
|------|------|
| `cargo check --all-targets` | 通过，0 warning |
| `cargo +nightly clippy --all-targets --all-features -- -D warnings` | 通过（`needless_bool` 错误消失；仅剩 cargo 的 manifest warning `unused dependency tar`，与本次改动无关） |
| `cargo +nightly fmt --all --check` | 通过 |
| `cargo test` | 1410 passed / 0 failed / 4 ignored（lib 1290、integration 115、其余 5） |

> 这条改动的意义就在于把此前每个 PR 都要备注的「clippy 仅剩上游既有 Windows-only lint」彻底消掉：本机 Windows 上完整 clippy 门禁现在可以通过。

## 关联

- 基线：`origin/main` @ `b8d30262`，该提交仍含触发 lint 的写法；
- fork 上另有 `fix/clippy-needless-bool`（`56a94902`）做同类改动，但 patch-id 不同、改法略长；两者择一即可，避免重复 PR。
