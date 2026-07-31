# PMP `(35)` 最终生产就绪审计报告

**PMP 版本：** `0.5.1877`  
**审计对象：** 本轮上传源码包  
**对比基线：** PMP `(34)` / `0.5.1871`  
**CI 状态：** **已通过**。按用户确认，本次上传项目已通过项目既有的 check、tests、Clippy 与 release build CI 门禁  
**静态校验：** 11 个 TOML、6 个 YAML/YML、2 个 JSON 均可解析；21 个 Markdown 文件未发现失效本地链接  
**代码规模：** Server 141 个 Rust 文件，约 46,177 行  
**相对 `(34)`：** 13 个文件变化；其中 12 个属于代码、迁移或依赖版本变更，另新增上一版审计文档  
**发布结论：** **NO-GO，继续作为 Development Preview**

---

# 1. 技术摘要

PMP35 的修改范围很集中，主要涉及：

- PersistenceWorker 代码拆分；
- WAL scanner 与 in-flight 登记；
-用户 Session 实例识别；
-playtime crash recovery；
- Plugin TCP callback 并发；
-版本和迁移。

本版取得的真实进展包括：

- 将单事件数据库/DLQ处理从 `worker.rs` 拆到 `persistence/process.rs`；
- recovery scanner 已改成在发送事件前登记 `in_flight`，关闭了上一版明确的 ACK 后反向残留竞态；
-新增全局 `server_instance_id`；
- `playtime` 表新增 `server_instance_id`；
-登录事务会记录当前实例 ID；
- stale session cleanup 开始只清理旧实例 Session；
- Plugin TCP callback 增加最大 4 个并发执行基础；
- Server 与 Plugin SDK 版本保持一致。

但是，核心持久化状态机的行为并没有随着文件拆分而改变：

```text
事件进入 WAL
→数据库失败
→DLQ 也失败
→事件不是 durable terminal
→Worker仍然推进 next_expected_sequence
→后继事件可以先提交
```

同时：

```text
Flush/Shutdown 进入延期状态
→第一次复查仍未完成
→ pending_control.take() 取走控制对象
→没有重新放回
→reply sender 被销毁
→调用方收到 acknowledgement dropped
```

此外，新增的 `server_instance_id` 是在**事件处理时**从当前进程全局变量读取，而不是在事件产生时写入 `UserAuthenticated`。因此旧 WAL/DLQ 中的历史认证事件在新实例重放时，会被错误标记为当前实例的在线 Session。

Persistent Room recovery 仍发生在 Room mailbox 启动之前，当前恢复路径依然无法真正完成。

Plugin TCP 本轮的并发改造还引入了一个资源治理回归：虽然同时执行 callback 的数量限制为 4，但 event worker 会先为每个事件创建 Tokio task，再在 task 内等待 Semaphore。事件队列会被快速清空，慢插件期间可积累大量等待 permit 的 task。

因此 PMP35 仍不具备生产发布条件。

---

# 2. CI 结论

用户已确认所有上传 PMP 项目包 CI 均通过，本报告将以下门禁视为已确认：

```text
cargo check
cargo test
cargo clippy -D warnings
release build
```

CI 证明当前代码可编译、现有测试通过、Clippy 和发布构建正常。

但现有测试仍未覆盖本报告发现的生产状态机问题：

-数据库和 DLQ 同时失败；
-WalOnly 与普通 queued event 交错；
-延期 Flush/Shutdown 至少经历两次复查；
-历史 UserAuthenticated 在新实例重放；
-旧 UserOffline 在新 Session 建立后重放；
-Persistent Room 在完整启动流程中的恢复；
-Plugin TCP 慢 callback + 高频远端输入；
-WAL append 失败形成 sequence gap；
-运行期 WAL checksum/JSON 损坏。

CI 通过不改变当前生产 No-Go 判定。

---

# 3. PMP34 → PMP35 修复状态

