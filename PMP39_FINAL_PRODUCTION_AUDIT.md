# PMP `(39)` 最终生产就绪审计与完成度报告

**PMP 版本：** `0.5.1958`  
**Plugin SDK 版本：** `0.5.1958`  
**审计对象：** 本轮上传源码包  
**对比基线：** PMP `(38)` / `0.5.1942`  
**CI 状态：** **已通过**。按用户确认，本次上传项目已通过项目既有的 check、tests、Clippy 与 release build CI 门禁  
**静态校验：** 11 个 TOML、6 个 YML、2 个 JSON 均可解析；25 个 Markdown 文件、32 个本地链接未发现失效引用  
**代码规模：** Server 143 个 Rust 文件，约 47,773 行  
**相对 `(38)`：** 排除附带审计文档后，19 个代码、配置或依赖文件变化，约新增 333 行、删除 85 行  
**发布结论：** **NO-GO，继续作为 Development Preview**

---

# 1. 最终结论

PMP39 是一次有效而且目标明确的稳定性迭代。

PMP38 报告中的主要验收项，大部分已经真实修复：

- active marker 下 WAL 缺失时，`compact()` 不再自动洗成 clean；
-完整但缺少尾部换行的 WAL frame 会在 replay 时补写换行并 fsync；
- Playing reconnect grace 使用断线入口捕获的固定 Session ID；
- ACK-gap 判断中的 WAL 读取错误改为 fail-closed；
- instance marker 改成临时文件 + rename；
- HighFrequency `flush()` 接受调用方共享预算；
- `UserDisconnect` 增加原始 `occurred_at`；
- stale playtime cleanup 清空 Session ID；
-认证访问冲突开始核验 Session 和连接时间；
- Plugin TCP 增加 pending bytes、dropped bytes 和 per-handle 锁基础。

这些改动使低频 WAL、Session 生命周期和 HighFrequency 更接近正式闭环。

但是，本轮深入追踪发现两个新的确定性 Blocker：

```text
WAL Admission 已经 append + flush + fsync 成功
→ mark_marker_active 失败
→ admit() 返回 Err
→ PersistenceWorker 向业务返回“未接受”
→认证或业务命令回滚
→事件实际上仍在 WAL
→scanner 或重启后执行该事件
```

以及：

```text
Plugin TCP event push
→ total_bytes 增加
→ worker pop并执行callback
→ total_bytes从不减少
→累计约4 MiB后
→即使队列已经为空，后续事件仍全部被判定超预算并丢弃
```

Plugin TCP receive 合并分支还会在没有预先检查字节预算的情况下扩展最后一条事件，因此既可能突破4 MiB限制，也可能让单条JSON bytes数组持续增长。

同一连接的事件顺序也未真正闭环：

- `tcp:accept` payload没有`handle`字段，因此不会进入conn_handle锁；
- high-priority disconnect可以越过更早的normal receive；
-多个worker对同一handle锁的竞争顺序不等于queue pop顺序。

此外，WAL append/ACK在写入中途失败时没有回滚到原文件长度，也没有确认完整frame是否已经落盘。该路径可能产生：

-调用方收到失败，但完整事件留在WAL；
-半frame后继续append，形成永久损坏行；
-数据库已经commit，但ACK尾部损坏，重启后重复执行。

因此当前仍不能进入Production Candidate。

> **PMP39：NO-GO。**

当前综合生产完成度约为：

> **88%**

---

# 2. CI结论

用户已确认所有上传的PMP项目文件CI均通过。

本报告将以下门禁视为已确认：

```text
cargo check
cargo test
cargo clippy -D warnings
release build
```

CI通过证明：

-源码能够编译；
-现有测试通过；
-Clippy门禁通过；
-Release产物可构建。

但本轮关键问题说明，现有测试尚未覆盖：

- WAL fsync成功后marker写失败；
-WAL append部分写入后返回Err；
-ACK append部分写入；
-Plugin TCP事件消费后的pending bytes回收；
-满队列receive merge与字节预算；
-accept/receive/disconnect同连接顺序；
-多种WAL degraded原因叠加；
-marker parent fsync失败；
-同一event_id与session_id分别冲突到不同记录。

CI通过不改变当前生产No-Go判定。

---

# 3. PMP38 → PMP39修复状态

