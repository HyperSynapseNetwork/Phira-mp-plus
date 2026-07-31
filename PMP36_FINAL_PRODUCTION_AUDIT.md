# PMP `(36)` 最终生产就绪审计报告

**PMP 版本：** `0.5.1913`  
**审计对象：** 本轮上传源码包  
**对比基线：** PMP `(35)` / `0.5.1877`  
**CI 状态：** **已通过**。按用户确认，本次上传项目已通过项目既有的 check、tests、Clippy 与 release build CI 门禁  
**静态校验：** 11 个 TOML、6 个 YML、2 个 JSON 均可解析；22 个 Markdown 文件、32 个本地链接未发现失效引用  
**代码规模：** Server 143 个 Rust 文件，约 47,007 行  
**相对 `(35)`：** 34 个文件变化，新增约 2,591 行，删除约 449 行  
**发布结论：** **NO-GO，继续作为 Development Preview**

---

# 1. 最终结论

PMP36 是目前为止持久化闭环最完整的一版。

PMP35 中列出的五个核心问题，大部分已经有真实代码实现，而不是仅增加字段或注释：

- `ProcessOutcome` 已真正返回给 Worker；
-普通运行期事件在 `RetryableFailure` 时不再推进 sequence；
- `PendingControl` 已改用 `as_mut()` 保持 reply sender；
-WAL sequence 分配已与 append/fsync 放入同一个临界区；
-运行期 `list_pending()` 已对中间损坏 fail-closed；
-初始 replay 增加了 pending ID 集合；
- `UserAuthenticated` 已携带原始 `server_instance_id`；
-增加 `mp_server_instances` 与 heartbeat；
-Persistent Room mailbox 已在 recovery 前启动；
-Plugin TCP 改成固定数量 callback worker；
-HighFrequency Shutdown 已携带 absolute deadline。

这些改动显著提高了 PMP 的生产可信度。

但当前仍存在五个直接阻断 Production Candidate 的问题：

```text
初始 WAL replay 中的前序事件 RetryableFailure
→ Worker 仍继续处理 replay deque 中的后续事件
→ 后续事件先提交
→ 前序事件之后被 scanner 重试
→ 最终状态可能反序
```

```text
Flush 在获得 send_gate 之前读取 target sequence
→ 一个已经先获得 send_gate 的 enqueue 正在写 WAL
→ Flush 读取旧 target
→ enqueue 完成并返回 WalOnly
→ Flush 不包含该事件
```

```text
WAL compact-to-zero 写 clean marker
→ 下一次启动把 clean marker 改成 active，但仍没有 WAL 文件
→ 本次运行没有新 admission
→ 再下一次启动把正常缺失 WAL 误判为人工删除
```

```text
初始 replay 的 ACK 或数据库处理发生瞬时失败
→ retry 依赖后续 channel message 或 10 秒级 scanner
→ startup recovery 只等待约 2.5 秒
→ 本可恢复的服务直接启动失败
```

```text
HighFrequency 已把 deadline 传到 retry loop
→ 但单次 COPY/INSERT 数据库调用本身没有 remaining-time timeout
→ DB call 卡住时仍可越过 Flush/Shutdown deadline
```

因此最终判定仍是：

> **PMP `(36)`：NO-GO，继续作为 Development Preview。**

但与前几版不同，当前主要问题已经从“大范围数据链不完整”收敛为少量状态机和故障恢复边界。

---

# 2. CI 结论

用户已确认所有上传的 PMP 项目文件 CI 均通过。

本报告将以下门禁视为已确认：

```text
cargo check
cargo test
cargo clippy -D warnings
release build
```

CI 证明：

-代码可以编译；
-现有测试全部通过；
-Clippy 门禁通过；
-Release 产物可生成。

但当前 `persistence_contracts.rs` 仍主要是事件构造和 kind/summary 级测试，没有覆盖：

