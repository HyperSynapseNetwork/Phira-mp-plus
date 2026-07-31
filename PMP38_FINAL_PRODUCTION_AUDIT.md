# PMP `(38)` 最终生产就绪审计与完成度报告

**PMP 版本：** `0.5.1942`  
**审计对象：** 本轮上传源码包  
**对比基线：** PMP `(37)` / `0.5.1933`  
**CI 状态：** **已通过**。按用户确认，本次上传项目已通过项目既有的 check、tests、Clippy 与 release build CI 门禁  
**静态校验：** 11 个 TOML、6 个 YML、2 个 JSON 均可解析；24 个 Markdown 文件、32 个本地链接未发现失效引用  
**代码规模：** Server 143 个 Rust 文件，约 47,525 行  
**相对 `(37)`：** 15 个文件变化；排除附带审计文档后，14 个代码/配置/依赖文件变化，约新增 177 行、删除 25 行  
**发布结论：** **NO-GO，继续作为 Development Preview**

---

# 1. 最终结论

PMP38 是一次目标非常集中的稳定性迭代。

PMP37 的多项明确问题已经真实关闭：

- 首次启动无 WAL 时，instance marker 改为 `clean=true`；
- Recovery 等待窗口从约 2.5 秒改为默认 30 秒并支持配置；
- `is_healthy()` 开始检查 pending ACK 数量；
- ACK retry 只有在队列全部清空时才解除 ACK degraded；
- `set_offline()` 删除空 Session ID 通配条件；
-管理员踢出路径会在清空 Session 引用前捕获固定 Session ID；
-普通 dangle grace 会保存固定 Session ID；
-HighFrequency enqueue 与 Shutdown 增加共同 admission gate；
-Plugin TCP receive 合并成功后不再重复 push 原始 payload；
-用户访问 Session ID 冲突改成 `ON CONFLICT DO NOTHING` 基础；
-启动 Recovery 增加周期性进度日志。

整体生产完成度可以从 PMP37 的约 82% 上调到约 86%。

但是，当前仍存在几个直接影响数据完整性和正式功能承诺的问题：

```text
active marker + WAL 文件异常缺失
→ shutdown compact() 发现 NotFound
→代码把 marker 改写为 clean
→下一次启动把异常 WAL 丢失当成合法 compact-to-zero
→未完成事件可能被永久掩盖
```

```text
WAL 最后一条 frame 完整且 checksum 正确
但缺少尾部换行
→ replay 接受该 frame
→没有物理补写换行
→下一次 append 直接把新 JSON 粘在旧 JSON 后
→WAL 在下一次 replay 时损坏
```

```text
Playing reconnect grace 到期
→重新从 Weak Session 读取 Session ID
→旧 Session 通常已经释放，得到空字符串
→严格 Session SQL 不匹配任何真实 Session
→旧 playtime Session 不会关闭
→用户可能持续显示 online
```

```text
Plugin TCP normal queue 已满
→receive 被合并到队尾事件
→事件数量保持有界
→但单条 bytes 数组可持续无限增长
→慢插件或远端洪泛仍可造成内存增长
```

此外，Plugin TCP 每插件使用 4 个 callback worker，会让同一 TCP 连接的 `accept/receive/disconnect` 和多个 receive chunk 存在并发执行与乱序风险。

因此当前仍不能进入 Production Candidate。

> **PMP38：NO-GO。**

---

# 2. CI 结论

用户已确认所有上传 PMP 项目文件 CI 均通过。

本报告将以下门禁视为已确认：

```text
cargo check
cargo test
cargo clippy -D warnings
release build
```

CI 通过证明：

-代码能够编译；
-现有测试全部通过；
-Clippy 门禁通过；
-Release 产物可以生成。

本轮源码差异没有新增对应的定向测试文件。现有 CI 尚未证明以下场景：