| PMP38问题 | PMP39状态 | 判断 |
|---|---|---|
|active marker + WAL缺失被洗成clean |active marker时compact返回Err并标记degraded | **已关闭主路径** |
|完整无换行WAL末帧 |replay补写换行、flush、fsync | **已关闭主路径** |
|Playing grace重新读取Weak Session |改为固定`disconnected_session_id` | **已关闭** |
|ACK-gap WAL读取错误回退空集合 |错误时保持gate并报告critical | **已关闭** |
|marker直接截断写入 |改为tmp + rename | **主体完成，parent fsync错误仍被忽略** |
|Plugin TCP pending bytes无上限 |增加4 MiB计数基础 | **实现有严重计数和merge缺陷** |
|Plugin TCP同连接无序 |增加per-handle Tokio Mutex | **未真正闭环** |
|HF flush固定5秒 |改为调用方timeout | **已关闭基础** |
|UserDisconnect使用处理时间 |增加并使用`occurred_at` | **改善，旧事件仍可回退时间字段** |
|stale playtime不清session_id |已清空 | **已关闭** |
|认证冲突仅核验user_id |增加session与connected_at核验 | **改善，event_id和双重冲突仍不完整** |
|WAL degraded单Boolean |增加reason字符串 | **实现不正确，原因仍可被覆盖并清除** |

---

# 4. 已真正关闭的问题

## 4.1 active marker下WAL异常缺失

`compact()`在WAL文件不存在时会读取marker。

若marker为active：

```text
clean=false
```

当前会：

-设置`compact:missing-wal` degraded；
-返回错误；
-不再把marker改成clean。

这关闭了PMP38中“异常WAL丢失被正常关闭流程合法化”的主路径。

## 4.2 无换行末帧

Replay识别完整、checksum正确但缺少换行的最终frame后，会：

```text
append "\n"
→flush
→sync_data
```

下一条Admission不再与旧JSON粘连。

## 4.3 Playing固定Session代际

Playing grace timer、grace disabled路径和普通dangle路径都开始使用：

```text
disconnected_session_id
```

而不是到期后重新读取Weak Session。

旧Offline关闭新Session或无法关闭旧Session的主要路径已经关闭。

## 4.4 ACK-gap错误fail-closed

Sequence-gap判断中，如果`list_pending()`失败：

-不再把错误当作空集合；
-不推进expected；
-报告critical failure；
-保留当前sequence gate。

## 4.5 HighFrequency共享预算

主Shutdown流程将剩余预算传入：

```rust
high_frequency_writer.flush(budget)
```

Flush的worker deadline和调用方timeout使用相同预算基础。

## 4.6 用户生命周期时间基础

`UserDisconnect`新增：

```text
occurred_at
```

并通过WAL和DLQ保留。

数据库不再总是使用事件重放时的当前时间。

---

# 5. P0-01：WAL已持久化后marker失败仍返回普通Err

这是PMP39当前最严重的低频持久化语义问题。

## 5.1 当前顺序

`PersistenceWal::admit()`：

```text
append WAL frame
→flush
→sync_data
→提交sequence counter
→增加admission count
→mark_marker_active()
```

现在`mark_marker_active()`错误会通过`?`向上传播。

## 5.2 WAL已接受，调用方却认为未接受

若marker更新失败：

```text
WAL frame已完成fsync
admit()返回Err
PersistenceWorker::enqueue()返回Err(event)
```

调用方按“没有进入持久化系统”处理。

认证路径明确会：

```text
拒绝认证
不发送Authenticate(Ok)
```

但事件仍位于WAL，scanner或重启后仍会执行：

```text
UserAuthenticated
→增加访问记录
→设置playtime online
```

这重新产生了之前已经多轮修复过的ghost event：

```text
客户端看到失败
数据库以后却记录成功事实
```

## 5.3 marker不是Admission事实

Marker负责检测WAL异常删除，但真正的Admission线性化点是：

```text
WAL frame fsync成功
```

一旦该步骤成功，就不能向业务返回普通Rejected。

## 5.4正确修复

推荐返回结构化结果：

```rust
enum WalAdmission {
    Admitted {
        wal_id,
        sequence,
        marker_state: MarkerState,
    },
    RejectedBeforeWal {
        error,
    },
}
```

