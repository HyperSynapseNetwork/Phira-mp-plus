# PMP 相对 Phira-mp 新增功能（详细）

> 本文档逐一说明 Phira-mp-plus（PMP）在官方 [Phira-mp](https://github.com/TeamFlos/phira-mp) 基础上新增的**每个功能与行为**。
> 官方 Phira-mp 是基础多人游戏服务器（TCP 协议 + 房间 + 游玩轮次），无持久化、插件、管理、可观测性、本地化、HTTP API。
> 以下所有条目均为 PMP 相对官方新增或增强的能力。

---

## 一、房间管理

### 1.1 房间创建 / 加入 / 离开
- 房间 ID 为 1-20 字符字符串（`Varchar<20>`）
- 创建房间、按 ID 加入、离开房间，均有原子事务与补偿（加入失败回滚，不残留 ghost member）
- 官方客户端顺序兼容：`OnJoinRoom` 广播 → `Message::JoinRoom` → `JoinRoom(Ok)`（P0-D 固定顺序）

### 1.2 Playing 状态下加入（官方不支持/行为不同）
- 房间 `Playing` 状态时，其他用户仍可加入：
  - 有 `join_pending_game`（游戏开始前已申请加入）→ **晚加入（late_join）**，进入对局
  - 否则 → 提示 `join-game-ongoing-warning`（「该房间游戏进行中，请再次确认以加入」），客户端确认后以 `late_join` 加入
- 晚加入者可能在超时被标记 `aborted`（若未在截止前提交成绩）

### 1.3 房间锁定 / 循环 / 隐藏
- **锁定（lock）**：禁止新成员加入（已有成员不受影响）
- **循环（cycle）**：整局结束后自动开始下一轮（房主轮换）
- **隐藏（hide）**：房间不出现在公开房间列表（`rooms.list` 过滤 `_` 前缀与隐藏标记）
- 三个状态可由房主 / 管理员 / CLI 切换，变更广播到客户端

### 1.4 房主管理
- 房主转移（手动 / 系统房主）
- 首名成员自动成为房主（`assign_room_host_if_missing`）
- 房主离开时房主转移 / 解散策略

### 1.5 监视者（Monitor）
- 监视者可加入任意房间旁观（不参与游玩）
- 监视者消息（状态、事件）与普通成员区分

### 1.6 持久房间
- 房间状态快照（`mp_room_snapshots`）持久化到 PostgreSQL
- 服务器重启后恢复房间（图表、成员、状态）

---

## 二、游玩轮次

### 2.1 准备 / 取消准备
- `Ready` / `CancelReady`，服务端记录 `started` 集合
- 管理员的 `force_start` 跳过准备检查
- 开赛失败（round store 打开失败）→ 回滚到 `WaitingForReady` + 发 `CancelReady` 收敛客户端

### 2.2 开赛（Playing 状态）
- 所有成员准备 / 管理员强制开赛 → 进入 `Playing`
- 未准备成员在开局时标记 `aborted`（不参与本轮）

### 2.3 游玩数据
- Touches / Judges 高频遥测，经 high-frequency writer 批量持久化
- 游玩超时（`playing_timeout`），超时强制结算

### 2.4 成绩提交（SubmitResult）
- 玩家提交成绩（分数、准确率、Perfect/Good/Bad/Miss、MaxCombo、全连、std）
- 幂等（重复提交拒绝）、deadline 内提交
- **首个完成者延长游玩超时**（给其他玩家追赶时间）
- 提交后广播 `Played`、触发结算检查

### 2.5 结算排行（本地化）
- 整局所有玩家完成 / 放弃 → 结算排行
- 排行显示：名次、名字、分数、准确率、±std、全连（FC）、放弃标记
- 详细行：Perfect/Good/Bad/Miss/MaxCombo
- **按每位用户的语言本地化**（zh-CN/zh-TW/en-US）
- 结算显示**当前轮**结果（不依赖持久化成功，防止读旧轮）

### 2.6 放弃（Abort）
- 玩家可主动放弃（`AbortRound`），标记 `aborted`
- 放弃玩家结算显示 `放弃` + 0 分
- 中途离开 / 超时未提交 → 自动标记 `aborted`

### 2.7 循环模式（Cycle）
- 整局完成自动开始下一轮，房主轮换

---

## 三、会话与认证

### 3.1 认证状态机
- `Authenticating → DurableAccepted → ResponseFlushed → Active`，每步失败可回滚
- 认证绝对 deadline（默认 5000ms），覆盖 API + 重试 + WAL + 最低响应延迟 + flush
- 持久化失败 → fail-closed 拒绝（`persistence unavailable`）

### 3.2 认证缓存
- token SHA-256 哈希作为缓存键，缓存命中毫秒级响应
- 缓存命中时仍做封禁检查

### 3.3 重连恢复
- Session origin / 代际（generation）贯穿：重连后旧命令不响应新连接
- 旧 Session 延迟关闭（新会话 Active 后）
- 原子恢复：单写锁内「检查并替换」绑定（`replace_binding_if_matches`）

### 3.4 断线宽限 / 心跳
- 断线宽限（`dangle_grace_secs`）、心跳超时（`heartbeat_timeout_secs`）
- 超时断开 + 持久化补偿（`UserDisconnect`/`UserOffline`）

### 3.5 会话代际防护
- 会话绑定代际，防止旧会话误清新会话（playtime/session_id 防护）

---

## 四、管理与 CLI

### 4.1 CLI 命令
`room list | create-empty | info | start | ready | cancel | lock | cycle | kick | host | force-move | hide | unhide | close | set | history | rounds | round | uuid | ban | unban | banlist`

- **room set**：lock / cycle / hidden / persistent / degraded / api_endpoint / host / chart
- **force-move**：强制转移用户到另一房间（`send_and_flush` 保证加入通知送达）
- 详细命令见 `docs/cli.md`

### 4.2 封禁
- **用户 ID 封禁**：带理由，认证时拒绝并显示理由
- **IP 封禁**：认证路径拒绝 + 显示理由（默认「IP 地址被封禁」）
- 封禁列表、解除封禁、IP 封禁基于用户连接历史

### 4.3 房间管理
- 强制解散、最大人数动态调整、回放录制开关、房间创建开关
- 房间降级标记（`degraded`）管理

---

## 五、插件系统

### 5.1 WASM 插件
- wasmtime + WIT/component-model，受信任沙箱
- 资源限制：内存（默认 64MB）、fuel、栈（2MB）、HTTP 超时/字节、文件大小、并发（默认 8）、队列（2048）
- 插件超时：init（默认 10s）/ 调用（默认 2s）独立预算
- 插件事件：`GameEnd`、`RoundComplete`、房间事件（response-after，不阻塞 Actor）

### 5.2 插件 HTTP / SSE
- 插件声明 HTTP 路径 + SSE 流，PMP 动态挂载
- `sse:translate` 回调翻译事件
- 通用 `/api/events` SSE

---

## 六、官方客户端兼容层

- **CommandOrigin 贯穿**：命令真实 Session 贯穿网络 → session actor → room actor → 响应
- **双 deadline**：commit/response 预算分离（`commit_response_reserve_ms`）
- **Outbound Gate**：认证屏障缓冲、快照 cutover 原子切换、事件 `room_seq` 绑定
- **最小响应延迟**：默认 0（官方客户端 rcall 无竞态）
- **协议 Hack**：固定顺序补偿（ChangeHost→ChangeState→PersistentRoom→Replay）
- **Golden 协议测试**：59 个协议变体判别值/字节布局固定
- 错误语义：deadline 过期、origin 过期、幂等冲突等均有明确错误码

---

## 七、持久化与数据安全

- **WAL**：所有权威事件先写 WAL（`data/persistence-worker.wal.jsonl`），带 SHA-256 checksum
- **fail-closed**：WAL 损坏/恢复不确定 → 拒绝启动/认证，不静默丢数据
- **PostgreSQL**：`mp_users`、`mp_rounds`、房间快照、回放、高频数据
- **原子恢复**：启动 replay、dead-letter 重放、abort 未完成 round
- **参数化 SQL**：全部防注入
- **关闭校验**：持久化未确认 → 非零退出码

---

## 八、HTTP / SSE API（对外）

- `/api/rooms`：房间列表（兼容 gooophira/tphira-mp 格式：`rooms/total` + `roomid/cycle/lock/host/state/chart/players`）
- `/api/rooms/listen`：SSE 房间事件流（create_room/update_room/join_room/leave_room/player_score/start_round）
- `/api/events`：通用 SSE
- 管理 API（token 鉴权，`X-Admin-Token`/`Authorization`）

---

## 九、可观测性

- **ProtocolTrace metrics**：请求/响应时序、认证屏障、慢路径、异常路径计数器（20+ 项）
- **响应延迟直方图**：命令处理耗时分布
- **Sentry**：错误/panic 上报（可选 feature，Build 产物带，Release 默认关）
- **协议追踪**：request_received → response_queued → flushed

---

## 十、本地化

- Fluent 三语：zh-CN / zh-TW / en-US
- 系统消息、结算排行、封禁理由、CLI 输出按用户语言

---

## 十一、性能

- **High-Frequency Writer**：Touches/Judges 批量写 PostgreSQL（独立 worker）
- **基准测试模式**：`benchmark` 子命令
- 延迟优化：握手/响应路径的 deadline 与 gate 设计

---

## 十二、网络与 PROXY 协议

- **PROXY 协议 v1/v2**（HAProxy 标准）：反向代理 / 负载均衡后获取**真实客户端 IP**
  - v1（文本）：`PROXY TCP4 192.168.1.1 10.0.0.1 12345 80\r\n`
  - v2（二进制）：12 字节签名 + 地址族数据
- **可信 CIDR**（`proxy_trusted_cidrs`）：仅对可信来源解析 PROXY 头，非可信来源跳过（防伪造）
- 非消费式 peek 解析（不破坏非 PROXY 直连）
- 真实 IP 用于：IP 封禁、速率限制、认证记录
- `trusted_forwarded_http_port`：HTTP 端口透传（与游戏端口分离）

---

## 部署差异（相对官方）

| 项 | 官方 | PMP |
|---|---|---|
| 数据库 | 无 | PostgreSQL 必填（`database_url`/`PM_DATABASE_URL`） |
| 持久化文件 | 无 | `data/`（WAL、快照、回放） |
| 插件 | 无 | `plugins/`（WASM） |
| 端口 | 游戏端口 | 游戏 + HTTP 双端口 |
| 依赖 | 仅 Rust | PostgreSQL + 可选 Sentry |