- replay 前序事件失败；
- Flush target 与 enqueue 并发；
-clean marker 连续空闲重启；
-ACK failure 后无新消息；
-scanner 首次运行时间；
-同实例旧 Session 离线事件；
-Plugin TCP unload 后 worker 泄漏；
-HighFrequency DB call 卡住。

CI 通过不改变生产 No-Go 判定。

---

# 3. PMP35 → PMP36 修复状态

| PMP35 问题 | PMP36 状态 | 判断 |
|---|---|---|
| Pipeline 只返回 bool | 增加结构化 `ProcessOutcome` | **已关闭基础** |
| Retryable event 仍推进正常 sequence |普通 channel path 已不推进 | **已关闭普通路径，replay路径仍错误** |
| `PendingControl.take()` 丢失 reply | 改为 `as_mut()`，仅完成时 take | **已关闭** |
| gate 初始化排除低 sequence in-flight | 改为读取全部 pending sequence | **已关闭基础** |
| WAL append失败消耗sequence | sequence在io_gate内，append成功后store | **已关闭** |
|运行期WAL损坏被跳过 | `list_pending()`改为严格校验 | **已关闭基础** |
| initial replay只看deque | 增加`replay_pending_ids` | **部分关闭，replay仍可反序** |
| Auth事件无instance ID |事件已携带原始instance ID | **已关闭** |
| playtime无实例心跳 |新增实例表和heartbeat | **基础完成** |
| Persistent Room恢复早于mailbox | mailbox已提前启动 | **已关闭启动顺序** |
| Plugin TCP先spawn再等Semaphore |改为4个固定worker | **主要问题关闭，worker清理仍泄漏** |
| HF Shutdown无deadline |已携带deadline | **部分关闭，单次DB调用仍不受限** |

---

# 4. 已真正关闭的问题

## 4.1 普通运行期 durable terminal gate

`process_event_through_pipeline()` 现在返回：

```rust
ProcessOutcome
```

包括：

- `DatabaseCommitted`
- `DurableDeadLetterStored`
- `PendingWalAck`
- `RetryableFailure`
- `FatalFailure`
- `Shutdown`

普通 channel event 返回 `RetryableFailure` 时：

-不推进 `next_expected_sequence`；
-移出 `in_flight`；
-保留 WAL；
-等待 scanner 重试；
-后续高 sequence 进入 buffer。

这关闭了 PMP35 普通运行期事件失败后仍然越过 sequence 的核心问题。

## 4.2 PendingControl reply 不再在首次复查时被销毁

Worker 当前使用：

```rust
pending_control.as_mut()
```

只有达到：

- Ready；
- Deadline；
-WAL read error；

时才 `take()` 并回复。

此前的确定性：

```text
persistence flush acknowledgement was dropped
```

问题已经修复。

## 4.3 WAL append failure 不再永久消耗 sequence

当前：

```text
获取 io_gate
→ candidate sequence
→ append + flush + fsync
→成功后 store sequence
```

append 失败时 counter 不推进，下一次 admission 可以重试同一个 sequence。

## 4.4 运行期 WAL 中间损坏开始 fail-closed

`list_pending()` 现在：

-严格解析全部非尾部 frame；
-验证版本；
-验证 Admission 和 ACK checksum；
-错误时设置 `degraded=true`；
-向调用方返回 Err。

Flush/Shutdown 不再把 WAL 读取错误当成 pending=0。

## 4.5 UserAuthenticated 保留原服务实例

事件创建时已经写入：

```text
server_instance_id
```

数据库重放使用事件里的实例 ID，不再在 replay 时读取新进程的全局 instance ID。

这关闭了历史 Auth 事件直接被错误绑定到当前实例的主要路径。

## 4.6 playtime heartbeat 基础

新增：

```text
mp_server_instances
instance_id
created_at
last_alive_at
```

服务每 30 秒更新 heartbeat。

stale session cleanup 使用旧实例最后活跃时间，而不是新实例启动时间，正常情况下不会再把全部停机时间算进 playtime。