- active marker 下 WAL 文件异常丢失；
-完整无换行末帧后继续 append；
- Playing reconnect grace 后 playtime 关闭；
-WAL ACK gap 遇到 `list_pending()` 错误；
-Plugin TCP 合并后的 pending bytes 上限；
-同一连接多个 callback worker 的顺序；
-HighFrequency enqueue 与 Shutdown 高并发压力；
-多原因 WAL degraded 状态不会互相覆盖。

CI 通过不改变当前生产 No-Go 判定。

---

# 3. PMP37 → PMP38 修复状态

| PMP37 问题 | PMP38 状态 | 判断 |
|---|---|---|
|首次空闲运行后 marker 误报 |首次 marker 改为 clean；NotFound compact 尝试保持 clean | **原场景关闭，但引入异常 WAL 缺失被洗成 clean 的新风险** |
|ACK gap 永久等待 |增加合法 ACK gap 跳过逻辑 | **主体改善，错误路径仍 fail-open** |
|Recovery 只有约 2.5 秒 |默认 30 秒并可配置 | **已关闭** |
|pending ACK 未进入健康判断 |增加 `AtomicUsize` pending ACK count | **已关闭基础** |
|一次 ACK 成功过早清除 ACK degraded |仅队列空时清除 | **已关闭 ACK 队列场景** |
|空 Session ID 通配关闭任意 Session |SQL删除空字符串通配 | **已关闭危险通配** |
|管理员踢出在清引用后取Session ID |提前捕获 | **已关闭** |
|普通 dangle grace 到期重新读Weak Session |提前捕获固定ID | **已关闭普通路径** |
|Playing grace到期重新读Weak Session |未修改 | **Blocker** |
|HF Shutdown与enqueue无共同gate |增加 admission gate | **已关闭基础** |
|Plugin TCP merge后重复push |merge成功立即return | **已关闭重复数据问题** |
|Plugin TCP event queue可突破长度上限 |事件数量不再突破 | **长度关闭，字节量仍无界** |
|用户访问Session冲突硬失败 |任意unique conflict均DO NOTHING并核验user | **改善，但核验字段仍不足** |
|有效WAL末帧无换行 |未修 | **Blocker** |

---

# 4. 已真正关闭的问题

## 4.1 Recovery 默认预算

新增：

```yaml
runtime:
  startup_recovery_timeout_secs: 30
```

Recovery 会：

-按配置计算 absolute deadline；
-每 100ms 检查一次健康状态；
-每秒记录 pending WAL 进度；
-超时错误中包含实际等待时间。

该窗口已经明显长于当前低频 pipeline 的约 2.8 秒退避预算。

## 4.2 Pending ACK 健康检查

`PersistenceWorker` 新增：

```text
pending_acks: AtomicUsize
```

`is_healthy()` 要求：

```text
pending_acks == 0
```

多条 ACK 中一条成功时，也不会立即解除 ACK degraded；只有队列全部清空才会解除。

PMP37 的“ACK A 成功但 ACK B 仍失败时可能短暂 healthy”主要路径已关闭。

## 4.3 HighFrequency Shutdown admission gate

enqueue 和 Shutdown 现在共享：

```rust
admission_gate: tokio::sync::Mutex<()>
```

语义：

```text
enqueue先获得gate
→完整进入main/overflow后释放
→Shutdown随后捕获全部已接受数据

Shutdown先获得gate
→closed=true
→后续enqueue被拒绝
```

此前 accepted item 落在 Shutdown control 之后的竞态已关闭。

## 4.4 Plugin TCP重复 push

normal queue 已满且 receive 合并成功时，现在会：

```text
notify
→立即return
```

不再把同一 payload 同时：

-追加到最后一条 receive；
-又作为新事件 push。

事件重复和队列长度直接突破 `max_len` 的代码路径已经关闭。

## 4.5 Session SQL不再接受空ID通配

`set_offline()` 当前严格要求：

```text
user_id
server_instance_id
session_id
```

全部匹配。

旧 Offline 事件不再通过空字符串关闭同一实例中的任意新 Session。