marker失败时：

-事件仍视为Admitted；
-返回`WalOnly/QueuedDegraded`；
-设置`marker` degraded；
-readiness进入degraded；
-不得让业务回滚。

另一种方案是在写WAL前先把marker切为active，但需要为append失败设计marker回滚或明确fail-closed语义。

---

# 6. P0-02：WAL append/ACK失败没有回滚不确定尾部

`append_frame_inner()`当前：

```text
OpenOptions append
→write_all
→flush
→sync_data
```

任一步失败都会直接返回Err。

但文件可能已经被部分修改。

## 6.1 完整frame可能已经写入

例如：

```text
write_all完成
flush或sync_data返回Err
```

事件可能已经存在于页缓存甚至磁盘。

调用方仍收到失败。

重启时，如果frame完整且checksum正确，Replay会执行该事件。

这与第5节产生相同ghost semantics。

## 6.2 半frame会污染后续append

`write_all()`在返回错误前可能已经写入部分字节。

下一次成功Admission仍使用append：

```text
partial JSON + new JSON + "\n"
```

形成无法解析的完整坏行。

Replay不会把它当作单纯尾部截断，因为后面已经附加了新frame，只能fail-closed。

## 6.3 ACK同样受影响

数据库已成功commit后写ACK：

```text
ACK append部分失败
→进入pending ACK retry
→retry ACK继续append到损坏尾部
```

最终可能：

-数据库已提交；
-WAL无法解析；
-服务重启失败；
-或事件重复重放。

## 6.4正确修复

在`io_gate`内：

1. 记录append前文件长度；
2.执行write/flush/sync；
3.失败后检查尾部：
   - 若完整frame存在，再次sync成功则视为成功；
   -若不完整，truncate回原长度并fsync；
4.若无法确认或回滚：
   -设置WAL fatal degraded；
   -停止新Admission；
   -不能返回普通Rejected。

需要同时测试Admission和ACK。

---

# 7. P0-03：Plugin TCP pending bytes从不在消费时减少

PMP39为`PluginEventChannel`增加：

```text
total_bytes
MAX_PENDING_EVENT_BYTES_PER_PLUGIN = 4 MiB
```

Push时会增加计数。

队列满时丢弃旧事件也会减少计数。

但是callback worker从high/normal queue `pop_front()`时，没有减少：

```text
total_bytes
```

## 7.1确定性后果

即使插件消费速度完全正常：

```text
累计接收4 MiB事件
→队列每次都及时清空
→total_bytes仍累计到4 MiB
```

之后所有新事件都会触发：

```text
total_bytes + incoming > 4 MiB
```

并被丢弃。

因此`pending_bytes`实际上不是pending bytes，而是近似累计历史流量减去少量eviction。

## 7.2生产表现

Plugin TCP会在运行一段时间后永久停止事件回调：

- TCP socket仍在读取；
-pull `recv()`缓冲可能仍有数据；
- `tcp:receive` callback不再到达；
- lifecycle事件也可能因同一总预算被拒绝；
-Stats显示队列空但pending bytes接近4 MiB。

## 7.3正确修复

不要把queue Arc直接暴露给worker。

提供原子pop API：

```rust
fn pop_next(&self) -> Option<Event> {
    pop high/normal
    total_bytes -= event_size
}
```

所有消费必须经过该API。

增加不变量：

```text
pending_bytes == high+normal中payload的实际字节总和
```

---

# 8. P0-04：receive merge绕过字节预算

Normal queue已满时：

```text
merge_receive()
→直接把incoming bytes append到最后一条
→total_bytes增加
→立即return
```

在merge之前没有检查：

```text
MAX_PENDING_EVENT_BYTES_PER_PLUGIN
```

## 8.1后果

在慢插件场景：

```text
normal queue满
同handle持续receive
→不断merge
→单条bytes数组持续增长
→4 MiB限制被绕过
```

这与第7节的计数不回收叠加后，既可能：

-永久拒绝后续事件；
-又可能让已合并事件超过预算并持续占用内存。

## 8.2并发预算也不是严格原子的

High queue和normal queue使用不同Mutex，但共享AtomicUsize：

```text
load total
→判断
→fetch_add
```

两个并发push都可能在同一旧值下通过预算，最终超过上限。

