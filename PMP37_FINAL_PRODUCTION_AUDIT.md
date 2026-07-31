# PMP `(37)` 最终生产就绪审计报告

**PMP 版本：** `0.5.1933`  
**审计对象：** 本轮上传源码包  
**对比基线：** PMP `(36)` / `0.5.1913`  
**CI 状态：** **已通过**。按用户确认，本次上传项目已通过项目既有的 check、tests、Clippy 与 release build CI 门禁  
**静态校验：** 11 个 TOML、6 个 YAML/YML、2 个 JSON 均可解析；23 个 Markdown 文件、32 个本地链接未发现失效引用  
**代码规模：** Server 143 个 Rust 文件，约 47,373 行  
**相对 `(36)`：** 25 个文件变化，新增约 1,833 行，删除约 162 行  
**发布结论：** **NO-GO，继续作为 Development Preview**

---

# 1. 最终结论

PMP37 是目前为止持久化、恢复和 Session 生命周期闭环程度最高的一版。

PMP36 的四个主验收点已经真正进入执行代码：

- 初始 WAL replay 与运行期事件统一进入 sequence buffer；
- Flush/Shutdown 在取得 `send_gate` 后捕获 WAL target；
- clean marker 连续空闲重启的主要状态转换已修；
- pending WAL ACK 可以在 channel 空闲时主动重试；
- HighFrequency 单次 COPY/INSERT 已受剩余 deadline 限制。

此外，本版还完成：

- `UserOffline` 增加实例和 Session ID；
- `playtime` 增加 Session ID；
-服务实例 heartbeat；
-Persistent Room mailbox 在 recovery 前启动；
-Round completion 查询错误 fail-closed；
-Plugin TCP 固定 callback workers；
-保存全部 Plugin TCP worker handles；
-HighFrequency batch ID 加入服务实例；
-运行期 WAL 损坏 fail-closed；
-Persistence contract tests 增加。

这些都是实质进步。

但是，当前仍存在六个直接阻断 Production Candidate 的问题：

```text
首次启动时没有 WAL
→创建 clean=false marker
→整个运行期没有任何持久化事件
→正常 shutdown compact 发现 WAL 不存在并直接返回
→下一次启动发现 marker active 但 WAL 缺失
→误判 WAL 被人工删除并拒绝启动
```

```text
sequence 2 数据库已提交但 WAL ACK 失败
sequence 3 已提交并 ACK
sequence 4 仍 pending
→重启 replay 只包含 2 和 4
→处理 2 后 expected=3
→3 永远不会再出现
→4 永久留在 buffer
→startup recovery失败
```

```text
初始 replay 数据库写入使用约2.8秒退避预算
→startup recovery只等待约2.5秒
→短暂数据库故障即使可自动恢复
→服务仍先退出
```

```text
断线清理在 Session Weak引用失效或被清空后读取session_id
→得到空字符串
→SQL将空字符串解释为“匹配本实例任意Session”
→同实例快速重连后
→旧Offline可关闭新Session
```

```text
HighFrequency enqueue先检查closed=false后被挂起
→Shutdown将closed=true并发送控制消息
→Worker完成目标并退出
→旧enqueue恢复并把Item放在Shutdown后
→该Item已被计为accepted但永远不处理
```

```text
Plugin TCP normal queue已满
→merge_receive成功把bytes追加到最后一条
→代码仍再次push原始payload
→同一数据重复
→queue长度突破max_len并可持续增长
```

因此当前仍不能作为生产版本发布。

> **PMP37：NO-GO。**

不过，与早期版本相比，本轮剩余问题已经收敛为少量明确、可测试、可一次性关闭的状态机缺陷。

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

CI通过证明：

-代码能够编译；
-现有测试全部通过；
-Clippy门禁通过；
-发布产物可构建。

但现有CI仍未覆盖：

-首次启动零事件后重启；
-WAL ACK产生非连续pending sequence；
-恢复时间超过2.5秒；
-同实例快速断线重连；
-HF enqueue与Shutdown线性化竞态；
-Plugin TCP receive满队列合并；
-有效最终WAL frame缺少换行后继续append；
-多条pending ACK的健康状态竞态。

CI通过不改变当前生产No-Go判定。

---

# 3. PMP36 → PMP37 修复状态