这关闭了最危险的“旧 Offline 关闭新连接”路径。

---

# 5. P0-01：`compact(NotFound)` 会掩盖异常 WAL 丢失

PMP38 为修复首次空闲启动问题，在 `compact()` 的 WAL NotFound 分支增加：

```text
如果marker是active
→把marker写成clean
→返回Ok(0)
```

该逻辑对正常首次空闲运行有帮助，但对异常状态不安全。

## 5.1 正常不变量

正常情况下：

```text
WAL不存在
⇒ marker应当已经是clean
```

如果 marker 是 active 而 WAL 不存在，说明至少存在以下情况之一：

- WAL 被人工删除；
-存储卷故障；
-目录被部分清理；
-文件系统异常；
-尚未ACK的WAL被意外移除。

这正是 instance marker 原本要检测的情况。

## 5.2 当前会把异常状态合法化

场景：

```text
marker clean=false
WAL中有未完成Admission
WAL文件异常丢失
服务开始shutdown
compact()读WAL得到NotFound
→把marker改成clean
→shutdown仍可继续
```

下一次启动：

```text
clean marker + missing WAL
→被视为合法compact-to-zero
```

原有未完成事件永久消失。

## 5.3 正确修复

`compact()` 的 NotFound 分支应：

```text
marker clean=true
→Ok(0)

marker不存在且从未admit
→创建clean marker并Ok(0)

marker clean=false
→Err(WalMissingWhileActive)
→设置degraded
→shutdown不得报告成功
```

不能把 active marker 自动洗成 clean。

---

# 6. P0-02：有效但无换行的WAL末帧仍会在下一次append时损坏

Replay 当前会区分：

-完整且checksum正确的无换行末帧；
-真正截断的末帧。

完整末帧会被保留，这是正确方向。

但代码没有物理补写：

```text
\n
```

## 6.1 确定性故障

磁盘末尾：

```text
{"ver":2,...valid frame...}
```

没有换行。

Replay：

```text
解析成功
checksum成功
保留frame
```

下一次 admission：

```text
append {"ver":2,...new frame...}\n
```

结果文件：

```text
{old frame}{new frame}\n
```

下一次 replay 会把两段 JSON 当成一行并失败。

## 6.2 高风险情况

若末帧是一个已ACK事件：

- replay可能返回空 pending；
-Worker不会因该事件自动compact；
-服务器正常开始接受新用户；
-第一条新Admission就会污染WAL。

## 6.3 修复

在 replay 的 `io_gate` 内发现完整无换行末帧时：

```text
append换行
→flush
→fsync
```

或者原子重写规范化WAL。

必须增加：

```text
完整frame无换行
→replay
→新admit
→再次replay
```

集成测试。

---

# 7. P0-03：Playing reconnect grace仍无法可靠关闭原Session

`User::dangle()` 已经接收真实：

```rust
disconnected_session_id: Uuid
```

普通 dangle 路径也已经保存固定Session ID。

但 Playing reconnect grace 到期路径仍执行：

```rust
let sid = self_.current_session_id().await;
```

## 7.1 到期时Weak Session通常已经失效

Playing grace通常持续数秒。

到期时旧transport Session已经释放，Weak引用无法upgrade：

```text
current_session_id() → ""
```

由于PMP38已删除空ID通配，数据库会执行严格匹配：

```text
session_id IS NOT DISTINCT FROM ''
```

正常认证Session ID是UUID，不会匹配。

结果：

```text
UserOffline SQL执行成功但rows_affected=0
→pipeline把事件视为DatabaseCommitted
→WAL ACK
→playtime.session_start仍非NULL
```

用户可能继续显示在线，直到：

-服务重启的stale cleanup；
-下一次认证覆盖；
-人工修复。

## 7.2 Playing grace disabled路径也应统一

立即移除分支仍重新调用：

```text
current_session_id()
```

当前调用时旧Session可能仍在，但没有必要依赖Weak引用。

## 7.3 正确修复