## 8.3正确修复

- merge前预留字节预算；
-使用CAS/fetch_update或统一queue-state Mutex；
-限制单事件bytes；
-限制单handle pending bytes；
-达到预算时drop/close策略明确；
-生命周期事件应有独立保留预算。

---

# 9. P0-05：同一TCP连接的事件顺序仍未保证

PMP39增加per-handle `TokioMutex`，但这并不等于FIFO。

## 9.1 accept没有进入conn_handle锁

`tcp:accept` payload包含：

```text
listener_handle
conn_handle
```

worker只读取：

```text
payload["handle"]
```

因此accept事件没有handle lock。

后续receive可能先执行。

## 9.2 high queue会越过更早的receive

Worker始终：

```text
先pop high
再pop normal
```

场景：

```text
normal: receive A
high: disconnect
```

disconnect会先被取出并执行。

插件可能观察：

```text
disconnect
→receive A
```

这违反TCP流生命周期。

## 9.3多个worker的锁竞争不是pop顺序

两个worker依次pop：

```text
receive A
receive B
```

但它们调用`lock().await`的实际调度顺序不一定与pop顺序一致。

即使Tokio Mutex对等待者公平，也不能保证worker A一定先进入等待队列。

## 9.4正确设计

采用每handle mailbox：

```text
plugin入口有界
→按conn_handle分片
→同handle单worker FIFO
→不同handle最多N并发
```

事件顺序必须保证：

```text
accept/connect
→receive chunks按流顺序
→error
→disconnect
```

生命周期优先级只能用于不同连接之间，不能越过同连接已接受的receive。

---

# 10. P0-06：WAL degraded reason仍可能被覆盖并错误清除

PMP39从单Boolean增加：

```text
degraded_reason: Option<String>
```

并试图只清除`ack:`原因。

但`set_degraded()`会直接覆盖旧reason。

## 10.1错误序列

```text
list_pending发现checksum损坏
→reason = corruption:checksum

随后某个ACK retry失败
→reason被覆盖为ack:retry

ACK最终成功且队列清空
→clear_ack_degraded()
→reason清空
→degraded=false
```

WAL corruption仍然存在，但`is_healthy()`可能重新返回true。

Marker或compact错误也可以被同样覆盖。

## 10.2正确模型

使用原因集合或bit flags：

```text
corruption
marker_error
compact_error
io_error
pending_ack_count
```

ACK成功只能清除ACK原因。

不可恢复的corruption必须保持latched，直到重启replay或管理员修复。

---

# 11. P0/P1：compact对marker读取失败默认按clean处理

WAL NotFound分支读取marker：

```text
read_to_string
→parse JSON
→读取clean
→任何失败unwrap_or(true)
```

因此以下情况会被解释为clean：

- marker文件不存在；
-marker无权限；
-marker JSON损坏；
-clean字段缺失。

如果运行期WAL和marker同时异常，或marker先损坏再丢失WAL：

```text
compact返回Ok(0)
```

可能掩盖异常。

应严格区分：

```text
marker clean=true →允许
marker不存在但确认从未Admission →允许
其他读取/解析错误 →Err + degraded
```

不能默认clean。

---

# 12. P1：Marker原子写仍未完全durable

Marker现在采用：

```text
tmp
→sync_all(file)
→rename
```

主体正确。

但parent目录：

```rust
if let Ok(dir) = open(parent) {
    let _ = dir.sync_all().await;
}
```

打开或sync失败均被忽略。

注释承诺durable rename，但实际无法确认目录项已持久化。

此外：

-临时文件名固定，异常退出后可能残留；
-Windows上rename覆盖已有目标需要单独验证；
-marker权限没有像WAL一样收紧。

这属于P1，但应补跨平台故障测试。

---

# 13. P1：认证冲突核验仍不包含event_id

冲突查询：

```sql
SELECT user_id, session_id, connected_at
FROM mp_user_visits
WHERE event_id=$1 OR session_id=$2
LIMIT 1
```

当前核验：

- user_id；
-session_id；
-connected_at。

没有读取和核验existing event_id。

若incoming event_id和session_id分别冲突到不同记录，`LIMIT 1`的结果不确定。

某些异常数据组合可能被错误视为幂等。

应要求单条已有记录同时满足：