| PMP36问题 | PMP37状态 | 判断 |
|---|---|---|
|初始replay使用独立路径 | replay全部进入统一BTreeMap sequence gate | **已关闭基础** |
|Flush target在send_gate外捕获 |改为gate内捕获 | **已关闭** |
|clean marker启动时被主动改active |clean+missing WAL启动保持clean | **关闭上一场景，但首次启动仍有新缺口** |
|pending ACK依赖业务消息 |有pending ACK时100ms唤醒重试 | **已关闭基础** |
|scanner首扫约10秒 |改为约5秒 | **改善，但仍晚于recovery deadline** |
|HF单次DB调用可越过deadline |DB attempt使用remaining-time timeout | **已关闭基础** |
|Offline没有Session ID |事件与playtime加入session_id | **字段完成，但空ID通配破坏保护** |
|历史Auth绑定新实例 |使用事件原始instance ID | **已关闭** |
|Persistent Room mailbox顺序 |mailbox在recovery前启动 | **已关闭启动顺序** |
|Round completion查询错误返回false |改为Result | **已关闭** |
|Plugin TCP worker handles泄漏 |保存全部worker handles | **已关闭基础** |
|Plugin TCP无生命周期优先级 |加入high/normal队列 | **方向正确，但receive合并有新Blocker** |
|HF batch ID跨实例重复 |加入server_instance_id | **已关闭基础** |

---

# 4. 已真正关闭的问题

## 4.1 Initial replay统一进入sequence gate

PMP37会把replay事件按真实：

```text
wal_sequence
```

全部放入同一个`BTreeMap`。

replay不再使用sequence 0绕过状态机。

普通queue事件、WalOnly scanner事件和初始replay现在共享同一处理入口。

## 4.2 Flush/Shutdown target线性化点

当前顺序为：

```text
获取send_gate
→读取wal.current_sequence
→发送Flush/Shutdown control
```

所有先取得gate的低频admission都会在target读取前完成。

PMP36中Flush漏掉正在WAL admission的WalOnly竞态已经关闭。

## 4.3 PendingControl生命周期

`pending_control`使用`as_mut()`复查，未完成时保留原reply sender。

此前第一次延期复查就出现：

```text
acknowledgement was dropped
```

的问题保持修复。

## 4.4 ACK idle retry

只要`pending_acks`非空，Worker会使用100ms channel receive timeout，使循环继续执行ACK retry。

ACK恢复不再完全依赖新业务消息。

## 4.5 HighFrequency数据库调用deadline

每次COPY和fallback INSERT都放在：

```text
tokio::time::timeout(remaining_deadline)
```

中。

单次数据库调用不再无限越过Flush/Shutdown deadline。

## 4.6 Persistent Room基础启动顺序

RoomCommandGateway mailbox已在`recover_state()`之前启动。

Persistent Room恢复不再因为没有`state_ref/self_ref`而必然失败。

## 4.7 Plugin TCP固定worker与完整handle管理

每插件使用固定worker消费callback，并保存全部JoinHandle。

卸载时不再只取消4个worker中的1个。

---

# 5. P0-01：首次空闲运行后，下一次启动会误判WAL被删除

这是PMP37当前最直接的启动Blocker。

## 5.1 首次启动创建active marker

当：

- WAL不存在；
-marker也不存在；

`replay()`调用：

```rust
write_instance_marker()
```

`write_instance_marker()`固定写入：

```json
"clean": false
```

但此时实际并没有WAL文件。

## 5.2 空闲运行不会创建WAL

如果服务启动后：

-没有用户认证；
-没有房间事件；
-没有Benchmark report；
-没有其他PersistenceEvent；

WAL文件一直不存在。

## 5.3 正常shutdown也不会把marker改为clean

`PersistenceWorker::shutdown()`最后调用：

```rust
wal.compact()
```

但`compact()`遇到WAL NotFound时直接：

```rust
return Ok(0)
```

不会写clean marker。

## 5.4 下一次启动fail-closed

最终磁盘状态：

```text
marker exists
clean=false
WAL missing
```

`check_instance_consistency()`会报：

```text
WAL instance marker exists but WAL file is missing
```

并拒绝启动。

## 5.5 可复现场景

```text
删除WAL和marker
启动PMP
不进行任何持久化操作
正常关闭
再次启动
```

预期应正常启动，当前会误判为WAL被意外删除。