## 4.7 Persistent Room mailbox 启动顺序

当前：

```text
room_commands.start_mailbox()
→ recover_state()
```

`create_empty_room()` 和 snapshot control command 不再因为 `state_ref/self_ref` 尚未设置而必然失败。

## 4.8 Plugin TCP 固定 callback worker

每插件建立固定 4 个 worker，worker直接：

```text
pop event
→ timeout(await callback)
→继续下一条
```

不再为每个事件先创建一个等待 Semaphore 的 Tokio task。

## 4.9 HighFrequency Shutdown deadline

`HfMessage::Shutdown` 已携带：

```text
deadline_ms
```

retry loop 会使用调用方 deadline 与内部 max-age 的较早值。

---

# 5. P0-01：初始 WAL replay 仍会越过非 durable 前序事件

这是 PMP36 当前最严重的持久化顺序问题。

## 5.1 Replay event 被强制改成 sequence 0

初始 replay deque 中本来包含：

```text
wal_id
event
真实 wal_sequence
```

Worker pop 时却构造：

```rust
wal_sequence: 0
```

并用 sequence 0 sentinel 绕过正常 sequence gate。

## 5.2 RetryableFailure 后仍继续 pop 下一条 replay

场景：

```text
replay seq100：UserAuthenticated
→ DB失败
→ DLQ也失败
→ ProcessOutcome::RetryableFailure
→不推进普通gate
→但replay deque仍然保留seq101、seq102

下一轮：
→继续pop seq101
→seq101可以提交
```

等 replay deque耗尽后，scanner才会重新发送seq100。

最终执行顺序：

```text
seq101
seq102
seq100
```

而不是：

```text
seq100
seq101
seq102
```

## 5.3 实际影响

例如：

```text
seq100 UserAuthenticated
seq101 UserOffline
```

seq100失败、seq101成功后，scanner稍后重试seq100，最终用户重新变成online。

还可能影响：

- RoomSnapshot版本；
-用户房间历史；
-ServerEvent；
-Round生命周期；
-playtime；
-last_connected/last_disconnected。

## 5.4 正确修复

初始 replay 不应使用 sequence 0 绕过 gate。

应：

-保留 WAL 中的原始 sequence；
-与正常 channel event 使用同一个 ordered state machine；
-前序 `RetryableFailure` 时停止后续 replay；
-后续事件进入 buffer；
-只有前序达到 durable terminal后继续。

---

# 6. P0-02：Flush/Shutdown target 捕获在 send_gate 外部

`flush()` 当前：

```rust
let target = self.wal.current_sequence();
let _send_guard = self.send_gate.lock().await;
```

`shutdown()`同样先读 target，再获取 gate。

## 6.1 竞态场景

```text
任务A：enqueue已经取得send_gate
→正在执行WAL append
→sequence尚未store

任务B：调用Flush
→先读取旧target=N-1
→然后等待send_gate

任务A：
→WAL写入sequence N
→queue已满，返回WalOnly
→释放send_gate

任务B：
→取得send_gate
→发送Flush(target=N-1)
```

该 WalOnly 事件已经在 Flush control 插入前完成 admission，但没有包含在 target 中。

Flush可能直接返回成功，而 sequence N 仍只在 WAL。

## 6.2 为什么queued event不一定暴露问题

若任务A成功进入mpsc，channel FIFO通常会使event排在Flush control前。

但 WalOnly没有进入channel，只能靠target fence覆盖，因此这个竞态会直接漏事件。

## 6.3 修复

target必须在获得send_gate后捕获：

```rust
let _guard = send_gate.lock().await;
let target = wal.current_sequence();
send control(target);
```

这样所有先取得gate的enqueue都会在target读取前完成。

---

# 7. P0-03：WAL clean marker会在连续空闲重启中误报丢失

当前compact-to-zero流程：

```text
删除WAL文件
→写marker clean=true
```

下一次启动：