```text
event_id
session_id
user_id
connected_at
```

冲突后查不到记录也不应继续提交用户/playtime，应回滚并重试或报告完整性错误。

---

# 14. P1：延迟UserDisconnect仍可回退用户时间

`record_user_disconnect()`现在使用原始`occurred_at`，这是进步。

但SQL无条件执行：

```text
last_seen_at = occurred_at
last_disconnected_at = occurred_at
updated_at = occurred_at
```

旧Disconnect在新Authenticate之后重放时，可能把时间字段回退到更早值。

建议：

```sql
last_disconnected_at = GREATEST(existing, occurred_at)
last_seen_at = GREATEST(existing, occurred_at)
updated_at = GREATEST(existing, occurred_at)
```

并根据Session代际决定是否记录最终断线。

---

# 15. P1：Plugin TCP handle锁表不会回收

Per-handle锁存储于：

```text
HashMap<u64, Arc<TokioMutex<()>>>
```

连接关闭时没有删除对应entry。

插件长期运行并建立大量短连接时，该map会持续增长，直到插件卸载。

应在disconnect/close且没有pending event后清理，或使用分片哈希而不是每handle永久entry。

---

# 16. P1：HighFrequency发送控制消息本身未纳入剩余预算

`flush(timeout)`和`shutdown(timeout)`会：

```text
tx.send(control).await
→随后timeout(timeout, reply)
```

如果channel满，`send().await`本身可能等待较久。

总耗时可能达到：

```text
send等待 + timeout
```

超过共享Shutdown预算。

应以absolute deadline计算：

-发送control剩余时间；
-等待reply剩余时间。

---

# 17. 完成度总表

完成度表示距离正式生产闭环的程度，不是代码量比例。

| 大项 | PMP38 | PMP39 | 当前判断 |
|---|---:|---:|---|
|项目架构与能力建设 | 95% | **96%** |主体功能已经完整 |
|客户端协议与房间玩法 | 90% | **91%** |核心协议稳定 |
|PostgreSQL数据模型 | 91% | **92%** |事务和代际字段较完整 |
|低频持久化可靠性 | 83% | **86%** |主要链路完善，append/marker边界仍阻断 |
|HighFrequency持久化 | 89% | **92%** |deadline和共享预算明显改善 |
|故障恢复与重启一致性 | 82% | **87%** |marker/no-newline/ACK-gap主路径修复 |
|插件宿主与WIT API | 89% | **90%** |主体稳定 |
|Plugin TCP | 72% | **66%** |发现确定性字节计数失效和顺序问题 |
|Real Benchmark | 84% | **84%** |本轮无实质变化 |
|运维、代理与管理接口 | 88% | **89%** |健康信息基础增加 |
|**综合生产完成度** | **约86%** | **约88%** |仍由WAL边界和Plugin TCP决定No-Go |

---

# 18. 核心项完成度

| 项目 | 完成度 | 状态 |
|---|---:|---|
|CI、构建与版本一致性 | **100%** |完成 |
|Room Actor核心状态所有权 | **92%** |基本完成 |
|JoinRoom协议顺序 | **92%** |基本完成 |
|RoundCompleted事务 | **93%** |基本完成 |
|UserAuthenticated幂等事务 | **92%** |主体完成 |
|WAL format与迁移 | **94%** |基本完成 |
|WAL ordered execution | **89%** |ACK-gap和replay主路径较完整 |
|WAL append原子边界 | **60%** |append失败不确定状态未闭环 |
|Admission结果语义 | **72%** |marker失败会产生ghost event |
|Flush/Shutdown fence | **92%** |低频主体完成 |
|Recovery预算与ACK健康 | **91%** |主要完成 |
|Marker生命周期 | **82%** |active missing主路径修复，错误传播仍不足 |
|Marker原子durability | **75%** |rename完成，parent fsync未闭环 |
|Session代际 | **90%** |Playing和普通路径已固定ID |
|Playtime recovery | **87%** |实例和Session基础完整 |
|HighFrequency admission/Shutdown | **93%** |主体完成 |
|HighFrequency DB deadline | **93%** |主体完成 |
|Plugin TCP事件数量有界 | **92%** |队列长度基础正确 |
|Plugin TCP字节有界 | **35%** |消费不减计数，merge绕过预算 |
|Plugin TCP同连接顺序 | **50%** |per-handle锁不足以保证FIFO |
|Persistent Room recovery | **77%** |可执行但仍偏best-effort |
|DLQ recovery | **83%** |主要链路完整 |