## 5.6 正确修复

二选一：

### 方案A

首次启动且WAL不存在时写：

```text
clean=true
```

第一次Admission成功后再切换为active。

### 方案B

`compact()`遇到NotFound时仍写：

```text
clean=true
max_sequence=current
```

推荐同时做A和B，形成完整不变量：

```text
WAL缺失
⇒ marker必须clean=true
```

---

# 6. P0-02：ACK产生的sequence空洞会让重启replay永久等待

当前sequence gate假定：

```text
terminal sequence N之后
下一个待处理一定是N+1
```

实际WAL replay只返回：

```text
未ACK的Admission
```

因此pending sequence可能天然不连续。

## 6.1 可复现状态

运行期：

```text
seq2：
数据库已提交
WAL ACK失败
→仍pending
→sequence gate继续推进

seq3：
数据库提交
WAL ACK成功
→重启时不会replay

seq4：
仍pending
```

进程崩溃后，WAL replay返回：

```text
seq2
seq4
```

没有seq3，因为seq3已ACK。

## 6.2 当前Worker行为

```text
expected=2
→处理seq2
→ACK成功
→expected=3

buffer只剩seq4
→buffer.remove(3)为空
→channel也不会再出现seq3
→seq4永久等待
```

Initial replay永远无法drain，startup recovery最终失败。

## 6.3 这不是sequence损坏

seq3缺失是合法状态：

```text
它已经完成并ACK
```

所以不能要求pending Admission必须连续。

## 6.4 正确模型

sequence gate应按：

```text
最小未terminal WAL sequence
```

推进，而不是机械：

```text
current + 1
```

处理一个terminal事件后，应重新确定：

- buffer中最小sequence；
-WAL中最小未ACK sequence；
-当前processing/retry sequence。

必须同时防止跳过尚未被scanner入队的低sequence WalOnly。

推荐为每条Admission维护显式状态：

```text
Pending
Queued
Processing
PendingAck
Terminal
```

expected始终取最小非Terminal sequence。

---

# 7. P0-03：启动恢复等待时间短于持久化pipeline自身预算

Recovery当前最多等待：

```text
50 × 50ms ≈ 2.5秒
```

数据库pipeline配置：

```text
5次尝试
backoff = 100ms + 200ms + 500ms + 2000ms
```

仅退避时间已经约：

```text
2.8秒
```

还没有计算：

-5次SQL执行时间；
-DLQ写入和fsync；
-WAL ACK；
-调度延迟。

## 7.1 后果

初始WAL replay遇到短暂数据库故障时：

```text
pipeline仍在按照设计重试
→recovery在2.5秒时先判定失败
→服务退出
```

即使数据库在第4或第5次尝试恢复，也来不及完成启动。

## 7.2 Scanner同样来不及

RetryableFailure需要scanner重新入队。

scanner第一次真实扫描约在：

```text
5秒
```

Recovery却在2.5秒退出。

所以初始replay只要真正进入RetryableFailure，当前启动周期不可能依靠scanner自愈。

## 7.3 正确修复

增加配置：

```yaml
recovery:
  persistence_timeout_secs: 30
  retry_scan_interval_ms: 250
```

Worker应直接保留并重试当前expected事件，而不是主要依赖5秒scanner。

Recovery必须暴露进度：

- expected sequence；
-replay pending；
-pending ACK；
-retry attempt；
-last error；
-next retry；
-elapsed/deadline。

---

# 8. P0-04：健康状态可在仍有pending ACK时短暂变为healthy

ACK retry成功时立即执行：

```rust
worker_wal.set_degraded(false)
```

但pending ACK队列可能还有其他记录。

`is_healthy()`当前检查：

- replay_succeeded；
-initial_replay_drained；
-WAL未degraded；
-Worker未closed。

它没有检查Worker本地：

```text
pending_acks.is_empty()
```

## 8.1 竞态场景

```text
pending ACK：A、B

A重试成功
→set_degraded(false)
→A出队
→B仍pending

Recovery此时轮询is_healthy
→可能得到true
```

随后B重试失败，再把degraded设回true。

服务可能已经越过WAL recovery阶段。

## 8.2 单Boolean还会覆盖其他故障原因

一次ACK成功会清除degraded，即使degraded原本来自：

-另一个ACK；
-WAL读取错误；
-marker错误；
-其他WAL IO故障。