```text
marker存在
WAL不存在
clean=true
→check_instance_consistency把marker重写为clean=false
→允许启动
```

如果这次运行没有产生任何Persistence admission：

- WAL文件仍不存在；
-marker保持clean=false；
- `compact()`发现WAL NotFound时直接返回，不会重新写clean marker。

再下一次启动：

```text
marker clean=false
WAL不存在
→判定WAL被意外删除
→fail-closed
```

## 7.1 可复现场景

```text
运行A：所有事件ACK并compact到0
运行B：启动后无玩家、无房间写入，正常关闭
运行C：启动
```

运行C可能错误提示：

```text
WAL instance marker exists but WAL file is missing
```

## 7.2 正确语义

clean marker在没有新Admission前应保持：

```text
clean=true
```

第一次新Admission成功写入WAL后，`mark_marker_active()`再改成：

```text
clean=false
```

`check_instance_consistency()`遇到clean marker+missing WAL时不应主动改为active。

---

# 8. P0-04：初始 replay 的 retry/ACK 依赖后续消息，启动期无法及时自愈

## 8.1 Pending ACK只在处理到一条消息时重试

Worker 的 ACK retry代码位于：

```text
成功选出message之后
→dispatch之前
```

如果初始 replay事件：

```text
数据库已commit
WAL ACK失败
```

会：

-进入 `pending_acks`；
-WAL degraded=true；
-replay deque可能已经空；
-Worker随后等待 `rx.recv()`。

启动恢复期间没有外部业务消息，ACK不会被定时重试。

Recovery只等待约2.5秒：

```text
50 × 50ms
```

最终服务启动失败，即使磁盘错误已经恢复。

## 8.2 Retryable replay依赖scanner，但scanner过晚

scanner初始化：

```text
第一个tick立即
第二个tick等待5秒
进入loop后又tick
```

第一次真实scan约在启动后10秒。

Recovery只等待约2.5秒，因此初始 replay一旦需要scanner重试，服务必然先失败退出。

## 8.3 修复

Worker需要独立定时驱动：

```text
ACK retry timer
Retryable current event timer
```

不能依赖新channel message。

Recovery timeout应配置化，例如30秒，并显示：

-当前expected sequence；
-replay pending数量；
-pending ACK数量；
-last error；
-next retry时间。

scanner第一次真实scan也不应延迟到约10秒。

---

# 9. P0-05：Initial replay health仍可能只阻止Ready，不能保证顺序

`replay_pending_ids`确保：

```text
所有初始event达到terminal前
initial_replay_drained不为true
```

这比上一版正确。

但它只决定是否Ready，不会阻止后续 replay event先提交。

因此当前health保证是：

> 所有replay事件最终都处理过。

不是：

> replay事件按WAL顺序达到terminal。

必须与第5节的统一sequence gate一起修复。

---

# 10. P0-06：HighFrequency单次数据库调用仍可能越过deadline

HighFrequency现在会在每次retry前检查：

```text
effective_deadline
```

但实际数据库操作：

```rust
try_copy_write(...).await
record_runtime_telemetry_batches(...).await
```

没有使用：

```rust
tokio::time::timeout(remaining, ...)
```

如果：

-连接池等待；
-COPY卡住；
-网络半开；
-数据库statement长期阻塞；

单次await可能超过Flush或Shutdown的absolute deadline。

调用方虽然timeout返回，但Worker仍停在数据库调用中，后续：

-Item；
-Flush；
-Shutdown；

均无法处理。

## 修复

每次数据库尝试都应使用剩余deadline：

```text
remaining = deadline - now
timeout(remaining, DB call)
```

同时配置 PostgreSQL：

- connect timeout；
-acquire timeout；
-statement timeout。

---

# 11. P0/P1：Session代际仍只有instance级，没有连接session级

PMP36的改善：

- `UserAuthenticated`有`session_id`和`server_instance_id`；
- `UserOffline`有`server_instance_id`；
- `set_offline()`只关闭匹配实例的playtime。

