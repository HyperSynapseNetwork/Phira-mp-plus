# WIT ABI 规范

> 规范源为 `wit/phira-plugin.wit`。修改后必须运行 `cargo test --test wit_abi_contracts`，并用真实 Component fixture 执行生命周期、capability 与资源限制集成测试。

## 当前状态

| 属性 | 值 |
|---|---|
| 运行时 ABI | `abi-wit-v2` / Component Model |
| World | `phira-plugin-v2` |
| 规范文件 | `wit/phira-plugin.wit` |
| `MIGRATION_PHASE` | `3`：JSON bridge 已移除，WIT-only |
| 接口/宿主函数 | 12 个接口、53 个宿主函数 |
| 生命周期导出 | `init`、`get-info`、`cleanup`、`on-event`、`on-api` |

本轮静态审计确认宿主分发路径已接通。本环境已组装 Rust 1.88 工具链并完成变更 Rust 文件的 rustfmt 校验，但由于 `index.crates.io` DNS 解析失败，依赖无法获取，WIT workspace check/test 尚未在本环境完成；以 CI 的锁文件构建与真实 Component fixture 测试为准。

## 接口

- `phira-types`：共享类型。
- `phira-host`：日志、UUID、时间、通用 API、聊天、受限 HTTP。
- `phira-events`：用户、房间、游戏、触摸、判定与轮次事件。
- `phira-query`：用户、房间和扩展数据查询。
- `phira-room-mgmt`：建房、踢出、房主、锁定、隐藏、关闭、房间 endpoint。
- `phira-user-mgmt`：踢人、封禁、解封与封禁查询。
- `phira-messaging`：用户、房间和全局消息。
- `phira-persistence`：事件、房间快照、触摸、判定和游玩时长查询。
- `phira-admin`：管理员 ID 管理。
- `phira-config`：逐插件配置读写、重载和版本轮询。
- `phira-simulation`：Simulation 状态与控制。
- `phira-runtime`：运行时诊断。

精确签名以 WIT 文件为准，不在本文复制第二份可漂移的接口定义。

## Capability 边界

宿主按插件稳定 ID 绑定 capability。关键映射：

| 能力 | 典型接口 |
|---|---|
| `state.read` | 用户、房间、runtime、持久化查询 |
| `send` | 消息发送 |
| `ext` | extension 数据 |
| `config` | 插件配置读写/重载 |
| `file.read` / `file.write` | 插件私有文件 |
| `plugin.call` / `plugin.register` | 插件 API 调用与内部路由/SSE 注册 |
| `http` | 出站 HTTP |
| `room.manage` | 房间管理写操作 |
| `admin` | 踢人、封禁、管理员写操作 |
| `simulation` | Simulation 控制 |

未知方法映射为拒绝，未知 capability 也拒绝加载。管理员和房间管理能力不会因缺少 sidecar 自动授予。

## 资源与故障语义

- Wasmtime 启用 fuel；每次 guest 调用重新设置预算。
- Store limiter 限制线性内存、实例、内存和表数量。
- 每插件只允许一个执行中的调用；并发调用快速失败。
- API/event 超时后插件进入 quarantine，后续调用被拒绝，直到显式重新启用或重载。
- 进程内 `spawn_blocking` 不能强杀已经进入任意阻塞宿主函数的线程。完全不可信插件必须迁移到独立进程或容器边界。

## 兼容性规则

- 仅支持 `abi-wit-v2`
- WIT 的破坏性修改必须提升 package/ABI 版本，而不是只修改 Rust 实现。
- 新增字段优先使用可选类型或新增接口，避免改变已有 record/variant 的二进制契约。
- 插件元数据中的显示名称不是安全身份；授权与清理均使用稳定插件 ID。

## 发布门禁

至少执行：

```bash
scripts/check-changed-rustfmt.sh
cargo check --locked --workspace --all-targets
cargo check --locked -p phira-mp-plus-server --no-default-features --features wit-bindgen
cargo test --locked --workspace
cargo test --locked -p phira-mp-plus-server --test wit_abi_contracts
cargo clippy --locked --workspace --all-targets
```

另需覆盖无限循环、内存增长、越权、初始化超时、事件超时、API 超时、trap 后重载和卸载清理。
