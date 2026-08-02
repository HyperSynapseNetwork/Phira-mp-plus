# PMP 相对 Phira-mp 新增功能总览

> 本文档说明 Phira-mp-plus（PMP）在官方 [Phira-mp](https://github.com/TeamFlos/phira-mp) 基础上新增的全部能力。
> 官方 Phira-mp 是一个基础的多人游戏服务器：TCP 游戏协议、房间、游玩轮次，无持久化、插件、管理或可观测性。

## 一、新增功能总览

| 分类 | 官方 Phira-mp | PMP 新增 |
|---|---|---|
| 数据持久化 | 无（内存态，重启丢失） | WAL + PostgreSQL + 原子恢复 |
| 官方客户端兼容 | 自研协议 | 兼容官方客户端的完整语义层 |
| 插件系统 | 无 | WASM 插件 + HTTP/SSE API + 事件 |
| 管理与运维 | 无 | CLI、封禁、房间管理、监控 |
| 可观测性 | 无 | metrics、Sentry、协议追踪 |
| 性能 | 无 | 高频写入、基准测试 |
| 本地化 | 无 | Fluent 三语 |
| HTTP/SSE API | 无 | `/api/rooms`、SSE 事件流 |
| 安全 | 无 | fail-closed、参数化 SQL、权限 |

---

## 二、数据持久化与数据安全

### 2.1 WAL（Write-Ahead Log）
- 所有权威事件（认证、房间变更、round 完成、高频数据）先写 WAL（`data/persistence-worker.wal.jsonl`）再入库
- 每帧带 SHA-256 checksum，损坏/撕裂即 **fail-closed**（拒绝启动/认证，不静默丢数据）
- 崩溃恢复：replay 未确认事件、dead-letter 队列重放、abort 未完成 round

### 2.2 PostgreSQL
- 持久化表：`mp_users`（用户）、`mp_rounds`（round）、房间快照、回放、高频数据（Touches/Judges 批量）
- 全部参数化 SQL（防注入）

### 2.3 fail-closed 语义
- WAL 损坏/恢复不确定 → 拒绝启动（`startup recovery: WAL replay has not completed`）
- 持久化不可用 → 拒绝认证（`persistence unavailable`）
- 关闭时持久化未确认 → 非零退出码（不谎报 clean）

---

## 三、官方客户端兼容层

官方客户端（Phira app）的协议与自研 server 不同，PMP 复现官方可观察行为：

- **CommandOrigin 贯穿**：命令的真实 Session 贯穿「网络 → session actor → room actor → 响应」，重连后旧命令不响应新连接
- **认证状态机**：`Authenticating → DurableAccepted → ResponseFlushed → Active`，每步失败可回滚
- **双 deadline**：commit/response 预算分离，权威提交不因响应超时被误判
- **Outbound Gate**：认证屏障缓冲房间广播，快照原子切换（cutover），事件按 `room_seq` 绑定
- **协议 Hack**：固定顺序补偿（ChangeHost→ChangeState→PersistentRoom→Replay）
- **Golden 协议测试**：59 个协议变体的判别值/字节布局固定

---

## 四、插件系统

### 4.1 WASM 插件（wasmtime）
- 插件以 WASM component（WIT 接口）运行，受信任沙箱
- 资源限制：内存、fuel、栈、HTTP、文件、并发、超时
- 插件事件：`GameEnd`、`RoundComplete`、房间事件

### 4.2 插件 HTTP/SSE
- PMP 动态挂载插件声明的 HTTP 路径与 SSE 流
- `/api/events`（通用 SSE）、插件自定义路径（如 HSNPhira 的 `/api/rooms`）

---

## 五、管理与运维

- **CLI**：房间管理（创建/锁定/循环/隐藏/踢人/强制移动）、封禁、监控
- **封禁**：用户 ID 封禁（带理由）、IP 封禁（认证时拒绝并显示理由）
- **持久房间**：房间状态快照、重启恢复
- **在线监控**：用户/房间/会话计数

---

## 六、可观测性

- **ProtocolTrace metrics**：请求/响应时序、认证屏障、慢路径、异常路径计数器
- **响应延迟直方图**：命令处理耗时分布
- **Sentry 集成**：错误/panic 上报（可选 feature，Release 默认关闭）
- **协议追踪**：request_received → response_queued → flushed

---

## 七、性能

- **High-Frequency Writer**：Touches/Judges 批量写 PostgreSQL（独立 worker，不阻塞主路径）
- **基准测试模式**：`benchmark` 子命令，压测房间/游玩/持久化

---

## 八、本地化

- Fluent 本地化：zh-CN / zh-TW / en-US
- 系统消息、结算排行、封禁理由等按用户语言输出

---

## 九、HTTP/SSE API（对外）

- `/api/rooms`：房间列表（兼容 gooophira/tphira-mp 格式）
- `/api/rooms/listen`：SSE 房间事件流（create/update/join/leave/player_score/start_round）
- 管理 API（token 鉴权）

---

## 十、安全

- 参数化 SQL（防注入）
- fail-closed（不确定状态拒绝，不静默）
- 权限：房主/管理员/监视者区分
- WAL 完整性校验 + 损坏检测

---

## 部署差异（相对官方）

| 项 | 官方 | PMP |
|---|---|---|
| 数据库 | 无 | PostgreSQL 必填（`database_url`/`PM_DATABASE_URL`） |
| 持久化文件 | 无 | `data/`（WAL、快照、回放） |
| 插件 | 无 | `plugins/`（WASM） |
| 依赖 | 仅 Rust | PostgreSQL + 可选 Sentry |
| 默认端口 | 游戏端口 | 游戏 + HTTP 双端口 |

---

## 附：官方没有、PMP 独有的核心概念

- WAL fail-closed 持久化
- Session origin/代际（重连语义）
- Outbound gate + cutover（快照原子性）
- Commit/response 双 deadline
- WASM 插件 + SSE 事件流
- 参数化 + 全审计的 SQL