## 8.3 修复

不要使用一个可随意清零的Boolean表达全部健康原因。

建议：

```text
WalHealth {
  corruption,
  io_failure,
  pending_ack_count,
  replay_state,
}
```

只有：

```text
pending_ack_count == 0
且无任何latched error
```

才允许healthy。

---

# 9. P0-05：空Session ID会退化为同实例通配Offline

PMP37增加了`session_id`，方向正确。

但当前：

```rust
current_session_id()
```

在Weak Session不能upgrade时返回空字符串。

注释明确说明：

```text
空字符串表示匹配本实例任意Session
```

数据库SQL：

```sql
AND (session_id IS NOT DISTINCT FROM $4 OR $4 = '')
```

## 9.1 多个断线路径会得到空ID

### 管理员踢出

代码先执行：

```text
user.session = None
```

之后才调用：

```text
user.current_session_id()
```

所以一定得到空字符串。

### Playing grace到期

等待若干秒后再从Weak Session读取ID，旧Session通常已经drop。

### 普通dangle到期

同样在grace结束时才读取Weak Session。

### 立即移除路径

也可能在transport Session已经释放后读取。

## 9.2 同实例快速重连风险

部分路径在enqueue Offline前释放：

```text
user_registration_gate
```

随后用户可能在同一个服务实例建立新Session。

新认证写入：

```text
playtime.session_id = new_session
```

旧Offline随后以：

```text
session_id = ""
```

提交。

SQL通配条件会关闭新Session。

## 9.3 现有函数参数已经提供正确ID

`User::dangle()`已经接收：

```rust
disconnected_session_id
```

但后续多个路径没有持续使用它，而重新读取Weak Session。

## 9.4 正确修复

-断线入口立即捕获`disconnected_session_id`；
-所有timer closure显式携带该ID；
-管理员踢出在清空Session引用前捕获ID；
-删除SQL中的`OR $4 = ''`；
-正常实时事件不允许空Session ID；
-legacy缺少Session代际的WAL/DLQ记录进入quarantine或专门迁移；
-Offline SQL必须严格匹配：
  - user_id；
  - server_instance_id；
  - session_id。

---

# 10. P0-06：HighFrequency Shutdown与enqueue没有共同线性化点

低频PersistenceWorker已有：

```text
send_gate
```

HighFrequencyWriter没有类似admission gate。

## 10.1 当前enqueue

```text
检查closed
→分配sequence
→try_send main或overflow
→更新last_accepted_sequence
```

## 10.2 当前shutdown

```text
closed.swap(true)
→发送Shutdown
→Worker drain并读取last_accepted_sequence
→退出
```

## 10.3 竞态场景

```text
任务A enqueue：
读取closed=false
暂停

任务B shutdown：
closed=true
发送Shutdown

Worker：
处理Shutdown
读取当前target
drain结束
返回Ok并退出

任务A恢复：
try_send Item到Shutdown消息之后
更新last_accepted_sequence
返回MainQueue/OverflowQueue
```

该Item已经向调用方报告accepted，却永远不会处理。

如果channel在Worker退出后已关闭，可能返回Err；但在Shutdown消息已排队、receiver尚未drop的窗口中，send仍可能成功。

## 10.4 修复

增加HF admission gate：

```text
enqueue：
获取gate
→检查closed
→完成main/overflow admission
→释放gate

shutdown：
获取gate
→closed=true
→捕获target
→发送Shutdown(target, deadline)
→释放gate
```

Shutdown control之后不允许再接受任何Item。

---

# 11. P0-07：Plugin TCP receive合并会重复数据并突破队列上限

`PluginEventChannel::push()`对normal queue：

```text
如果queue已满
→尝试merge_receive
→merge成功时把incoming bytes追加到最后一条
→然后无条件queue.push_back(original payload)
```

## 11.1 直接后果

假设queue长度为64：

```text
merge成功
→最后一条receive已包含新bytes
→再次push原始receive
→queue长度变65
```

下一次又可继续：

```text
65 → 66 → 67 ...
```

因此`max_len`不再是上限。

## 11.2 数据重复

incoming bytes同时存在于：

-被合并的最后一条；
-新push的原始payload。

插件会收到重复TCP字节。

这可能破坏上层协议解析，而不仅是资源问题。

