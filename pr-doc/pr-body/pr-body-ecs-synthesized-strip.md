# fix(ecs_handler): clear synthesized ECS from the request after the chain

## 背景

上游 PR #426（`afc23736`）把 `ecs_handler` 的 ECS 处理调整为：`forward: false` 时在**正向阶段**删除客户端自带 ECS（非 IN 类查询同样覆盖），只有 IN 类查询且保留了客户端 ECS 时才算「ECS 是转发的」。

但插件**自己生成**的 ECS（`preset` 或 `send`）仍然留在请求上：正向阶段把它写进请求的 OPT 记录，回程阶段只清理响应，请求里的 ECS 一直留到请求结束。

后果是同一条请求上下文里，链后的组件（缓存键、查询摘要、指标、日志）会看到一个**上游从未收到**的网段。`cache` 配了 `ecs_in_key` 时，如果 `cache` 在 `ecs_handler` 之后，键里会带上这个内部生成的网段；`cache` 在其之前则不会——同一个 ECS 在链内链外表现不一致。

## 变更

- `prepare_request` 现在返回 `PreparedEcs { forwarded_client_ecs, synthesized }`，记录本次是否合成了 ECS 以及合成的子网（地址 + 前缀）。
- 回程阶段在剥离响应 ECS 之后，额外清除**本插件合成**的请求 ECS：
  - 仅当请求上的 ECS 仍等于本次合成的子网时才删除；若下游执行器替换过该选项，替换后的值保留，不会被误删；
  - 客户端自带 ECS 不在此处处理：`forward: true` 时它已转发给上游，`forward: false` 时正向阶段已删除。
- 链中语义不变：上游查询仍然看到带 ECS 的请求。
- 单测：新增 `EcsProbe` 下游探针，断言「链中可见合成 ECS、链后请求已无 ECS」「下游替换后的 ECS 保留」「`forward: true` 时客户端 ECS 保留」；三个原先断言「链后请求仍带 ECS」的用例改为断言链内可见、链后已清理。
- 集成测试：`test_ecs_handler_upstream_policy_and_request_lifecycle` 的链后请求断言改为「只有转发的客户端 ECS 保留」；`test_edns_filter_upstream_scopes_and_composition` 的组合场景期望请求不再包含合成的 ECS。
- 文档：插件参考中/英同步回程阶段的清理范围，并说明缓存键在链内计算、不受链后清理影响。

## 影响面

- 仅 `ecs_handler` 插件；不涉及配置项、指标与 WebUI；
- **行为变化点**：链结束后请求不再携带插件生成的 ECS。若有组件依赖「链后仍能读到请求 ECS」，应改在 `ecs_handler` 之前读取，或改用 `client_ip_from_ecs`；
- `forward: true` 且客户端自带 ECS 的路径完全不变（既不合成也不清理）；
- 与上游 #426 的语义衔接：上游把「客户端 ECS」的处理前移到正向阶段并声明不清空请求；本 PR 只补上「内部生成 ECS」这一段，两者互补而非冲突。

## 测试

| 命令 | 结果 |
|------|------|
| `cargo check --all-targets` | 通过，0 warning |
| `cargo test --lib ecs_handler` | 14 passed / 0 failed（含 3 个新用例） |
| `cargo test` | 1410 passed / 0 failed / 4 ignored（lib 1290、integration 115、其余 5） |
| `cargo +nightly clippy --all-targets --all-features -- -D warnings` | 见下方说明 |
| `cargo +nightly fmt --all --check` | 通过 |

> clippy 说明：本机为 Windows，`src/infra/network/dial.rs:483` 的 `needless_bool` 属上游既有 lint（仅 `cfg(not(unix))` 分支编译），该文件未被本 PR 修改，Linux CI 不编译该分支。

## 关联

- 上游 PR #426（`afc23736`）已实现链前剥离客户端 ECS；本 PR 是其补充，只针对内部合成 ECS；
- 早期版本曾尝试链后剥离**全部**请求 ECS（含客户端 ECS），与上游既定语义冲突，已放弃；
- 基线：`origin/main` @ `a3dd95c0`。