---

# 19. 当前生产阻断项完成度

|生产门禁|完成度|是否阻断|
|---|---:|---|
|WAL fsync后marker失败语义 | **35%** | **是** |
|WAL append/ACK不确定尾部 | **45%** | **是** |
|Plugin TCP pending bytes回收 | **20%** | **是** |
|Plugin TCP merge字节预算 | **40%** | **是** |
|Plugin TCP同连接FIFO | **50%** | **是** |
|WAL degraded多原因状态 | **55%** | **是/条件阻断** |
|compact marker读取fail-closed | **65%** | **条件阻断** |
|Marker parent fsync | **75%** | P1 |
|认证冲突完整字段核验 | **82%** | P1 |
|Disconnect时间单调性 | **78%** | P1 |
|HF控制发送共享预算 | **82%** | P1 |

---

# 20. PMP39 Core P0任务清单

## P0-A：修正WAL Admission阶段语义

- [ ] WAL frame fsync成功后不得返回普通Rejected
- [ ] marker失败返回AdmittedDegraded
- [ ] AdmissionOutcome增加degraded状态
- [ ]认证不得因marker失败回滚已durable事件
- [ ] readiness标记marker degraded
- [ ] marker恢复后清除对应原因
- [ ] WAL成功/marker失败故障测试
- [ ]客户端响应与数据库结果对账

## P0-B：处理append/ACK部分写入

- [ ] 记录append前文件长度
- [ ] write/flush/sync错误后检查尾部
- [ ]完整frame重新sync后视为成功
- [ ]半frametruncate回原长度
- [ ]truncate失败进入fatal degraded
- [ ] Admission部分写入测试
- [ ] ACK部分写入测试
- [ ]失败后继续Admission测试

## P0-C：Plugin TCP消费字节回收

- [ ] Queue提供统一pop API
- [ ] pop时准确减少total_bytes
- [ ] worker禁止直接pop Arc queue
- [ ] pending_bytes与实际队列对账
- [ ]队列清空后pending_bytes为0
- [ ]累计100 MiB正常流量不永久拒绝
- [ ] unload时清零字节状态

## P0-D：Plugin TCP严格字节预算

- [ ] merge前检查预算
- [ ]单事件最大bytes
- [ ]每handle最大pending bytes
- [ ]plugin总pending bytes
- [ ]预算预留使用原子CAS或统一Mutex
- [ ] lifecycle保留独立预算
- [ ] overrun时drop/close策略
- [ ]慢插件洪泛内存测试

## P0-E：Plugin TCP同连接FIFO

- [ ] accept使用conn_handle作为排序key
- [ ]同handle只有一个串行mailbox
- [ ] receive chunk保持字节顺序
- [ ] disconnect在已接受receive之后
- [ ] error与disconnect终态明确
- [ ]不同handle允许并发
- [ ] callback timeout后的顺序定义
- [ ]多worker随机调度测试

## P0-F：原因化WAL健康状态

- [ ] degradation使用reason set/bit flags
- [ ] corruption不可被ACK原因覆盖
- [ ] marker错误不可被ACK成功清除
- [ ] pending ACK单独计数
- [ ]每个原因独立恢复条件
- [ ] is_healthy检查全部原因
- [ ]多原因交替故障测试

## P0-G：marker读取和compact fail-closed

- [ ] marker缺失、损坏、无权限分别处理
- [ ] active/clean必须显式解析
- [ ]读取失败不得默认clean
- [ ]运行期marker丢失设置degraded
- [ ]compact不得掩盖marker异常
- [ ] WAL+marker组合故障测试

---

# 21. P1任务清单

## Marker durability

- [ ] parent open失败返回Err
- [ ] parent fsync失败返回Err
- [ ]清理遗留tmp
- [ ]验证Windows原子替换
- [ ]marker权限收紧

## User persistence

- [ ]冲突查询读取event_id
- [ ]要求同一row匹配全部字段
- [ ] conflict后None必须回滚
- [ ] Disconnect时间使用GREATEST
- [ ] GenerationMismatch进入监控