## 11.3 修复

成功merge后立即：

```rust
notify.notify_one();
return;
```

不能再次push。

增加不变量：

```text
normal.len() <= max_len
high.len() <= max_len
```

并测试：

```text
queue full
连续1000次同handle receive
→queue不增长
→每个byte只出现一次
```

---

# 12. P0/P1：有效的最终WAL frame缺少换行会在下一次append时损坏

Replay对“最后一条没有换行”的处理：

-如果frame完整且checksum正确，则保留；
-如果frame不完整，则截断。

完整frame路径只是把它重新加入内存解析列表，没有物理补上：

```text
\n
```

## 12.1 后果

磁盘末尾：

```text
{valid frame}
```

没有换行。

下一次Admission append：

```text
{valid frame}{new frame}\n
```

两段JSON粘在同一行。

下一次replay会解析失败并fail-closed。

## 12.2 修复

识别到完整无换行frame后：

-append一个换行并fsync；
-或原子重写规范化WAL；
-之后才允许新Admission。

增加测试：

```text
写完整有效frame但无换行
→replay成功
→admit新事件
→再次replay成功
```

---

# 13. P1：WAL marker写入并非真正原子替换

注释声称：

```text
Atomic + durable write
```

实际使用：

```rust
File::create(marker_path)
```

直接截断原marker并写入。

进程在写入中崩溃时，可能留下：

-空marker；
-部分JSON；
-未完整字段。

下一次启动会因为marker无法解析而fail-closed。

## 修复

marker也应使用：

```text
写临时文件
→flush
→fsync文件
→rename
→fsync父目录
```

另外`admit()`当前忽略：

```rust
mark_marker_active()
```

错误。

如果WAL已写入但marker仍错误地保持clean，WAL随后丢失时可能被误认为合法compact结果。

Marker状态转换失败应：

-设置degraded；
-进入AdmissionOutcome中；
-不得静默忽略。

---

# 14. P1：UserDisconnect仍未使用Session代际

`PersistenceEvent::UserDisconnect`已经携带：

- server_instance_id；
-session_id。

Dead-letter也保存这些字段。

但pipeline仍调用：

```rust
record_user_disconnect(user_id, user_name)
```

SQL只按user_id更新，并使用事件处理时当前时间。

延迟WAL/DLQ中的旧Disconnect可能在新连接后更新：

- last_disconnected_at；
-last_seen_at；
-updated_at。

建议：

-增加occurred_at；
-数据库按Session代际或时间条件更新；
-旧Disconnect不得覆盖更晚的connect事实。

---

# 15. P1：DLQ仍不是完全无损表示

剩余问题：

-缺少kind或event的记录只计skipped，可能随replaying文件删除；
-quarantine写入失败只warning，源文件仍可能被删除；
-legacy Offline/Disconnect缺少instance/session时回退为当前实例；
-non-critical admission失败允许Ready，只保留文件但健康状态不明确。

鉴于项目当前没有兼容负担，legacy缺少代际信息的状态事件不应绑定当前实例。

更安全：

```text
quarantine
→明确管理员处理
```

---

# 16. P1：并发多个Flush/Shutdown仍可能覆盖控制对象

Worker只保存：

```rust
Option<PendingControl>
```

若第一个Flush正在等待，第二个control到达时可能覆盖旧control并drop旧reply。

当前主要关闭流程通常串行，风险低于核心P0，但状态机应明确：

-拒绝第二个control；
-合并target和deadline；
-或使用控制队列。

---

# 17. P1：服务实例注册失败仍只warning

playtime crash recovery依赖：

```text
mp_server_instances.last_alive_at
```

但实例注册失败只warning后继续。

持续heartbeat失败达到阈值才报告critical。

若首次注册失败：

-下次恢复缺少last_alive；
-可能重新把停机时间计入playtime；
-恢复质量下降。

建议首次实例注册为生产required条件。

---

# 18. P1：Persistent Room仍是部分best-effort

启动顺序已经正确，但：

-字段恢复失败多为warning；
-空房host恢复语义不完整；
-snapshot parse和字段应用未统一事务；
-恢复完成后没有对账；
-required模式下部分字段失败仍可能Ready。

需明确：

```text
恢复房间ID
```

还是：

```text
恢复完整控制状态
```

并按承诺设置门禁。

---