在所有dangle路径中统一使用函数参数：

```rust
let disconnected_session_id = disconnected_session_id.to_string();
```

并将该值显式move进：

- playing grace timer；
-playing immediate cleanup；
-normal grace；
-UserDisconnect；
-UserOffline。

不应再次从Weak Session读取断开连接的代际。

## 7.4 数据库应区分“成功关闭”和“未匹配”

`set_offline()`当前只判断SQL是否执行成功，不判断：

```text
rows_affected
```

建议返回：

```rust
OfflineResult::Closed
OfflineResult::AlreadyClosed
OfflineResult::GenerationMismatch
OfflineResult::Failed
```

GenerationMismatch应产生指标和告警，而不是静默ACK。

---

# 8. P0-04：ACK-gap推进中的WAL读取错误被当成“没有pending”

PMP38新增了合法ACK gap跳过逻辑。

处理terminal sequence后，如果buffer中存在更高sequence，会读取：

```rust
worker_wal.list_pending()
```

用于判断中间sequence是：

-已ACK的合法空洞；
-仍在WAL中的WalOnly。

但错误分支当前是：

```rust
Err(_) => HashSet::new()
```

## 8.1 这会在WAL错误时fail-open

场景：

```text
expected=10
buffer有12
sequence11实际仍pending于WAL
list_pending因为IO/checksum错误失败
→代码把pending集合当成空
→跳过11
→处理12
```

之后11若被scanner送入，会因为：

```text
wal_sequence < next_expected
```

被当成stale跳过。

它还可能永久留在`in_flight`，只能等重启处理。

## 8.2 与其他路径不一致

Flush、Shutdown和gate初始化已经对WAL读取错误fail-closed。

ACK-gap推进也必须一致。

## 8.3 修复

`list_pending()`失败时：

-不得推进sequence；
-设置明确WAL degraded原因；
-保留当前expected；
-停止处理更高sequence；
-报告critical failure或进入retry。

不能使用空集合回退。

---

# 9. P0-05：WAL marker写入不是原子替换，且关键错误被忽略

`write_marker_inner()`注释称：

```text
Atomic + durable write
```

实际实现是：

```text
File::create(marker_path)
→直接截断原文件
→write
→fsync
```

没有：

-临时文件；
-原子rename；
-可靠parent fsync错误传播。

## 9.1 崩溃可留下损坏marker

进程在截断后、完整写入前崩溃时，marker可能变成：

-空文件；
-部分JSON；
-缺少clean/max_sequence字段。

下一次启动会fail-closed。

这是可接受的保守结果，但不符合注释和正式可用性要求。

## 9.2 Admission忽略active marker写入错误

成功写入WAL后：

```rust
let _ = self.mark_marker_active().await;
```

错误被忽略。

场景：

```text
marker仍clean
WAL已有新Admission
marker active更新失败
WAL随后异常丢失
下一次启动看到clean marker
→允许missing WAL
```

这会削弱instance marker的防丢失作用。

Replay结束时刷新marker也存在忽略错误的路径。

## 9.3 修复

Marker应采用：

```text
写marker.tmp
→flush
→fsync file
→rename
→fsync parent
```

`mark_marker_active()`失败后：

-Admission可返回明确的degraded outcome；
-至少设置WAL degraded；
-禁止把marker错误静默忽略。

---

# 10. P0-06：Plugin TCP队列只限制事件数，没有限制字节数

PMP38修复了receive重复push。

但receive merge实现：

```text
last_bytes.extend(incoming_bytes)
```

没有任何单事件或队列总字节上限。

## 10.1 事件数量有界不等于内存有界

normal queue满后，同一handle持续receive：

```text
始终合并到最后一条事件
→queue长度维持64
→最后一条bytes数组持续增长
```

远端每次最多读取约8KiB，但在慢插件场景下可以持续追加：

```text
8KiB
→16KiB
→1MiB
→100MiB
→……
```

这部分内存不受：

