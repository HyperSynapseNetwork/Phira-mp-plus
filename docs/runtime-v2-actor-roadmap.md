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

## Actor 边界

| 边界 | 职责 | 状态 |
|------|------|------|
| server-supervisor | 进程生命周期、关闭、监听器启动 | Mirrored |
| session-actor | 客户端连接、认证、命令解码、发送队列 | Mirrored |
| room-actor | 房间状态机、成员管理、游戏生命周期 | WriteRouted |
| persistence-actor | 数据库批处理、背压、重试、关闭刷新 | ReadRouted |
| simulation-actor | Shadow world、场景套件、确定性回放 | ReadRouted |
| plugin-actor | 插件调度、capability 检查、事件分发 | ReadRouted |
| cli-actor | CLI/TUI/管理员命令执行 | WriteRouted |