但 `UserOffline` 和 `UserDisconnect` 没有：

```text
session_id
session generation
occurred_at
```

`playtime`表也没有当前session_id。

## 11.1 同实例旧事件风险

同一个服务实例内，用户快速断线、重连、旧异步事件延迟时：

```text
旧UserOffline(instance=A)
新Session(instance=A)
```

实例条件无法区分两个连接Session。

数据库仍可能关闭新Session的playtime。

严格WAL顺序可以降低此风险，但DLQ重放、延迟任务和未来并发路径仍需要真实session generation。

## 11.2 UserDisconnect完全忽略instance

事件带有 `server_instance_id`，但pipeline调用：

```rust
record_user_disconnect(user_id, user_name)
```

没有使用instance或session条件。

旧Disconnect可能在新连接后更新：

- `last_disconnected_at`
- `last_seen_at`
- `updated_at`

## 修复

所有Session生命周期事件应携带：

```text
session_id
server_instance_id
occurred_at
```

playtime保存当前session_id，Offline SQL同时匹配：

```text
user_id
server_instance_id
session_id
```

---

# 12. P1：Dead-letter payload丢失Offline/Disconnect实例信息

`PersistenceEvent`内包含：

```text
UserOffline.server_instance_id
UserDisconnect.server_instance_id
```

但 `dead_letter_payload()` 当前只写：

```json
{"user_id": ...}
```

和：

```json
{"user_id": ..., "user_name": ...}
```

Recovery重构缺失字段时回退：

```text
current server_instance_id
```

这会改变原始事件身份。

即使startup阶段通常尚未接受客户端，该行为仍会：

-丢失原始Session归属；
-使旧Offline无法正确关闭旧实例Session；
-或让legacy事件被错误绑定到当前实例。

Dead-letter必须是lossless representation，应写入完整instance/session信息。

---

# 13. P1：Plugin TCP固定worker中有3个无法卸载的detached task

每个插件创建4个event worker：

```rust
let mut worker_handles = Vec::with_capacity(4);
for _ in 0..4 {
    worker_handles.push(tokio::spawn(...));
}
```

之后只保存：

```rust
let worker_handle = worker_handles.pop().unwrap();
event_workers.insert(plugin_id, worker_handle);
```

其余3个 `JoinHandle` 被drop。

Drop `JoinHandle`不会取消Tokio task，任务会继续运行。

插件卸载时只abort保存的1个worker。

另外3个task仍持有：

- queue Arc；
- Notify Arc；
-callback Arc。

删除 `event_channels` 不会让这些task退出，因为task自身仍持有Arc。

## 13.1 后果

每次插件reload可能永久泄漏3个worker task。

旧worker还可能继续处理旧queue中的事件，并持有旧callback/PluginManager资源。

## 修复

保存全部JoinHandle：

```text
plugin_id → Vec<JoinHandle<()>>
```

或者使用：

- CancellationToken；
-channel closed signal；
-JoinSet。

卸载时：

```text
cancel
→ drain/丢弃策略
→ await worker退出
```

---

# 14. P1：Plugin TCP队列仍可能丢生命周期事件

`PluginEventChannel`满时统一：

```text
drop oldest
```

没有区分：

- `tcp:receive`
- `tcp:accept`
- `tcp:error`
- `tcp:disconnect`

大量receive可能挤掉disconnect/error，使插件认为连接仍存在。

建议：

- receive可丢、合并或按bytes限流；
-disconnect/error使用高优先级队列；
-生命周期事件不得被普通数据挤掉；
-分别统计 dropped_receive / dropped_lifecycle。

---

# 15. P1：多个并发Flush/Shutdown仍可能覆盖PendingControl

Worker只保存：

```rust
Option<PendingControl>
```

若第一个Flush已延期，期间又收到第二个Flush或Shutdown，dispatch分支会直接：

```rust
pending_control = Some(new_control)
```