## Plugin TCP

- [ ] handle lock entry回收
- [ ] lifecycle事件不可静默drop
- [ ] dropped bytes分类
- [ ] Stats输出真实pending bytes
- [ ]插件卸载drain/cancel策略

## HighFrequency

- [ ] control send受remaining deadline限制
- [ ] reply等待使用剩余时间
- [ ]共享shutdown总预算集成测试
- [ ]最终accepted/terminal对账

## 测试

- [ ]每个P0添加定向单元或集成测试
- [ ]真实文件系统故障注入
- [ ]Plugin TCP内存和顺序压力测试
- [ ]用户生命周期重放测试

---

# 22. 必须新增的生产门禁测试

## 22.1 Marker失败后的Admission

```text
WAL append和fsync成功
marker rename失败
UserAuthenticated enqueue
```

断言：

```text
业务不能收到RejectedBeforeWal
事件结果和客户端结果一致
服务进入marker degraded
```

## 22.2 Admission部分写入

```text
WAL只写入完整JSON、不写换行并返回Err
```

断言：

-不得把事件一边返回失败一边在重启后执行；
-文件被确认或回滚到原长度。

## 22.3 ACK部分写入

```text
数据库commit
ACK只写一半
ACK retry
```

断言WAL仍可解析，事件不会重复提交。

## 22.4 Plugin TCP累计流量

```text
队列持续被及时消费
累计100 MiB receive
```

断言：

```text
pending_bytes回到接近0
callback持续工作
不会在4 MiB后永久停止
```

## 22.5 Plugin TCP merge预算

```text
normal queue满
同handle持续8 KiB receive
callback阻塞30秒
```

断言总pending bytes不超过配置。

## 22.6 Plugin TCP顺序

```text
accept
receive A
receive B
disconnect
```

断言插件观察顺序完全一致。

## 22.7 多原因WAL degraded

```text
先checksum错误
再ACK失败
ACK恢复
```

断言corruption原因仍存在，`is_healthy=false`。

## 22.8 Marker损坏

```text
active marker JSON损坏
WAL文件缺失
调用compact
```

断言fail-closed，不能按clean处理。

---

# 23. Go / No-Go上线门槛

PMP只有满足以下条件，才能进入Production Candidate。

## WAL Admission

- [ ] fsync后的事件永远不会返回未接受
- [ ] append错误具有确认或回滚语义
- [ ] ACK错误不会污染WAL尾部
- [ ] marker错误只降级，不制造ghost event

## WAL健康与恢复

- [ ] degraded原因互不覆盖
- [ ] marker读取错误fail-closed
- [ ] corruption不可被ACK成功清除
- [ ]重启后客户端事实和数据库事实一致

## Plugin TCP

- [ ] pending bytes在消费后准确减少
- [ ]字节预算严格不可突破
- [ ]同连接事件完全FIFO
- [ ]慢插件和恶意远端不能造成永久拒绝或内存增长
- [ ] lifecycle事件不会越过或丢失关键receive

## Session与数据库

- [ ]认证冲突完整字段一致
- [ ]旧Disconnect不回退新时间
- [ ]Offline严格Session代际
- [ ]访问、在线、playtime可对账

---

# 24. 最终判断

PMP39已经关闭PMP38报告中的大部分显式问题：

```text
active marker异常缺失
无换行WAL末帧
Playing固定Session ID
ACK-gap错误fail-closed
Marker临时文件rename
HighFrequency共享Flush预算
```

但是，本轮发现WAL Admission在线性化点之后仍可能返回普通失败，这会重新产生ghost event；WAL append/ACK失败也没有明确的确认或回滚语义。

Plugin TCP新增的字节预算实现存在确定性计数错误：消费时不减计数，累计约4 MiB后事件回调会永久被拒绝；merge又绕过预算，同连接FIFO也没有真正保证。

最终结论：

> **PMP `(39)`：NO-GO，继续作为 Development Preview。**

当前综合生产完成度约为：

> **88%**

下一轮建议只验收四个闭环：

```text
WAL append/marker Admission语义
→ WAL多原因健康状态
→ Plugin TCP字节计数与严格预算
→ Plugin TCP每连接FIFO
```

这四项关闭并完成故障注入测试后，PMP才适合重新评估Production Candidate。