- per-connection pull read buffer 1MiB；
-per-plugin read buffer 4MiB；

约束，因为PluginEventChannel拥有独立的JSON payload副本。

## 10.2 修复

必须同时限制：

```text
max_event_bytes
max_queue_bytes
max_pending_bytes_per_plugin
```

达到上限时：

-丢弃最旧receive；
-分块；
-关闭异常连接；
-或发出overrun事件。

不能无限extend JSON数组。

---

# 11. P0/P1：Plugin TCP四个worker破坏单连接事件顺序

每插件使用4个固定worker，共享同一high/normal队列。

事件从队列中按顺序pop，但callback会并发执行。

## 11.1 TCP字节流要求顺序稳定

同一handle可能产生：

```text
receive chunk A
receive chunk B
disconnect
```

多个worker可能变成：

```text
worker1处理A
worker2处理B
worker3处理disconnect
```

`call_plugin_api()`内部虽然最终通过插件Host mutex串行，但多个任务对锁和execution permit的竞争顺序不保证与pop顺序一致。

插件可能观察到：

- B先于A；
-disconnect先于最后一个receive；
-receive在accept callback完成前执行。

## 11.2 high-priority队列还会主动改变全局顺序

disconnect/error位于high queue。

即使receive更早进入normal queue，后到的disconnect也会优先被worker取走。

生命周期优先级可以防止被数据洪泛淹没，但不能无条件破坏同一连接的协议顺序。

## 11.3 推荐设计

可采用：

### 方案A：每handle有序队列

```text
plugin级有界入口
→按handle分片
→同handle串行
→不同handle最多N并发
```

### 方案B：事件sequence

每连接增加单调sequence，并在插件调用前保证按sequence提交。

至少应保证：

```text
accept
→receive按字节顺序
→error/disconnect
```

---

# 12. P1：HighFrequency共享shutdown预算仍不完整

HighFrequency `shutdown(budget)` 已使用调用方预算。

但主关闭流程会先调用：

```text
high_frequency_writer.flush()
```

该函数内部固定使用5秒，没有接收共享remaining budget。

如果整个进程只剩2秒：

```text
flush仍可能等待5秒
→突破总shutdown deadline
```

建议：

```rust
flush(timeout: Duration)
```

并由main传入当前`remaining()`。

---

# 13. P1：UserDisconnect仍未使用Session代际

事件已经携带：

- `server_instance_id`
- `session_id`

但数据库pipeline仍调用：

```rust
record_user_disconnect(user_id, user_name)
```

SQL只按user_id更新，并使用事件处理时当前时间。

延迟WAL或DLQ中的旧Disconnect可能在用户重新连接后覆盖：

- `last_disconnected_at`
- `last_seen_at`
- `updated_at`

建议：

-事件增加`occurred_at`；
-SQL按时间或Session代际条件更新；
-旧Disconnect不得覆盖更新的connect事实。

---

# 14. P1：访问冲突核验只比较user_id

`commit_user_authenticated()`在event_id或session_id冲突时，会读取已有记录。

当前只验证：

```text
existing.user_id == incoming.user_id
```

然后把冲突视为幂等成功。

但没有验证：

- event_id；
-session_id；
-connected_at。

如果同一用户错误复用event_id但携带不同session_id，代码仍会：

-不新增visit；
-继续把playtime改为incoming session；
-更新last_connected_at。

正确幂等要求应是：

```text
event_id、session_id、user_id、connected_at
```

与已有记录一致。

否则应视为数据完整性错误。

---

# 15. P1：stale playtime cleanup没有清空session_id

`close_all_stale_sessions()`会清空：

- `session_start`
- `server_instance_id`

但没有同步：

```text
session_id = NULL
```

下一次认证会覆盖该值，所以通常不会直接影响业务，但会造成数据库中：

```text
offline row仍保留旧session_id
```

不利于审计和状态不变量。

应统一清空。

---

# 16. P1：WAL degraded仍是单Boolean

