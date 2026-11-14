# perf(upstream): clone bootstrap pool Arc instead of pinning a guard

## 背景

`BootstrapUpstream::inner_query` 是每请求热路径：它先用 `self.pool.load()` 取当前连接池状态，再 `.await` 整条上游查询。`load()` 返回的是 arc-swap 的 `Guard`——它只是**借用**池状态，而不是持有它。

arc-swap 的文档写明了两个后果：

- 每个线程能同时持有的借用（`Guard`）数量有限（`DEBT_SLOT_CNT = 8`，且同一线程上所有 `ArcSwap` 共用这些槽），超出后会退化为「创建一份拥有实例」的慢路径；
- 内容被替换时，写者必须把所有被钉住的 `Guard` **提升**为拥有实例，读者随后还要在旧实例的引用计数上互相竞争。

把 `Guard` 跨越整条查询 await 持有，意味着并发查询各自占住一个借用槽；而当引导解析结果变化、池被换成新实例时，写者要为一堆被钉住的 `Guard` 做提升，读者们随后在旧实例的引用计数上竞争。

## 变更

`src/infra/network/upstream/bootstrap.rs` 的热路径改为**把 `Arc` 从借用里克隆出来，随即释放 `Guard`**：

```rust
let pool = (*self.pool.load()).clone();
```

- 借用槽在语句结束时就释放，不再跨越查询 await；
- 池被替换时不再有需要提升的 `Guard`；
- 克隆出来的 `Arc` 同样把当前池状态钉住，查询仍然跑在「查询开始那一刻的池」上，并发的池切换不会把在途查询改道。

**为什么不用 `load_full()`**：它同样能避免占用借用槽，但 `load_full()` 内部是 `Guard::into_inner(load())`，而 `into_inner` 会调用 `Debt::pay_all`，遍历所有用过 `ArcSwap` 的线程节点、逐节点取写者预留并扫描其槽位，把当前值上的未结债务一次性结清。对每请求路径来说，这比一次引用计数克隆贵得多——arc-swap 自己的文档也说 `load_full` “usually more expensive than `load`”。克隆 `Arc` 的做法拿到同样的收益，成本是一次原子引用计数增减。

## 收益与代价对比

| 写法 | 每查询成本 | 跨 await 的借用槽 | 池切换时写者成本 |
|---|---|---|---|
| `load()`（改动前） | O(1)：取 1 个快速槽 | 占用 1 个槽直到查询结束（每线程 8 个，且与本线程其它 `ArcSwap` 共享）；并发超过槽数后其它 load 退化为慢路径 | 需把所有被钉住的 `Guard` 提升为拥有实例，读者在旧实例引用计数上竞争 |
| `load_full()` | O(线程数)：`into_inner` → `pay_all` 遍历所有节点 | 不占用 | 不需提升 |
| `(*load()).clone()`（本 PR） | O(1)：取槽 + 一次 `Arc` 引用计数克隆，随即释放槽 | 不占用 | 不需提升 |

- **收益**：借用槽不再被长查询占住；池切换不再需要批量提升；稳态下每查询成本与改动前同量级。
- **代价**：每查询多一对原子引用计数操作（`Arc` 的引用计数在多线程下是共享缓存行，高并发时存在竞争——这是 arc-swap 文档明确指出的 `load_full` 同类代价）；换来的是槽位不被长期占用、写者不需要提升。
- **没有改变的**：池的生命周期语义（新旧池仍由 `Arc` 引用计数决定何时释放）；查询所看到的池状态（仍是查询开始那一刻的池）。

## 评审记录

- 改动仍然必要：上游 `origin/main` 的 `src/infra/network/upstream/bootstrap.rs` 仍是 `self.pool.load()`。
- 语义核对：`load()` 返回 `Guard<Arc<BootstrapPoolState<C>>>`，克隆出来的是同一个 `Arc<BootstrapPoolState<C>>`；后续 `pool.pool.query(request, deadline).await` 与改动前等价。
- 安全性：克隆发生在 `Guard` 仍被钉住期间，指针在克隆瞬间必然有效；克隆完成后由 `Arc` 自身保证存活。
- 快照一致性：只取一次值，两种写法都是单一快照，无差别。
- 同类模式仍在别处（本次未改，均为冷路径或后台任务，不在每请求路径上）：
  - `bootstrap.rs` 的 `init_pool_if_needed`：第二次 `load()` 未在进入冷路径 await 前释放（第一次 `load()` 已显式 `drop`）；
  - `pool/pipeline.rs` 的 `maintain()`：持有 `slots` 的 `Guard` 期间调用 `compare_and_swap`，写者需要提升该 `Guard`；
  - `pool/pipeline.rs` 的 `usable_or_inflight_slot_count()`：同样是 `Guard` 存活期间做 CAS。
  
  这三处可以后续单独收敛，不影响本 PR 的每请求收益。

## 影响面

- 仅 `BootstrapUpstream` 每请求热路径一处；无配置项、无 API 契约变化；
- 不涉及 `ReusePool` / `PipelinePool` 等其它池实现。

## 测试

| 命令 | 结果 |
|------|------|
| `cargo check --all-targets` | 通过，0 warning |
| `cargo test --lib bootstrap` | 17 passed / 0 failed |
| `cargo test --lib upstream` | 144 passed / 0 failed |
| `cargo test` | 1410 passed / 0 failed / 4 ignored（lib 1290、integration 115、其余 5） |
| `cargo +nightly clippy --all-targets --all-features -- -D warnings` | 见下方说明 |
| `cargo +nightly fmt --all --check` | 通过 |

> clippy 说明：本机为 Windows，`src/infra/network/dial.rs:483` 的 `needless_bool` 属上游既有 lint（仅 `cfg(not(unix))` 分支编译），该文件未被本 PR 修改，Linux CI 不编译该分支。

## 关联

- 本改动在内部分支上先以 `21ff3d0c` 落地（该版本用 `load_full()`），评审时发现 `load_full()` 的 `pay_all` 遍历对每请求路径偏重，改为「克隆 `Arc` 并立即释放 `Guard`」；随后 cherry-pick 到 `origin/main` @ `ad2046d3`。