| PMP34 问题 | PMP35 状态 | 判断 |
|---|---|---|
| scanner 发送后才登记 `in_flight` | 改为发送前登记，失败回滚 | **已关闭** |
|持久化处理逻辑过度集中在 Worker |拆出 `persistence/process.rs` | **结构改善，不改变语义** |
|没有服务实例 ID |新增全局实例 ID和数据库字段 | **基础完成，事件代际未闭环** |
|停机后 Session无法区分实例 | stale cleanup按实例筛选 | **部分完成** |
|非 durable事件仍推进sequence |未修改 | **Blocker** |
|延期PendingControl被丢失 |未修改 | **Blocker** |
|gate初始化排除低序号in-flight |未修改 | **Blocker** |
|Persistent Room恢复早于mailbox启动 |初始化顺序未修改 | **Blocker** |
|WAL append失败消耗sequence |未修改 | **Blocker** |
|运行期`list_pending()`损坏fail-open |未修改 | **Blocker** |
|initial replay仅表示deque耗尽 |未修改 | **Blocker** |
|Plugin TCP callback 串行等待 |改为4并发task | **执行并发改善，但引入无界等待task风险** |

---

# 4. 已真正关闭的问题

## 4.1 Scanner 的 in-flight 登记竞态

上一版 scanner 是：

```text
try_send
→再登记 in_flight
```

Worker可能先提交并ACK，随后scanner再插入，造成已ACK ID永久残留。

PMP35改为：

```text
先登记 in_flight
→ try_send
→发送失败则回滚
```

这一竞态已经正确修复。

## 4.2 用户 Session 开始带服务实例概念

新增：

```text
server_instance_id
```

并在进程启动时生成 UUID。

`playtime` 表新增同名字段，正常实时认证提交时会记录当前实例。

stale cleanup也开始仅关闭：

```text
server_instance_id != current instance
```

这为正确的崩溃恢复建立了必要基础。

## 4.3 Persistence处理逻辑拆分

数据库提交、DLQ保留和WAL ACK逻辑被移到：

```text
persistence/process.rs
```

Worker文件明显缩小。

这是有价值的可维护性改进，但当前只是物理拆分，未改变 terminal outcome 的返回模型。

---

# 5. P0-01：非 durable 事件仍然推进 WAL sequence

`process_event_through_pipeline()` 仍然返回：

```rust
bool
```

该布尔值只表示事件是否为旧式 `PersistenceEvent::Shutdown`。

函数内部会计算：

```rust
let mut durable = false;
```

但不会把 durable 结果返回给 Worker。

Worker无论事件是否：

-数据库成功；
-DLQ成功；
-数据库和DLQ全部失败；

都会在处理后执行：

```rust
next_expected_sequence = wal_sequence + 1;
```

## 5.1 故障链

```text
seq100 UserAuthenticated
→数据库失败
→DLQ文件也不可写
→ durable=false
→WAL不ACK
→仍留在in_flight
→Worker推进到seq101

seq101 UserOffline
→数据库成功
→WAL ACK
```

重启时只有seq100仍待重放：

```text
UserAuthenticated重放
→用户重新online
→playtime.session_start重新设置
```

最终状态与真实生命周期相反。

## 5.2 文件拆分没有修复问题

`process.rs` 的注释已经正确识别：

```text
Only durable events get WAL ACKed
```

但Worker的sequence状态仍不知道此次处理是否durable。

因此当前保证仍然只是：

> 按WAL顺序进行一次处理尝试。

不是：

> 按WAL顺序达到durable terminal。

## 5.3 正确接口

应返回：

```rust
enum ProcessOutcome {
    DatabaseCommitted,
    DurableDeadLetterStored,
    PendingWalAck,
    RetryableFailure,
    FatalFailure,
    Shutdown,
}
```

只有前三种可以推进sequence。

`RetryableFailure`必须保持当前expected sequence，并在当前进程退避重试。

---

# 6. P0-02：延期 Flush/Shutdown 的 reply 仍会被丢失

Worker每轮顶部：

```rust
if let Some(pc) = pending_control.take() {
```

如果检查结果：

-仍有WAL pending；
-仍有buffer；
-仍有pending ACK；
-deadline尚未到；

代码既不回复，也没有：

```rust
pending_control = Some(pc);
```

控制对象离开作用域时，里面的oneshot sender被销毁。

## 6.1 确定性复现

```text
一个WalOnly事件仍未被scanner送入
→调用Flush
→首次检查发现wal_pending > 0
→保存PendingControl

下一轮take PendingControl
→100ms内事件仍未terminal
→deadline未到
→没有重新存回
→reply sender被drop
```

调用方收到：

```text
persistence flush acknowledgement was dropped
```

