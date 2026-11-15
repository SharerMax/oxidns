# perf(upstream): run probe blocking work on tokio's blocking pool

## 背景

`oxidns probe` 的诊断路径里，SOCKS5 代理解析与系统域名解析都是阻塞调用，原先每次都 `std::thread::spawn` 一个独立 OS 线程：

- 线程数没有上限：一次探测会按上游数量 × 采样轮次发起解析，每个未完成的解析各占一个 OS 线程；
- 探测超时后线程不会退出（系统解析要等 syscall 返回），于是一次超时的探测会留下一串仍在运行的线程。

tokio 的 blocking pool 是有界的（默认 512 线程，超出则排队），把阻塞工作放上去可以封住线程创建。

## 变更

- `src/infra/network/upstream/probe.rs`：`run_probe_blocking_with_timeout` 改用 `tokio::task::spawn_blocking`，并 `drop` 掉 JoinHandle 让任务脱离——与原先「超时后线程继续跑」的行为一致。
- `src/cli/probe.rs`：CLI 侧显式 `runtime.shutdown_background()`。原因是 tokio 的 runtime 在 drop 时会**无界等待** blocking 任务完成（tokio 文档：*“Normally, dropping a runtime will block indefinitely for spawned blocking tasks to complete”*）；不处理的话，一次卡住的 `getaddrinfo` 会让 `oxidns probe` 无法退出。`shutdown_background()` 与改动前「分离线程不阻塞进程退出」的行为一致。

## 评审记录

- **改动仍然必要**：上游 `origin/main` 的 `src/infra/network/upstream/probe.rs` 仍是 `std::thread::spawn`。
- **作用域澄清**：该模块只被 CLI 诊断命令使用（`src/cli/probe.rs` → `oxidns probe`），**不是**服务端每请求路径上的探测（`ip_selector` / `dual_selector` 各有独立的 probe 模块）。因此这是诊断命令的资源治理，不是热路径优化。
- **行为等价性**：超时、取消、panic 三条路径与改动前一致——
  - 超时后阻塞调用仍在后台跑到 syscall 返回（和原先的分离线程一样）；
  - 超时后 oneshot 发送失败，错误被忽略；
  - blocking 任务里的 panic 由 tokio 捕获并交给被 drop 的 JoinHandle，不会带崩进程（分离线程 panic 同样不会）。
- **池上限**：blocking pool 默认 512 线程，超出后任务排队（队列本身无界），但探测侧的超时仍然生效——排队等待也计入超时，表现为「探测超时」而不是线程风暴。
- **共享池风险**：blocking pool 是进程内共享资源；本 PR 的探测运行在 CLI 自己的 runtime 里，不与服务器进程共享，因此不会挤占服务端的其它阻塞任务。
- **剩余同类点（本次未改）**：`src/cli/probe.rs` 的 `lookup_host_with_timeout` 是**同步**函数，用 `std::sync::mpsc::recv_timeout`，没有 runtime handle 可用，不在本 PR 范围；可作为后续项（例如改用 `Handle::current().spawn_blocking`）。

## 影响面

- 只影响 `oxidns probe` 命令；服务端运行路径不变；
- 无配置项、无 API 契约变化。

## 测试

| 命令 | 结果 |
|------|------|
| `cargo check --all-targets` | 通过，0 warning |
| `cargo test probe` | 55 passed / 0 failed（lib 52、integration 3） |
| `cargo test` | 1410 passed / 0 failed / 4 ignored（lib 1290、integration 115、其余 5） |
| `cargo +nightly clippy --all-targets --all-features -- -D warnings` | 见下方说明 |
| `cargo +nightly fmt --all --check` | 通过 |

> clippy 说明：本机为 Windows，`src/infra/network/dial.rs:483` 的 `needless_bool` 属上游既有 lint（仅 `cfg(not(unix))` 分支编译），该文件未被本 PR 修改，Linux CI 不编译该分支。

## 关联

- 本改动在内部分支上先以 `71d699d8` 落地，再 cherry-pick 到 `origin/main` @ `ad2046d3`；评审时补上了 CLI 的 `shutdown_background()`，以免把「探测超时」变成「命令无法退出」。