# 19. 当前评分

|领域|评分|说明|
|---|---:|---|
|架构方向|9.5/10|核心数据链已经高度收敛|
|CI与构建|10/10|用户确认CI通过|
|普通durable terminal gate|8.5/10|基本正确|
|Initial replay gate|7/10|统一入口完成，但ACK gap会死锁|
|Flush/Shutdown fence|9/10|低频线性化点已修|
|WAL marker生命周期|4/10|首次空闲运行仍会误判丢失|
|WAL格式完整性|7/10|中间损坏fail-closed，无换行frame仍有缺口|
|Recovery可恢复性|5/10|2.5秒窗口短于自身重试模型|
|ACK健康状态|5/10|pending ACK未进入health模型|
|User Session代际|6/10|字段存在，空ID通配破坏保护|
|Playtime recovery|8/10|实例heartbeat基础形成|
|Round recovery|9/10|顺序和查询错误处理较完整|
|Persistent Room|7/10|可执行，仍是best-effort|
|HighFrequency|7.5/10|DB deadline已修，Shutdown admission竞态仍存在|
|Plugin TCP|5.5/10|worker治理改善，merge bug可导致无界队列和重复数据|
|客户端协议|8.5/10|未发现新的核心Phira帧时序回归|
|当前阶段|Development Preview|仍不能进入Production Candidate|

---

# 20. PMP37 Core P0任务清单

## P0-A：首次空闲运行marker

- [ ]首次启动无WAL时写clean marker
- [ ] compact NotFound时确保marker clean
- [ ]第一次Admission后切换active
- [ ]空闲启动→正常关闭→再次启动测试
- [ ]连续10次无事件重启测试

## P0-B：允许合法ACK sequence空洞

- [ ]不再机械使用current+1
- [ ]expected取最小未terminal sequence
- [ ]区分不存在、已ACK、未到达WalOnly
- [ ]pending Admission状态表
- [ ] seq2 pending、seq3 ACK、seq4 pending重启测试
- [ ] compact后非连续pending测试

## P0-C：Recovery deadline与主动retry

- [ ] recovery timeout配置化
- [ ]默认至少30秒
- [ ]Retryable expected事件短周期直接重试
- [ ]scanner首次扫描不晚于配置周期
- [ ]pipeline retry预算纳入recovery deadline
- [ ]启动进度指标
- [ ]数据库延迟5秒后恢复测试
- [ ]DLQ延迟恢复测试

## P0-D：ACK健康状态

- [ ]health显式检查pending_ack_count
- [ ]一条ACK成功不得清除其他故障
- [ ]degraded reason分离
- [ ]错误原因使用latched状态
- [ ]全部ACK完成后才清除ACK degraded
- [ ]多ACK交替成功失败测试

## P0-E：严格Session代际

- [ ]断线入口使用disconnected_session_id
- [ ]timer closure携带固定Session ID
- [ ]踢出前捕获Session ID
- [ ]删除`OR session_id=''`
- [ ]空Session ID拒绝或quarantine
- [ ]Offline严格匹配instance+session
- [ ]同实例快速重连测试
- [ ]旧Offline不得关闭新Session

## P0-F：HighFrequency Shutdown线性化

- [ ]增加HF admission gate
- [ ]enqueue在gate内检查closed并完成admission
- [ ]shutdown在gate内closed+target+control
- [ ]Shutdown后不得再接受Item
- [ ]Item accepted必须进入shutdown target
- [ ]enqueue/shutdown竞态压力测试
- [ ] accepted/terminal对账

## P0-G：Plugin TCP receive merge

- [ ]merge成功后立即return
- [ ]不得再次push原payload
- [ ]normal queue长度永不超过max_len
- [ ]合并bytes不重复
- [ ]不同handle不误合并
- [ ]满队列持续receive压力测试
- [ ]pending bytes上限

---

# 21. P1任务清单

## WAL格式与marker

- [ ]有效末帧无换行时物理补换行
- [ ]marker临时文件原子rename
- [ ]marker parent fsync错误处理
- [ ]mark_marker_active失败设置degraded
- [ ]marker和WAL状态对账测试

## Session事件

- [ ]UserDisconnect使用instance/session
- [ ]增加occurred_at
- [ ]last_disconnected条件更新
- [ ]dead-letter完整保留代际
- [ ]legacy缺代际事件quarantine