而不是等待目标事件完成。

Shutdown存在同样问题。

## 6.2 修复

不要消费PendingControl所有权后再决定是否保存。

可使用：

```rust
if let Some(control) = pending_control.as_mut()
```

或者显式状态机：

```text
Waiting
→Succeeded
→TimedOut
→Failed
```

未完成时必须继续保留原reply sender。

---

# 7. P0-03：Sequence gate 初始化仍可能跳过低序号事件

Worker接收第一个普通channel event时，会从WAL读取pending，并排除所有：

```text
in_flight IDs
```

再计算最小sequence。

场景：

```text
seq100最初WalOnly
seq101正常Queued
scanner稍后把seq100发到channel尾部
seq100和seq101都已登记in_flight

channel顺序：
seq101
seq100
```

Worker收到seq101并初始化expected：

```text
WAL pending中的seq100、seq101都因in_flight被排除
→没有候选
→expected = 101
```

随后：

```text
seq101先执行
seq100到达时被当作stale跳过
```

WAL顺序仍然被破坏。

## 修复

expected必须来自：

```text
最小未达到terminal的durable WAL sequence
```

不能通过排除in_flight猜测channel内部顺序。

建议由Worker单线程维护：

```text
pending
processing
pending_ack
terminal
```

而不是让queue/scanner/Worker共同修改一个HashSet。

---

# 8. P0-04：WAL append失败仍会制造永久sequence gap

当前admit顺序：

```rust
seq = admit_sequence.fetch_add(1) + 1;
append_frame(...).await?;
```

如果磁盘写入、flush或fsync失败：

```text
sequence N已经被消耗
WAL中却没有N
```

磁盘恢复后下一事件得到N+1。

如果Worker正在等待N：

```text
N+1进入buffer
N永远不会出现
```

Worker、Flush和Shutdown都可能永久阻塞。

## 修复

sequence分配必须与WAL append放在同一个串行临界区：

```text
读取candidate sequence
→写frame并fsync
→成功后提交counter
```

或者为失败sequence写入durable tombstone。

---

# 9. P0-05：运行期 WAL 检查仍会跳过损坏记录

`list_pending()` 对以下内容直接 `continue`：

- JSON反序列化失败；
-未知未来版本；
-checksum失败。

而Flush、Shutdown和scanner依赖该函数判断WAL pending。

结果可能是：

```text
WAL有损坏且未完成的关键Admission
→ list_pending跳过
→控制逻辑认为pending=0
→ Flush返回成功
```

Worker调用处还存在：

```text
Err(_) => 0
```

和：

```text
unwrap_or(0)
```

进一步把读取失败伪装成没有pending。

## 修复

运行期发现非尾部截断错误时必须：

```text
WAL degraded
→停止成功Flush/Shutdown
→报告critical failure
→拒绝新的关键admission
```

数据库事实和WAL状态不能在损坏时fail-open。

---

# 10. P0-06：Initial replay drained仍不等于durable terminal

`initial_replay_drained=true`的条件仍是：

```text
replay VecDeque被pop空
```

如果replay事件：

```text
数据库失败
DLQ也失败
WAL不ACK
```

它仍然已经从deque中移除。

Worker随后会报告initial replay drained，Recovery可能继续：

-处理Round；
-清理playtime；
-恢复房间；
-最终Ready。

正确状态应区分：

```text
Parsing
Processing
PendingAck
Complete
Failed
```

只有所有初始事件均达到durable terminal并完成必要ACK后，才能Complete。

---

# 11. P0-07：新增 server_instance_id 没有进入持久化事件

`UserAuthenticated` 当前包含：

```text
event_id
session_id
user_id
connected_at
```

但不包含：

```text
server_instance_id
```

数据库事务处理事件时直接调用：

```rust
crate::server_instance::current()
```

## 11.1 历史认证重放会绑定到新实例

旧实例崩溃前：

```text
UserAuthenticated进入WAL
数据库尚未提交
```

新实例启动后重放该事件：

```text
commit_user_authenticated
→读取新进程current instance ID
→把历史Session写成当前实例Session
```

随后stale cleanup使用：

```text
server_instance_id != current
```

过滤旧Session。

由于该历史Session已经被错误写成current，它不会被清理。

最终产生phantom online：