PMP38解决了“多条ACK队列”内部的一次成功过早清除问题。

但`degraded`仍是一个共享Boolean，可能同时表达：

- ACK失败；
-checksum错误；
-marker错误；
-WAL读取错误；
-fsync错误。

最后一条ACK成功并且pending队列清空时，仍可能：

```text
set_degraded(false)
```

从而清除另一个原因设置的degraded。

建议改为原因化状态：

```rust
WalHealth {
    corruption: bool,
    marker_error: bool,
    io_error: bool,
    pending_ack_count: usize,
}
```

只有全部原因解除才healthy。

---

# 17. P1：DLQ legacy Session事件仍回退到当前实例

DLQ重构中，缺少：

- `server_instance_id`
- `session_id`

的旧UserOffline/UserDisconnect仍可能回退：

```text
current server instance
empty session ID
```

PMP当前没有外部兼容负担。

更安全的策略是：

```text
缺失Session代际
→quarantine
→不自动绑定当前实例
```

否则会产生语义不明确的no-op或错误时间更新。

---

# 18. 完成度总表

完成度表示“距离正式生产闭环的程度”，不是代码量比例。

| 大项 | PMP37 | PMP38 | 当前判断 |
|---|---:|---:|---|
|项目架构与能力建设 | 94% | **95%** |主体能力完整 |
|客户端协议与房间玩法 | 89% | **90%** |核心路径稳定 |
|PostgreSQL数据模型 | 90% | **91%** |主要事务与迁移完整 |
|低频持久化可靠性 | 78% | **83%** |Recovery、ACK、Session SQL明显改善 |
|HighFrequency持久化 | 80% | **89%** |admission gate与DB deadline已形成 |
|故障恢复与重启一致性 | 74% | **82%** |预算改善，WAL边界仍有Blocker |
|插件宿主与WIT API | 88% | **89%** |主体稳定 |
|Plugin TCP | 76% | **72%** |重复push修复，但发现字节无界与顺序问题 |
|Real Benchmark | 84% | **84%** |本轮无实质变化 |
|运维、代理与管理接口 | 88% | **88%** |本轮无实质变化 |
|**综合生产完成度** | **约82%** | **约86%** |仍由少数一致性边界决定No-Go |

---

# 19. 核心项完成度

| 项目 | 完成度 | 状态 |
|---|---:|---|
|CI、构建与版本一致性 | **100%** |完成 |
|Room Actor核心状态所有权 | **92%** |基本完成 |
|JoinRoom协议顺序 | **92%** |基本完成 |
|RoundCompleted事务 | **93%** |基本完成 |
|UserAuthenticated幂等事务 | **91%** |主体完成 |
|WAL format与v1→v2迁移 | **93%** |基本完成 |
|WAL admission | **92%** |基本完成 |
|WAL ordered execution | **84%** |ACK-gap主体改善，错误路径仍需修 |
|Flush/Shutdown fence | **90%** |低频基本完成 |
|Recovery预算 | **90%** |已完成基础 |
|Pending ACK健康 | **87%** |队列闭环完成，原因模型未完成 |
|WAL marker生命周期 | **62%** |异常缺失和原子写仍有问题 |
|WAL末帧处理 | **68%** |完整无换行未规范化 |
|Session代际 | **77%** |普通路径改善，Playing路径未完成 |
|Playtime recovery | **84%** |实例heartbeat较完整 |
|HighFrequency admission/Shutdown | **92%** |主要竞态关闭 |
|HighFrequency DB deadline | **92%** |主要完成 |
|Plugin TCP事件数量有界 | **91%** |重复push已修 |
|Plugin TCP内存有界 | **55%** |合并bytes无上限 |
|Plugin TCP事件顺序 | **55%** |多worker并发顺序未定义 |
|Persistent Room recovery | **76%** |可执行但仍偏best-effort |
|DLQ recovery | **82%** |主要链路完整，legacy语义待清理 |

---

# 20. 当前生产阻断项完成度

