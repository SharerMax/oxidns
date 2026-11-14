# perf(proto): O(1) compression-pointer loop detection

> **状态:已合入上游(PR #432,merge `de0a8290`)。** 分支 `perf/proto-compression-pointer-loop` 已删除(本地与 fork),提交以标签 `archive/proto-compression-pointer-loop` @ `75d3e4ab` 归档;上游 `origin/main` 含该改动。
> 上游随后在该实现之上又收紧一步:`83153730` `fix(proto): reject compression cycles across labels and pointers` 把判定基准从「当前游标」改为「上一个指针目标(`pointer_limit`)」,以便在标签推进游标后仍能当场识别 label→pointer→label 型环,并补了嵌套压缩后缀与指针预算边界的用例。

## 背景

`crates/proto` 的名字解析在遇到压缩指针时,用「已访问偏移量」的小向量做环检测:每跳一次指针就线性扫描一遍已访问集合。构造一个由长指针链组成的报文,这条路径的最坏开销是 O(n²)——解析单个名字要付出成百上千次比较,构成 CPU 放大面。

RFC 1035 §4.1.4 给出了更强的约束:压缩指针必须指向**先前出现**的名字。这意味着每一次跳转都必须严格回到更小的偏移,链条不可能成环,单个指针的校验是 O(1)。指针数量上限(`MAX_COMPRESSION_POINTERS`)作为兜底继续保留。

## 变更

- `crates/proto/src/name.rs`
  - 解析压缩指针时先校验 `ptr >= cursor`,命中即拒绝(`dns compression pointer does not reference a prior occurrence`),删除 `visited` 偏移集合与每跳一次的线性扫描;
  - 指针数量上限与报文长度校验保持不变;
  - 错误文案由「dns name compression pointer loop detected」改为「dns compression pointer does not reference a prior occurrence」。
- 行为变化:此前能被接受的**前向指针**(指向自身之后的偏移)现在按 RFC 拒绝;自环、截断、越界等既有错误路径的行为不变。
- 测试:指针矩阵新增「forward pointer」用例;新增两条回归用例——前向指针链在第一个前向跳转处即被拒绝(不再有二次方扫描)、合法的后向指针链仍由指针预算兜底。

## 影响面

- 只影响 `crates/proto` 的名字解析;编码路径、其它 RR 类型、EDNS 等均不变;
- 合法 DNS 报文的压缩指针一律指向更早的偏移,不受影响;不合规(前向指针)报文此前也能解析,现在按 RFC 拒绝;
- 解析热路径上每个指针少一次线性扫描,恶意长链不再放大 CPU;
- 无配置项、无 API 契约变化。

## 测试

| 命令 | 结果 |
|------|------|
| `cargo check --all-targets` | 通过，0 warning |
| `cargo test -p oxidns-proto` | 79 passed / 0 failed（含 3 个新用例） |
| `cargo test` | 1408 passed / 0 failed / 4 ignored（lib 1288、integration 115、其余 5） |
| `cargo +nightly clippy --all-targets --all-features -- -D warnings` | 见下方说明 |
| `cargo +nightly fmt --all --check` | 通过 |

> clippy 说明：本机为 Windows，`src/infra/network/dial.rs:483` 的 `needless_bool` 属上游既有 lint（仅 `cfg(not(unix))` 分支编译），该文件未被本 PR 修改，Linux CI 不编译该分支。

## 关联

- 上游当前仍是「已访问偏移扫描」实现：`origin/main` 的 `crates/proto/src/name.rs` 仍含 `visited.contains(...)`；
- 本改动在内部分支上先以 `202a8403` 落地，再 cherry-pick 到 `origin/main` @ `a3dd95c0`；附带补充了三条测试用例。