-用户实际没有连接；
- `session_start`非NULL；
-排名和在线时间继续增长；
-新实例认为该Session属于自己。

## 11.2 正确设计

事件产生时必须写入：

```rust
UserAuthenticated {
    server_instance_id,
    ...
}
```

WAL/DLQ重放必须使用原始实例ID。

数据库只能把**当前实例且仍被Session registry确认存活**的Session视为online。

---

# 12. P0-08：UserOffline/UserDisconnect没有Session代际保护

当前：

```rust
UserOffline { user_id }
UserDisconnect { user_id, user_name }
```

都没有：

- `session_id`；
- `server_instance_id`；
-session generation。

`set_offline()`也只按：

```sql
WHERE user_id = $1 AND session_start IS NOT NULL
```

更新。

## 故障场景

```text
旧Session的UserOffline在WAL/DLQ中延迟
用户已经建立新Session
新UserAuthenticated已提交

旧UserOffline随后重放
→直接关闭新Session的playtime
```

同样，旧UserDisconnect可以在新连接后更新：

```text
last_disconnected_at
last_seen_at
updated_at
```

导致时间反序。

## 修复

所有Session生命周期事件必须携带：

```text
session_id
server_instance_id
generation
occurred_at
```

数据库更新条件必须匹配当前Session代际：

```sql
WHERE user_id = ?
  AND session_id = ?
  AND server_instance_id = ?
```

旧事件只能关闭其所属旧Session，不能影响新Session。

---

# 13. P0-09：server_instance_id仍没有解决停机时间误计

`close_all_stale_sessions()`仍然使用：

```text
startup_now - session_start
最多计入1小时
```

新增实例ID只能判断Session来自旧实例，不能确定旧实例何时最后存活。

如果服务器停机20分钟：

```text
这20分钟仍计入playtime
```

1小时cap只是限制误差，不是正确计时。

需要：

- server instance lifecycle表；
-实例heartbeat/last_alive；
-或Session heartbeat；
-恢复时累计到旧实例最后确认存活时间。

---

# 14. P0-10：Persistent Room recovery依然发生在mailbox启动前

初始化顺序未改变：

```text
recover_state()
→ room_commands.start_mailbox()
```

Recovery内部：

```text
restore_persistent_rooms
→ create_empty_room
→ init_empty_room
→ room_mailbox
```

`room_mailbox_sender()`需要`state_ref/self_ref`，它们只在`start_mailbox()`中设置。

因此恢复阶段：

```text
room_mailbox_sender = None
→init_empty_room失败
→create_empty_room回滚
```

Persistent Room无法真正恢复。

## 修复

在Recovery前执行：

```rust
state.room_commands.start_mailbox(...)
```

或者提供不依赖普通runtime mailbox的bootstrap API。

恢复失败若功能已启用，不应只warning后Ready。

---

# 15. P0-11：Round completion安全查询仍吞掉数据库错误

Recovery调用：

```rust
db.has_round_completion_event(...).await
```

接口仍返回bool，并将SQL错误折叠为false。

数据库短暂错误会被解释为：

```text
没有round.completed事件
```

随后可能把真实已完成Round标记aborted。

应改为：

```rust
Result<bool, DbError>
```

错误必须阻止Recovery继续。

---

# 16. UserAuthenticated幂等冲突仍未完整处理

`mp_user_visits.session_id`已有UNIQUE约束。

INSERT仍为：

```sql
ON CONFLICT (event_id) DO NOTHING
```

同一session_id但不同event_id会触发唯一约束错误，事务反复失败并进入DLQ。

更稳妥：

```sql
ON CONFLICT DO NOTHING
RETURNING ...
```

随后读取冲突记录并核对：

- user_id；
-session_id；
-event_id；
-connected_at；
-server_instance_id。

逻辑一致则视为幂等成功；不一致则报告数据完整性错误，不能无休止重试。

---

# 17. P0-12：Plugin TCP并发改造会积累无界等待task

本轮增加：

```text
MAX_CONCURRENT_CALLBACKS = 4
Semaphore
```

但event worker的行为是：

```text
从有界queue持续pop
→每条事件tokio::spawn
→task内部等待Semaphore
```

这只限制：

```text
同时执行callback的数量
```

不限制：

```text
已经spawn、正在等待permit的task数量
```

由于worker会快速清空queue，queue的64条上限不再形成真正背压。