旧控制对象的reply sender会被替换。

主关闭流程通常串行调用，当前风险较低，但管理操作、测试或未来调用方并发时仍会触发。

应：

-拒绝第二个control；
-合并target；
-或维护control queue。

---

# 16. P1：Flush内部deadline没有使用调用方timeout

`flush(timeout)`把调用方timeout只用于外部oneshot等待。

Worker内部固定使用：

```text
30秒
```

如果调用方只剩2秒shutdown budget：

-外部2秒后返回timeout；
-Worker仍保留Flush control最多30秒；
-随后Shutdown可能覆盖或与其竞争。

Flush message应与Shutdown一样携带absolute deadline。

---

# 17. P1：Server instance registration/heartbeat失败只记录warning

准确playtime crash recovery依赖：

```text
mp_server_instances.last_alive_at
```

但启动注册失败和heartbeat失败都只warning。

如果当前实例没有成功注册，下一次崩溃恢复会使用：

```text
COALESCE(last_alive_at, startup_now)
```

再次把停机时间计入playtime，最多受1小时cap限制。

建议：

-首次instance registration失败时fail-closed或not-ready；
-持续heartbeat失败进入degraded；
-暴露heartbeat age；
-shutdown final heartbeat使用共享deadline。

---

# 18. P1：Persistent Room恢复仍是部分best-effort

mailbox顺序已修复，但：

-单个房间创建失败只warning；
-snapshot查询仍以`Option`表示无数据和部分错误路径；
-lock/cycle/hidden/chart/host应用失败只warning；
-空房host通常没有对应member，set_host可能失败；
-恢复完成后没有snapshot对账。

如果Persistent Room属于正式承诺，应支持配置：

```text
required=true
```

并在关键恢复失败时not-ready。

---

# 19. P1：HighFrequency batch ID跨进程会重复

当前：

```text
batch_uuid = hf-{min_seq}-{max_seq}
```

HighFrequency sequence每次进程启动从1开始。

不同运行实例可能生成相同batch UUID。

当前主要幂等依赖每条record的随机event_id，batch UUID自身不能作为跨重启唯一键。

建议：

```text
hf-{server_instance_id}-{min_seq}-{max_seq}
```

并明确真正的数据库幂等约束。

---

# 20. WAL marker的其他可靠性问题

除clean→active误判外，marker写入使用：

```rust
tokio::fs::write
```

没有：

-临时文件；
-file fsync；
-parent fsync；
-原子rename。

WAL本体使用了更强的durability流程，但marker没有。

如果marker承担：

- accidental deletion detection；
-max_sequence high-water；
-clean状态；

它也应采用原子持久化。

---

# 21. 静态检查结果

本轮完成：

-11/11 TOML解析；
-6/6 YML解析；
-2/2 JSON解析；
-22个Markdown文件检查；
-32个本地Markdown链接检查；
-无失效本地链接；
-Server与Plugin SDK版本一致；
-关键持久化、恢复、Session、Plugin TCP和HighFrequency链路静态追踪。

版本：

```text
Server      0.5.1913
Plugin SDK  0.5.1913
```

---

# 22. 当前评分

| 领域 | 评分 | 说明 |
|---|---:|---|
|架构方向 | 9.4/10 |持久化设计继续收敛 |
|CI与构建 | 10/10 |用户确认CI通过 |
|普通durable terminal gate | 8.5/10 |Retryable普通路径已正确阻塞 |
|初始replay顺序 | 4/10 |失败前序仍可被后续replay越过 |
|PendingControl | 8/10 |reply丢失已修；并发control仍需治理 |
|WAL sequence分配 | 9/10 |append成功后才提交counter |
|WAL运行期完整性 | 8/10 |中间损坏已fail-closed |
|Flush/Shutdown fence | 6/10 |有target，但捕获点存在WalOnly竞态 |
|WAL marker生命周期 | 3/10 |连续空闲重启可误报WAL删除 |
|Initial replay恢复 | 5/10 |terminal集合改善，retry驱动和超时不足 |
|User认证持久化 | 8.5/10 |instance保真，session generation仍不足 |
|Playtime recovery | 8/10 |heartbeat基础形成，registration策略仍弱 |
|Persistent Room recovery | 7/10 |mailbox顺序修复，仍是best-effort |
|HighFrequency | 8/10 |Shutdown deadline已进入Worker，单次DB call仍可越界 |
|Plugin TCP | 7/10 |执行有界，卸载task泄漏和事件优先级仍存在 |
|客户端协议 | 8.5/10 |本轮未发现新的核心Phira帧顺序回归 |
|当前阶段 | Development Preview |尚不能进入Production Candidate |