| 生产门禁 | 完成度 | 是否阻断 |
|---|---:|---|
|active marker + missing WAL处理 | **40%** | **是** |
|有效无换行WAL末帧 | **65%** | **是** |
|Playing grace严格Session ID | **65%** | **是** |
|ACK-gap错误路径fail-closed | **75%** | **是** |
|marker原子持久化与错误传播 | **60%** | **是/条件阻断** |
|Plugin TCP pending bytes上限 | **50%** | **是** |
|Plugin TCP同连接事件顺序 | **55%** | **是/正式能力阻断** |
|HF共享shutdown预算 | **80%** | P1 |
|UserDisconnect代际时间保护 | **70%** | P1 |
|幂等冲突完整字段核验 | **80%** | P1 |
|原因化WAL健康状态 | **75%** | P1 |
|DLQ legacy Session隔离 | **75%** | P1 |

---

# 21. PMP38 Core P0任务清单

## P0-A：禁止把active missing WAL洗成clean

- [ ] `compact(NotFound)`读取marker状态
- [ ] clean marker才允许Ok
- [ ] active marker必须返回Err
- [ ]设置WAL degraded
- [ ]shutdown不得报告成功
- [ ] active marker + missing WAL测试
- [ ] pending event WAL删除测试
- [ ] clean compact-to-zero回归测试

## P0-B：规范化无换行末帧

- [ ] replay识别完整无换行frame
- [ ]在io_gate内补换行
- [ ] flush + fsync
- [ ]更新total_bytes
- [ ] list_pending与compact使用相同规则
- [ ]完整Admission无换行测试
- [ ]完整ACK无换行测试
- [ ]补换行后继续admit测试

## P0-C：完成所有Playing Session代际路径

- [ ] 使用`disconnected_session_id`参数
- [ ] playing grace timer携带固定ID
- [ ] grace disabled使用固定ID
- [ ] UserDisconnect使用固定ID
- [ ] UserOffline使用固定ID
- [ ]禁止正常事件生成空ID
- [ ] set_offline返回rows_affected语义
- [ ] playing重连与grace到期测试

## P0-D：ACK-gap读取错误fail-closed

- [ ] `list_pending()`错误不得回退空集合
- [ ]保持当前expected
- [ ]停止处理更高sequence
- [ ]设置reason-specific degraded
- [ ]报告critical failure
- [ ]错误恢复后重新判断gap
- [ ] gap判断期间WAL IO失败测试
- [ ] gap判断期间checksum失败测试

## P0-E：Marker原子写与错误传播

- [ ] marker.tmp
- [ ] file fsync
- [ ] atomic rename
- [ ] parent fsync
- [ ] `mark_marker_active()`失败不得忽略
- [ ] replay marker刷新失败不得忽略
- [ ] compact marker写失败返回Err
- [ ] marker写中崩溃测试

## P0-F：Plugin TCP字节级有界

- [ ] max receive event bytes
- [ ] max normal queue bytes
- [ ] max pending bytes per plugin
- [ ] merge前检查字节预算
- [ ]超过预算drop/close策略
- [ ] dropped bytes指标
- [ ]慢插件+持续输入压力测试
- [ ]内存稳定性测试

## P0-G：Plugin TCP每连接有序交付

- [ ] accept先于receive
- [ ] receive chunk严格按流顺序
- [ ] disconnect晚于已接受receive
- [ ]同handle串行
- [ ]不同handle可并发
- [ ]每handle sequence或分片worker
- [ ] timeout后连接状态定义
- [ ]并发连接顺序测试

---

# 22. P1任务清单

## Session与用户

- [ ] UserDisconnect增加occurred_at
- [ ] Disconnect SQL使用Session或时间条件
- [ ] stale cleanup清空session_id
- [ ]访问冲突核验全部字段
- [ ] GenerationMismatch指标

## WAL健康