远端持续发送时可以形成：

```text
大量等待Semaphore的Tokio task
```

造成：

-内存增长；
-task调度压力；
-payload长期占用；
-插件卸载后大量滞后回调。

## 正确实现

可选方案：

### 方案A：固定4个worker

```text
bounded queue
→4个固定consumer
→每个直接await callback
```

### 方案B：入队前获取owned permit

但permit必须在创建task前取得，不能先spawn再等待。

还应限制：

- pending event数量；
-pending bytes；
-每连接速率；
-lifecycle event优先级。

---

# 18. Plugin TCP生命周期优先级仍需治理

当前队列满时采用：

```text
drop oldest
```

但没有区分：

- `tcp:receive`
- `tcp:accept`
- `tcp:disconnect`
- `tcp:error`

普通数据事件可能挤掉disconnect/error，导致插件侧资源状态失真。

建议：

- receive可丢或合并；
-disconnect/error不可被receive挤掉；
-生命周期事件独立高优先级队列；
-增加dropped_receive和dropped_lifecycle指标。

---

# 19. HighFrequency当前状态

PMP35本轮没有针对HighFrequency作实质修改，PMP34的改进保持：

-实际sequence interval；
-overflow过期标记dropped；
-DB失败标记dropped；
-Flush调用时target；
-Worker内Flush deadline；
-明确terminal检查。

剩余P0仍是：

## Shutdown没有把调用方deadline传入Worker

`Shutdown`数据库重试仍可能超过外部等待时间。

## 单次数据库调用缺少remaining-time timeout

即使循环检查deadline，一次SQL/COPY调用本身也可能超时过晚。

因此仍需：

```text
Shutdown { target, absolute_deadline }
```

并给每次数据库调用套用剩余时间timeout或数据库statement timeout。

---

# 20. 静态检查结果

本轮完成：

- 11/11 TOML解析；
-6/6 YAML/YML解析；
-2/2 JSON解析；
-21个Markdown本地链接检查；
-无失效本地链接；
-Server和Plugin SDK版本一致；
-源码差异和关键数据链路静态追踪。

版本：

```text
Server      0.5.1877
Plugin SDK  0.5.1877
```

`git diff --check`仅在仓库内附带的上一版审计Markdown中发现一处尾随空格，不属于运行代码问题。

---

# 21. 当前评分

| 领域 | 评分 | 说明 |
|---|---:|---|
|架构方向 | 9.2/10 |改动集中于生产一致性 |
|CI与构建 | 10/10 |用户确认CI通过 |
|代码可维护性 | 8/10 |单事件处理已拆出Worker |
|durable terminal sequence | 3/10 |处理结果仍未反馈给sequence gate |
|PendingControl | 2/10 |确定性reply丢失未修 |
|WAL ordered execution | 4/10 |scanner竞态修复，但gate初始化和gap仍错误 |
|WAL运行期完整性 | 4/10 |list_pending仍跳过损坏 |
|Initial replay | 5/10 |deque drained不等于terminal |
|用户认证幂等 | 8/10 |visit事务保持，但实例/Session代际未闭环 |
|playtime recovery | 5/10 |实例字段有基础，历史重放和停机时间仍错误 |
|Persistent Room recovery | 2/10 |mailbox启动顺序未修 |
|Round recovery | 7/10 |顺序保持改善，completion查询仍fail-open |
|Plugin TCP | 6/10 |并发增加，但等待task可无界增长 |
|HighFrequency | 7.5/10 |上一版改善保持，Shutdown deadline仍不足 |
|客户端协议 | 8.5/10 |本轮未发现新的核心Phira帧顺序回归 |
|当前阶段 | Development Preview |仍不能进入Production Candidate |

---

# 22. PMP35 Core P0任务清单

## P0-A：返回durable terminal outcome

- [ ] `process_event_through_pipeline()`返回结构化Outcome
- [ ] DatabaseCommitted推进sequence
- [ ] DurableDeadLetterStored推进sequence
- [ ] PendingWalAck进入ACK状态
- [ ] RetryableFailure不得推进sequence
- [ ] FatalFailure触发Supervisor和not-ready
- [ ] DB+DLQ同时失败测试
- [ ] 后继事件不可越过测试

## P0-B：修复PendingControl