---

# 23. PMP36 Core P0任务清单

## P0-A：统一初始replay与正常sequence gate

- [ ] replay保留真实wal_sequence
- [ ] replay不再使用sequence=0 sentinel
- [ ] RetryableFailure立即阻止后续replay
- [ ]后续replay进入同一BTreeMap buffer
- [ ]只有durable terminal推进expected
- [ ] Auth→Offline replay故障测试
- [ ] RoomSnapshot连续版本故障测试
- [ ] initial replay最终状态对账

## P0-B：修正Flush/Shutdown线性化点

- [ ]先获取send_gate
- [ ] gate内读取current_sequence
- [ ] gate内发送control
- [ ] enqueue已先取得gate时必须进入target
- [ ] WalOnly并发admission测试
- [ ] Shutdown使用相同线性化规则
- [ ] control success后target范围全部terminal

## P0-C：修复WAL clean marker状态机

- [ ] clean+missing WAL启动时保持clean
- [ ]首次新Admission成功后才切active
- [ ] compact NotFound时保持/恢复clean
- [ ] marker原子写+fsync+parent fsync
- [ ] compact-to-zero→空闲重启→再次重启测试
- [ ] marker损坏fail-closed测试
- [ ] high-water不回退测试

## P0-D：启动期独立retry驱动

- [ ] pending ACK独立timer重试
- [ ] Retryable current sequence独立退避
- [ ]不依赖新channel message
- [ ] scanner第一次扫描不晚于配置周期
- [ ] recovery timeout配置化
- [ ] recovery显示progress和last error
- [ ] transient ACK failure启动测试
- [ ] transient DB/DLQ failure启动测试

## P0-E：HighFrequency DB call deadline

- [ ]每次COPY使用remaining-time timeout
- [ ] fallback INSERT使用remaining-time timeout
- [ ] pool acquire timeout
- [ ] statement timeout
- [ ] Flush/Shutdown deadline统一
- [ ] DB半开连接测试
- [ ]调用方timeout后Worker可继续响应或明确终止

---

# 24. P1任务清单

## Session generation

- [ ] UserOffline增加session_id
- [ ] UserDisconnect增加session_id
- [ ] playtime保存current session_id
- [ ] Offline SQL匹配instance+session
- [ ] Disconnect记录occurred_at
- [ ] dead-letter payload保存instance/session
- [ ]同实例快速重连测试
- [ ]旧离线事件不影响新Session测试

## Plugin TCP

- [ ]保存全部worker JoinHandle
- [ ]使用CancellationToken
- [ ] unload等待worker退出
- [ ] lifecycle高优先级队列
- [ ] receive可丢/合并策略
- [ ] dropped分类指标
- [ ] reload 100次无task泄漏测试

## Persistence control

- [ ] Flush message携带caller deadline
- [ ]并发control拒绝/合并/排队
- [ ] ACK retry不依赖event
- [ ] control状态指标
- [ ] caller timeout后旧control清理

## Server instance

- [ ] registration失败按策略fail-closed
- [ ] heartbeat age指标
- [ ]连续heartbeat失败degraded
- [ ] shutdown final heartbeat遵守deadline
- [ ]缺失instance row恢复策略

## Persistent Room

