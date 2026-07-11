# 架构修改清单

## 本轮代码修改

- 连接接入：accept/认证解耦，增加全局 Session 与 pending-auth permit。
- 会话 I/O：有界命令队列、非阻塞发送、慢消费者隔离。
- Room Actor：取消不确定状态下的副作用 fallback 重放，统一命令结果。
- 插件：生产路径 capability enforcement、fuel、Store limiter、有界事件队列、每插件单执行闸门和超时 quarantine。
- 持久化：背压、有限重试、Flush/Shutdown acknowledgment、关闭 drain。
- 生命周期：canonical disconnect event，移除重复派发。
- Supervisor：任务退出/panic 观测、注册可靠化、统一取消和 join。
- 关闭流程：停止接入、共享 deadline、先摘除 Session、插件与持久化有序关闭；认证任务提交前复查 shutdown。
- 配置：增加 `max_sessions`、`max_pending_auth`、`graceful_shutdown_timeout_secs`、插件事件队列和调用期限。
- 文档：明确 PPB/PMP 边界，修正 PROXY 命名和 Idle/持久化语义。

## 明确保留

- PMP 内部 HTTP/SSE/WebSocket 不重构为公网网关。
- Room 全状态 Actor ownership 尚未完成。
- 当前持久化无磁盘 WAL。
- 插件仍在 PMP 进程内执行。