- [ ] degraded原因拆分
- [ ] corruption不可被ACK成功清除
- [ ] marker错误不可被ACK成功清除
- [ ] health状态可查询
- [ ] health reason进入TUI/OpenUDS

## HighFrequency

- [ ] `flush(timeout)`接收共享预算
- [ ] main传入remaining
- [ ] shutdown失败后指标
- [ ] accepted/terminal最终对账

## DLQ

- [ ]缺失instance/session进入quarantine
- [ ]不回退当前实例
- [ ]quarantine失败保留源文件
- [ ]non-critical失败健康策略

##测试

- [ ]本轮每个P0增加定向回归测试
- [ ] WAL crash fixture
- [ ] Session快速重连fixture
- [ ] Plugin TCP内存压力
- [ ] HF shutdown concurrency

---

# 23. 必须新增的生产门禁测试

## 23.1 active marker下WAL丢失

```text
写入未ACK Admission
marker active
删除WAL
调用shutdown/compact
```

断言：

```text
compact失败
marker保持active
下一启动fail-closed
```

## 23.2 无换行末帧

```text
写完整ACK frame但去掉最后换行
replay
admit新事件
再次replay
```

断言全部frame可解析。

## 23.3 Playing grace

```text
用户进入Playing
断线
旧Session对象释放
grace到期
```

断言旧playtime行被准确关闭。

## 23.4 Gap判断WAL错误

```text
buffer含更高sequence
中间pending WalOnly
list_pending临时失败
```

断言不得越过中间sequence。

## 23.5 Marker写崩溃

```text
marker重写到一半进程退出
```

断言原marker或新marker至少一个完整存在。

## 23.6 Plugin TCP字节上限

```text
callback每次耗时30秒
同handle持续8KiB receive
```

断言：

```text
pending bytes不超过配置
内存不持续增长
连接得到明确overrun行为
```

## 23.7 Plugin TCP顺序

```text
accept
receive A
receive B
disconnect
```

断言插件观察顺序完全一致。

---

# 24. Go / No-Go上线门槛

PMP只有满足以下条件才能进入Production Candidate。

## WAL与恢复

- [ ] active marker下WAL缺失不能被自动洗成clean
- [ ]无换行末帧可自动规范化
- [ ] gap判断所有错误fail-closed
- [ ] marker写入原子且错误不被忽略
- [ ] Recovery和ACK健康可对账

## Session

- [ ]所有断线路径携带固定真实Session ID
- [ ] Playing grace能关闭旧Session
- [ ] GenerationMismatch可观测
- [ ]旧事件不能关闭或污染新Session

## Plugin TCP

- [ ]事件数量和字节量都严格有界
- [ ]同连接事件顺序稳定
- [ ]生命周期事件不会被receive淹没
- [ ]慢插件和恶意远端不能造成内存增长

## HighFrequency

- [ ] Shutdown admission线性化测试通过
- [ ] DB deadline测试通过
- [ ]共享shutdown预算不被固定Flush超时突破
- [ ] accepted/committed/dropped完整对账

---

# 25. 最终判断

PMP38 已经关闭了 PMP37 的多数显式阻断：

```text
首次空闲marker主路径
Recovery预算
pending ACK健康计数
空Session ID通配
HF enqueue/Shutdown竞态
Plugin TCP重复push
```

但进一步检查发现，marker修复对异常WAL缺失处理过度，会把本应fail-closed的active状态改成clean；WAL完整无换行末帧仍会在下一次append时损坏；Playing grace仍未使用函数已经提供的真实断开Session ID；Plugin TCP虽然事件数量有界，但字节量和顺序仍未闭环。

最终结论：

> **PMP `(38)`：NO-GO，继续作为 Development Preview。**

当前综合生产完成度约为：

> **86%**

下一轮建议只验收五个闭环：

```text
active marker异常缺失
→无换行末帧规范化
→Playing严格Session ID
→gap错误fail-closed
→Plugin TCP字节有界与同连接有序
```

这些关闭并完成定向故障测试后，PMP才适合重新评估Production Candidate。
