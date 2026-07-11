# PMP 架构加固交付报告

## 1. 交付边界

PMP 在 PP 架构中定位为受 PPB 控制的游戏服务端。公共 Web API、统一认证、TLS、边缘限流和公网接口治理属于 PPB。本轮保留 PMP 现有 HTTP/SSE/WebSocket 作为受控网络中的兼容、诊断和插件接口，没有把它扩展为独立公网网关。

本轮修改集中在 PMP 内部的连接接入、会话与房间命令语义、插件边界、事件分发、持久化、后台任务监督、优雅关闭、配置与文档一致性。

## 2. 已实施修改

### 2.1 接入与会话

- TCP listener 与认证解耦，慢认证连接不再串行阻塞全局 accept。
- `max_pending_auth` 限制并发认证；`max_sessions` permit 从认证前预留并持有到 Session 结束。
- 关闭开始后停止新接入；已接受但尚未提交的认证任务在注册前复查 shutdown。
- socket 读取与业务命令通过有界队列解耦。
- 慢消费者使用非阻塞有界发送策略，不再无限阻塞整个房间广播。
- 发布模式缺少有效 `HSN_SECRET_KEY` 时拒绝启动；调试模式使用进程级随机密钥。

### 2.2 Actor 与命令一致性

- Session 和 Room 管理命令使用有界 mailbox。
- 命令已成功入队但 reply 超时/丢失时，不再通过 inline fallback 重放副作用。
- lock、cycle、host、hidden、endpoint、kick、start、cancel、close 等管理写操作统一进入 per-room gateway。
- lock/cycle 删除 mailbox 影子状态，回到 `Room` 的单一当前真理源。
- Supervisor 具备任务登记、退出/panic 观测、关闭时 abort/join；不虚假宣称自动重启。

### 2.3 插件

- capability 检查接入真实逐插件 host 调用边界，未知方法和未知 capability 默认拒绝。
- 授权、运行时注册和卸载清理使用稳定插件 ID。
- Wasmtime 启用 fuel，并在每次 guest 调用前重置预算。
- Store 接入线性内存、实例、内存和表数量限制。
- 插件事件使用有界可靠队列并按事件顺序消费；单个事件内按 `max_event_concurrency` 有界并行插件，高频触摸/判定事件明确为 best-effort 低优先级路径。
- 每插件增加单执行闸门。超时后进入 quarantine，后续调用快速拒绝，避免继续堆积 blocking 任务。
- WIT 中消息、房间管理、用户管理、配置和持久化查询由占位行为改为真实路径。
- 出站 HTTP 采用标准 URL 解析、凭据拒绝、私网/保留地址检查、禁用重定向和流式响应上限。

### 2.4 事件与持久化

- 用户连接/断开通过 canonical 生命周期入口分发，移除直接插件触发与 EventBus 二次触发并存。
- EventBus 降为诊断/观察广播；必须送达的插件事件与持久化使用专用有界队列。
- 持久化队列不再因 `try_send` 满而静默丢弃，Flush/Shutdown 带 acknowledgment。
- 数据库写入增加有限重试；关机流程显式 flush。
- Idle 标志不再改变权威持久化语义。

### 2.5 关闭与配置

- 关闭顺序为：停止接入 → 摘除并关闭 Session → drain 插件事件 → 插件 cleanup → 扩展数据保存 → 持久化 flush/shutdown → Supervisor 统一停止任务。
- 使用共享 shutdown deadline，避免每个子系统各自消耗完整超时时间。
- 新增并校验 `max_sessions`、`max_pending_auth`、`graceful_shutdown_timeout_secs`、插件事件队列和调用期限。
- 文档统一写明 PPB/PMP 边界、内部 HTTP 属性、可信转发头并非 PROXY v1/v2，以及真实的插件超时和持久化保证。

## 3. 没有伪装为“已解决”的剩余问题

### 3.1 Room 全状态尚未 Actor-owned

Room 仍由多个锁和原子字段保存完整状态，Actor 目前主要负责管理命令串行化。要获得严格的单写者和一致快照，需要把成员、房主、chart、round、状态转换等收敛为单一 `RoomState`，并删除共享状态兼容路径。这是结构性迁移，不适合在无法编译和运行回归测试的环境中盲目一次性替换。

### 3.2 无崩溃一致性 WAL

正常运行和正常关闭路径已加固，但内存队列没有本地 WAL。`kill -9`、进程崩溃或主机掉电仍可能丢失尚未落库的数据。需要 append-only WAL 或 transactional outbox 才能声明崩溃级持久化保证。

### 3.3 插件仍是进程内隔离

fuel 和 Store limiter 可以限制 guest 计算与资源增长；超时 quarantine 能阻止后续调用堆积。但进程内 `spawn_blocking` 不能强杀已进入任意阻塞宿主函数的线程。完全不可信插件必须使用独立进程、容器或 cgroup/seccomp 边界。

### 3.4 性能尚未通过实测证明

代码中最直接的阻塞与放大路径已处理，但本轮没有获得 1,000/5,000 并发、热点房间、慢消费者、插件故障和数据库中断下的 p95/p99、CPU、RSS 与队列数据。因此不能把“架构加固”替代为“高性能已验证”。

## 4. 验证状态

当前执行环境没有 Rust/Cargo 工具链。本轮完成了源码修改、静态控制流审阅、配置/文档对齐和结构化文件解析，但没有在本环境执行：

```bash
cargo fmt --check
cargo check --workspace --all-targets
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
```

合并前必须由 CI 或安装 Rust 的开发机执行上述命令。动态门禁还应包含：慢认证、容量竞态、慢消费者、mailbox reply 丢失、SIGTERM、DB 中断、WASM 无限循环/内存增长/越权/超时、插件重载与 6 小时以上 soak test。

## 5. 当前结论

本版本已修复此前审计中大部分可直接导致拒绝服务、重复副作用、权限绕过、静默丢数据和假监督的 P0/P1 路径，PMP 作为 PPB 后方受控游戏服务端的架构边界明显更合理。

但在 workspace 编译与动态回归通过前，不能标记为“生产验证完成”；在 Room 全状态单写者、崩溃一致性 WAL 和进程级不可信插件隔离完成前，也不能无条件宣称已经实现最终的“稳定、高性能、高扩展性”愿景。