- [ ] snapshot query返回Result<Option<_>>
- [ ] required模式fail-closed
- [ ] host恢复语义明确
- [ ]恢复后snapshot对账
- [ ]关键字段应用失败处理

## HighFrequency

- [ ] batch UUID加入server_instance_id
- [ ]明确event_id/batch_id幂等边界
- [ ] shutdown使用shared remaining budget
- [ ] main不再固定使用10秒而忽略remaining

---

# 25. 必须新增的生产门禁测试

## 25.1 Replay顺序

```text
WAL：
seq1 UserAuthenticated
seq2 UserOffline

seq1首次DB+DLQ失败
seq2数据库可用
```

断言seq2不能先于seq1提交。

## 25.2 Flush线性化

```text
enqueue先获得send_gate
WAL写入中
Flush读取target
enqueue最终WalOnly
```

断言Flush必须等待该WalOnly terminal。

## 25.3 Clean marker

```text
compact到零
重启且不产生任何PersistenceEvent
正常关闭
再次重启
```

断言不能误报WAL被删除。

## 25.4 Initial replay ACK

```text
replay事件DB成功
第一次WAL ACK失败
之后磁盘恢复
没有任何新业务消息
```

断言Worker主动重试ACK并完成启动。

## 25.5 Initial replay DB retry

```text
数据库启动慢5秒
WAL有pending event
```

断言在配置deadline内恢复，而不是2.5秒直接退出。

## 25.6 HighFrequency DB call

```text
COPY调用卡住20秒
Shutdown deadline10秒
```

断言Worker在10秒内结束或返回明确timeout，不得后台继续占用Actor。

## 25.7 Plugin reload

```text
加载插件
建立TCP event channel
卸载并重载100次
```

断言task数量和Arc资源不增长。

## 25.8 Session generation

```text
旧Session Offline延迟
同实例新Session已online
```

断言旧事件不能关闭新Session。

---

# 26. Go / No-Go上线门槛

PMP只有满足以下条件才能进入Production Candidate。

##持久化顺序

- [ ] initial replay与runtime使用同一个sequence state machine
- [ ] Retryable前序不被后继越过
- [ ] Flush/Shutdown target在线性化点内捕获
- [ ]所有target内Admission达到terminal

##恢复与WAL

- [ ] clean marker连续重启无误报
- [ ] ACK retry无需业务消息
- [ ] transient DB/WAL故障可在recovery deadline内自愈
- [ ] marker与WAL均使用原子durable写

##高频

- [ ]单次DB调用遵守absolute deadline
- [ ] Shutdown不在caller timeout后继续阻塞
- [ ] accepted/committed/dropped完整对账

##Session与资源

- [ ] Offline/Disconnect绑定具体session generation
- [ ]旧事件不能覆盖新Session
- [ ] Plugin TCP unload不遗留worker task
- [ ]生命周期事件不被receive数据淹没

---

# 27. 最终判断

PMP36 是一次实质性进步。

已经基本关闭：

```text
普通运行期non-durable事件推进sequence
PendingControl reply丢失
WAL append失败制造sequence gap
运行期WAL损坏fail-open
历史Auth绑定当前实例
Persistent Room mailbox启动顺序
Plugin TCP无界Semaphore等待task
HighFrequency Shutdown无deadline
```

当前剩余问题已经明显收敛，但都位于最关键的故障恢复边界：

```text
初始replay仍可反序
Flush target线性化竞态
clean marker连续重启误判
启动期retry缺少独立驱动
HighFrequency单次DB调用越过deadline
```

因此最终结论：

> **PMP `(36)`：NO-GO，继续作为 Development Preview。**

下一轮只需要围绕以下四项进行验收：

```text
统一initial replay sequence gate
→ gate内捕获Flush/Shutdown target
→修正clean marker生命周期
→独立retry timer与DB call deadline
```

这四项通过真实故障注入测试后，PMP才具备重新评估Production Candidate的基础。