- [ ]未完成时保留原PendingControl
- [ ] reply sender不得被drop
- [ ] Flush和Shutdown分离状态
- [ ]同时多个Flush的策略明确
- [ ] Flush等待WalOnly测试
- [ ] Shutdown等待ACK测试
- [ ] deadline到期只回复一次

## P0-C：重建有序Worker状态模型

- [ ] expected取最小未terminal sequence
- [ ]不得排除尚未处理的in-flight低sequence
- [ ] stale sequence必须验证ACK/terminal
- [ ] duplicate sequence fail-closed
- [ ] processing状态由Worker单线程拥有
- [ ] scanner只发送wake或WAL hint
- [ ] WalOnly/Queued交错测试
- [ ]当前进程自动重试non-durable event

## P0-D：消除WAL sequence gap

- [ ] sequence分配与append+fsync串行提交
- [ ] append失败不推进counter
- [ ]或写durable tombstone
- [ ]磁盘满后恢复测试
- [ ] fsync失败后下一admission测试
- [ ]不存在sequence永久等待测试

## P0-E：运行期WAL fail-closed

- [ ] `list_pending()`遇到损坏返回Err
- [ ] checksum失败设置degraded
- [ ]未知版本设置degraded
- [ ]非尾部JSON错误设置degraded
- [ ]控制路径不得把Err当0
- [ ] `pending_wal_count()`返回Result
- [ ] Flush/Shutdown在WAL错误时失败
- [ ]运行期损坏测试

## P0-F：Initial replay terminal状态

- [ ] Parsing/Processing/PendingAck/Complete/Failed
- [ ] deque为空不能直接Complete
- [ ] non-durable replay阻止healthy
- [ ] pending ACK阻止healthy
- [ ] recovery timeout配置化
- [ ]进度指标
- [ ]大WAL恢复测试

## P0-G：Session代际完整持久化

- [ ] UserAuthenticated携带server_instance_id
- [ ] UserOffline携带session_id与instance_id
- [ ] UserDisconnect携带session_id与instance_id
- [ ]数据库条件更新当前Session代际
- [ ]历史事件不得关闭新Session
- [ ]旧Auth replay不得标记当前online
- [ ] Session generation冲突测试
- [ ] reconnect/replay顺序测试

## P0-H：正确playtime crash recovery

- [ ] server instance lifecycle表
- [ ] instance heartbeat/last_alive
- [ ]恢复累计到旧实例最后存活时间
- [ ]停机时间不计入playtime
- [ ] current instance历史replay防护
- [ ] 20分钟停机测试
- [ ] SIGKILL测试

## P0-I：Persistent Room启动恢复

- [ ] Recovery前启动RoomCommandGateway
- [ ]或实现bootstrap Actor API
- [ ]恢复失败按配置fail-closed
- [ ] snapshot查询返回Result<Option<_>>
- [ ] lock/cycle/hidden/chart恢复测试
- [ ]恢复完成后RoomSnapshot对账
- [ ] host在空房中的语义明确

## P0-J：Plugin TCP真实有界执行

- [ ]不要先spawn再等待Semaphore
- [ ]固定数量async worker
- [ ] queue容量真正约束pending callback
- [ ] pending bytes限制
- [ ] receive与lifecycle分级
- [ ] lifecycle不可被普通receive丢弃
- [ ]慢插件+持续远端输入压力测试
- [ ] unload取消等待任务

## P0-K：HighFrequency Shutdown deadline

- [ ] Shutdown携带target与absolute deadline
- [ ] deadline传入数据库retry
- [ ]单次DB调用受remaining timeout
- [ ]外部timeout后Worker不继续无界工作
- [ ] shutdown失败后状态可重试或明确终止
- [ ] DB outage 30秒测试

---

# 23. P1任务清单

## User persistence

- [ ] session_id冲突改为可验证幂等
- [ ] conflict后读取已有visit核对字段
- [ ] IP写入是否允许best-effort明确
- [ ] total visits与`mp_user_visits`周期对账
- [ ]旧UserOnline/UserSeen路径清理

## Round recovery

- [ ] `has_round_completion_event()`返回Result
- [ ] DB错误阻止abort
- [ ] RoundCompleted重放集成测试
- [ ] abnormal completed/aborted invariant修复

## WAL维护

