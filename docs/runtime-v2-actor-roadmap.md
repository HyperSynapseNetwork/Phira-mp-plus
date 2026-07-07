# Runtime v2 Actor 迁移路线图

本路线图是防止代码继续堆积到 `server.rs`、`session.rs`、`room.rs` 和 `cli.rs` 的长期目标。

项目在 Actor 迁移完成之前仍然允许发布有用的补丁。规则不是"在 Actor 存在之前停止功能开发"，而是"新的跨功能工作应朝着 Actor 边界移动，而不是在大文件之间添加更多直接调用"。

## 明确非目标

**不要**为 Runtime v2 实现特权 Web 管理 API。

允许的 Web/API 工作:
- 只读诊断
- 已有的公开房间/状态 API
- 不修改服务器状态的可观测端点

写能力的控制面应保持:
- CLI
- TUI
- 游戏内管理员 `_` 命令
- WIT/插件 API（带显式 capability）

## 迁移规则

每个 Actor 迁移应按此顺序进行:

1. **Mirror** — EventBus 镜像现有事件，不改变原有路径
2. **Route reads** — 将读取路径从直接调用改为通过 Actor 边界
3. **Route writes** — 将写入路径改为通过 Actor 边界（保留旧路径作为 fallback）
4. **Own** — Actor 拥有状态所有权，删除旧直接调用

## 当前边界状态

见 `src/actor_runtime.rs` 中的 `default_boundaries()` 函数获取最新状态。CLI 命令 `runtime actors` 也可查看运行时状态。

## 必要性审计

Actor 迁移仍然必要，但不能再靠增加 facade 命令来制造进度。当前最有价值的工作是把已经路由的边界推进到状态所有权，而不是继续扩大命令数量。

优先推进:

- **room-actor**：必要。7 个房间命令已经经过 typed mailbox，但 Room 状态仍由旧 `Room` 方法实际修改。下一步应迁移一小块状态所有权，而不是继续添加 gateway facade。
- **session-actor**：必要。Session 已拆成多个模块，但连接生命周期、认证、命令解码和发送队列仍没有 actor 所有权。
- **persistence-actor**：必要。Worker、pipeline、telemetry batcher 已存在，但仍有多条生产写入绕过 worker。
- **plugin-actor**：必要，但必须先完成 WIT lifecycle 和 host API，再谈 actor ownership。

暂缓推进:

- **cli-actor**：路由拆分已经足够。除非能减少真实耦合，否则不要继续做只搬文件的 CLI 步骤。
- **simulation-actor**：当前作为 shadow-world load/test harness 足够。不要在 Room/Session ownership 之前扩大仿真功能。
- **server-supervisor**：保持 mirrored。等 Room/Session/Persistence 边界稳定后再处理进程生命周期所有权。

## Actor 边界

| 边界 | 职责 | 状态 |
|------|------|------|
| server-supervisor | 进程生命周期、关闭、监听器启动 | Mirrored |
| session-actor | 客户端连接、认证、命令解码、发送队列 | Mirrored |
| room-actor | 房间状态机、成员管理、游戏生命周期 | WriteRouted (lock/cycle/host tracked and effectively Owned; close/kick/start/cancel still WriteRouted) |
| persistence-actor | 数据库批处理、背压、重试、关闭刷新 | ReadRouted |
| simulation-actor | Shadow world、场景套件、确定性回放 | ReadRouted |
| plugin-actor | 插件调度、capability 检查、事件分发 | ReadRouted |
| cli-actor | CLI/TUI/管理员命令执行 | WriteRouted |

## 下一阶段建议

1. ✅ lock/cycle 已跟踪：mailbox worker 拥有 `owned_locks`/`owned_cycles` 并 post-commit 追踪，独占写入已由可见性限制证实。
2. ✅ 合同测试增加：`set_lock_visibility_is_restricted` 验证 mailbox 是唯一写入路径。
3. 下一步：迁移 Session command routing，不要同时改 socket 生命周期。
4. Persistence 只迁移低频写入；Touch/Judge 继续保持 `direct_only` / `worker_preferred` 双轨，直到 worker 延迟和丢弃指标稳定。