## DLQ

- [ ]missing kind/event进入quarantine
- [ ]quarantine失败保留源文件
- [ ]non-critical failure标记degraded
- [ ]不再回退为当前instance
- [ ]多次重启无丢失测试

## Persistence control

- [ ]多个并发control拒绝/合并/排队
- [ ]control queue指标
- [ ]caller timeout清理旧control
- [ ]degraded worker control返回准确语义

## Server instance

- [ ]首次注册失败not-ready
- [ ]heartbeat age健康指标
- [ ]heartbeat长期失败degraded
- [ ]shutdown final heartbeat

## Persistent Room

- [ ]required模式字段恢复fail-closed
- [ ]host恢复语义
- [ ]snapshot应用对账
- [ ]恢复后重新读取状态验证

---

# 22. 必须新增的生产门禁测试

## 22.1 首次空闲重启

```text
无marker
无WAL
启动
不产生事件
正常shutdown
再次启动
```

断言正常启动。

## 22.2 ACK sequence空洞

```text
seq2 commit但ACK失败
seq3 commit且ACK成功
seq4 pending
崩溃重启
```

断言seq2和seq4均能处理，不能等待不存在的seq3。

## 22.3 Recovery预算

```text
PostgreSQL在启动后5秒恢复
WAL含pending Auth
```

断言服务在配置deadline内恢复成功。

## 22.4 多ACK健康状态

```text
ACK A成功
ACK B仍失败
```

断言`is_healthy=false`。

## 22.5 Session wildcard

```text
旧Session断线
Weak引用失效
同实例新Session建立
旧Offline提交
```

断言新Session保持online。

## 22.6 HF Shutdown竞态

```text
enqueue读取closed=false后暂停
shutdown发送control并准备退出
enqueue恢复
```

断言enqueue被拒绝或被包含在shutdown target，不得accepted后丢失。

## 22.7 Plugin TCP merge

```text
normal queue已满
连续同handle receive 1000次
```

断言：

```text
len <= max_len
总bytes准确
无重复
```

## 22.8 WAL末帧无换行

```text
写完整有效frame但不写换行
replay
admit新事件
再次replay
```

断言WAL仍可解析。

---

# 23. Go / No-Go上线门槛

PMP只有满足以下条件，才能进入Production Candidate。

##启动与WAL

- [ ]首次空闲运行后可正常重启
- [ ]合法ACK sequence空洞不会死锁
- [ ]完整无换行frame可规范化
- [ ]marker状态原子持久化

##恢复

- [ ]Recovery deadline覆盖完整重试预算
- [ ]Retryable事件无需等待5秒scanner
- [ ]pending ACK全部完成前不得healthy
- [ ]数据库短暂故障可自愈启动

##Session

- [ ]Offline/Disconnect始终携带真实Session ID
- [ ]空Session ID不能通配当前实例
- [ ]旧Session事件不能影响新Session
- [ ]访问、在线状态、playtime可对账

##高频与插件网络

- [ ]HF Shutdown与enqueue有共同线性化点
- [ ]accepted HF item全部有terminal
- [ ]Plugin TCP队列严格有界
- [ ]receive合并不重复字节
- [ ]生命周期事件不被数据洪泛破坏

---

# 24. 最终判断

PMP37已经基本关闭PMP36报告中的四个主问题：

```text
initial replay统一sequence gate
Flush/Shutdown target gate内捕获
clean marker空闲重启主状态转换
独立ACK retry与HF DB deadline
```

但进一步故障推演发现：

-首次启动marker仍错误标记active；
-pending sequence并不保证数值连续；
-Recovery等待窗口与自身重试策略冲突；
-Session ID空值使代际保护退化为通配；
-HF Shutdown与enqueue缺少线性化；
-Plugin TCP merge存在明确重复与无界增长代码路径。

这些问题都可通过小范围状态机修改和定向测试关闭，不需要再次重构整体架构。

最终结论：

> **PMP `(37)`：NO-GO，继续作为 Development Preview。**

下一轮建议只验收五个闭环：

```text
首次空闲marker
→ ACK-gap sequence gate
→ Recovery预算与ACK health
→严格Session代际
→ HF/Plugin TCP有界线性化
```

这五项关闭并通过故障注入测试后，PMP才适合重新评估Production Candidate。