- [ ]恢复counter使用历史最大sequence或明确局部语义
- [ ] compact与新admit并发测试
- [ ] marker与WAL删除策略测试
- [ ] WAL状态机独立模块

## Plugin TCP

- [ ] dropped事件指标
- [ ] pending event/byte指标
- [ ] per-connection rate
- [ ] Actor shutdown cancellation
- [ ] callback timeout
- [ ] disconnect一致性测试

##代码结构

- [ ] Worker loop拆分为gate/control/ACK模块
- [ ] ProcessOutcome独立类型
- [ ] Recovery使用阶段状态机
- [ ] server_instance模块增加持久状态
- [ ]删除与真实保证不一致的注释

---

# 24. 必须新增的生产门禁测试

## 24.1 Durable terminal

```text
seq1数据库失败
seq1 DLQ失败
seq2数据库成功
```

断言seq2不得越过seq1。

## 24.2 Deferred Flush

```text
一个WalOnly
→调用Flush
→至少经历两次100ms复查
```

断言reply不丢失，最终只返回一次。

## 24.3 Gate初始化

```text
seq1 WalOnly
seq2 Queued
scanner把seq1放在channel尾部
```

断言执行顺序仍为seq1、seq2。

## 24.4 WAL gap

```text
sequence N的append/fsync失败
磁盘恢复
下一事件进入
```

断言Worker不会等待不存在的N。

## 24.5 Runtime WAL损坏

```text
运行中破坏一条checksum
立即Flush
```

断言degraded且Flush失败。

## 24.6 Historical Auth replay

```text
旧实例UserAuthenticated留在WAL
新实例启动并重放
```

断言旧Session不会被标记为当前在线。

## 24.7 Old Offline replay

```text
用户建立新Session
旧UserOffline随后重放
```

断言新Session不受影响。

## 24.8 Persistent Room

```text
保存persistent room与snapshot
重启
```

断言房间和控制状态恢复。

## 24.9 Plugin TCP

```text
远端持续发送
callback每次1秒
持续60秒
```

断言Tokio task数量、queue、内存保持有界。

## 24.10 HighFrequency Shutdown

```text
数据库中断30秒
调用Shutdown
```

断言Worker在absolute deadline内返回明确结果。

---

# 25. Go / No-Go上线门槛

PMP只有满足以下条件才能进入Production Candidate。

##持久化状态机

- [ ]只有durable terminal推进sequence
- [ ] PendingControl不会丢失
- [ ] WalOnly/Queued严格按WAL顺序
- [ ] sequence append失败不制造gap
- [ ] WAL损坏运行期fail-closed

##恢复

- [ ] initial replay真正terminal
- [ ]历史Auth不产生phantom online
- [ ]旧Offline不影响新Session
- [ ]停机时间不计入playtime
- [ ] Persistent Room真实恢复

##资源与高频

- [ ] Plugin TCP callback pending数量有界
- [ ]生命周期事件不被普通数据淹没
- [ ] HighFrequency Shutdown deadline进入Worker和DB调用
- [ ]慢插件/慢数据库不破坏核心运行时

##数据库一致性

- [ ] Session generation条件更新
- [ ] `mp_user_visits`冲突可验证幂等
- [ ] Round completion查询错误fail-closed
- [ ]访问计数、在线状态和playtime重启后对账

---

# 26. 最终判断

PMP35 是一次有价值的重构和补强。

已经关闭：

```text
scanner发送后登记in_flight竞态
缺少server_instance_id基础字段
Plugin TCP完全串行callback
持久化处理逻辑过度集中
```

但核心持久化保证仍然没有成立：

```text
WAL durable顺序
≠
durable terminal顺序
```

PendingControl确定性丢reply、sequence gap、运行期WAL fail-open、历史Session代际错误和Persistent Room启动顺序也仍然存在。

Plugin TCP的新并发实现还把“有界队列积压”转换成了“可能无界的Semaphore等待task”。

因此最终结论：

> **PMP `(35)`：NO-GO，继续作为 Development Preview。**

下一轮应只验收以下五个闭环：

```text
ProcessOutcome与durable terminal sequence
→ PendingControl状态机
→ Session/instance generation
→ Persistent Room bootstrap recovery
→ Plugin TCP与HighFrequency真实deadline/有界执行
```

完成这些运行级门禁后，再重新评估是否进入Production Candidate。
