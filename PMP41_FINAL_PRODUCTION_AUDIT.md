# PMP `(41)` 最终生产就绪审计与完成度报告

**PMP 版本：** `0.5.1982`  
**Plugin SDK 版本：** `0.5.1982`  
**审计对象：** 本轮上传源码包  
**对比基线：** PMP `(40)` / `0.5.1972`  
**CI 状态：** **已通过**。按用户确认，本次上传项目已通过项目既有的 check、tests、Clippy 与 release build CI 门禁  
**静态校验：** 11 个 TOML、6 个 YML、2 个 JSON 均可解析；27 个 Markdown 文件、32 个本地链接未发现失效引用  
**代码规模：** Server `src` 目录 143 个 Rust 文件，约 48,067 行  
**相对 `(40)`：** 排除随包附带的上一版审计文档后，19 个代码、测试、配置或依赖文件变化，约新增 188 行、删除 51 行  
**发布结论：** **NO-GO，继续作为 Development Preview**

---

# 1. 最终结论

PMP41 是一次方向正确、改动集中的生产一致性迭代。

PMP40 报告中的四个核心验收点已经取得明显进展：

- 精确匹配的 `UserAuthenticated` 重放现在直接返回成功，不再重写用户、IP 和 playtime；
- WAL 在写入前无法读取可靠回滚点时，会在写入前停止；
- WAL append 失败后执行 `set_len + sync_all`，普通失败路径不再只做非持久化 truncate；
- corruption/compact degraded 状态会阻止新的 WAL Admission；
- WAL degraded 从单一状态改为独立原因 bitmask；
- marker 使用临时文件、文件 fsync、rename 和父目录 fsync；
- Plugin TCP stale peek 增加锁后再次核对队头 handle；
- Plugin TCP 单事件增加 raw bytes 上限；
- HighFrequency Flush/Shutdown 的 send 和 reply 使用同一个 absolute deadline；
- HighFrequency Shutdown 增加显式生命周期状态；
- `UserOffline` 增加并持久化 `occurred_at`。

这些改动使综合生产完成度从 PMP40 的约 89% 上升到约 90%。

但当前仍有四类生产阻断：

```text
ACK append失败且rollback失败
→ WAL进入corruption
→ pending ACK循环仍再次调用wal.ack()
→继续向无法确认的WAL尾部append
→可能把可恢复的truncated tail变成mid-file corruption
```

```text
Admission frame可能已经完整写入
→write/flush/sync失败
→rollback本身失败
→业务收到“未接受”
→重启时完整frame仍可能通过checksum并被执行
```

```text
Plugin TCP：
normal receive A已排队
→ high disconnect后到
→ worker始终先取high
→插件观察disconnect后才观察receive A
```

```text
HighFrequency flush/shutdown返回DataLoss或timeout
→main只记录warning
→最终退出状态仍只取决于低频PersistenceWorker
→进程可能以“正常退出”结束，但accepted HF数据没有terminal
```

此外，Plugin TCP 仍没有真正形成“每连接 mailbox”：

- `tcp:accept` 使用 `conn_handle`，不会进入当前 `peek_handle()` 的排序；
- disconnect callback 前就删除 handle lock；
-高优先级队列会改变同一连接的生命周期顺序；
-字节预算仍是非原子的 load + fetch_add；
-receive merge 后的单事件仍可增长超过声明的单事件上限。

因此最终判断仍然是：

> **PMP41：NO-GO。**

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

-源码能够编译；
-现有测试通过；
-Clippy 门禁通过；
-Release 产物可构建。

但本版 `persistence_contracts.rs` 只补充了 `UserOffline.occurred_at` 的构造字段，没有增加与本轮主要修复对应的故障注入测试。

现有 CI 尚未证明：

-精确 Auth 重放前后数据库状态完全不变；
-WAL metadata 读取失败时文件绝不改变；
-write、flush、sync、truncate、rollback-sync 每个阶段的故障行为；
-rollback失败后完整frame与半frame的分类；
-corruption状态下ACK不会继续写入；
-auto-compaction发现损坏后会锁定degraded；
-marker运行期被删除后的处理；
-Plugin TCP accept/receive/disconnect严格顺序；
-receive洪泛下生命周期事件不会丢失；
-HF shutdown失败会影响最终退出状态。

CI通过不改变当前生产No-Go判定。

---

# 3. PMP40 → PMP41 修复状态

| PMP40 问题 | PMP41 状态 | 判断 |
|---|---|---|
|精确Auth重放仍重写旧Session |全字段匹配后立即no-op返回 | **已关闭主路径** |
|WAL rollback point使用默认0 |metadata失败时写入前返回Err | **已关闭** |
|truncate后没有durable确认 |增加`set_len + sync_all` | **普通回滚路径关闭，rollback失败的歧义仍存在** |
|corruption后仍允许新Admission |Admission拒绝corruption/compact状态 | **已关闭Admission路径** |
|WAL degraded原因相互覆盖 |改为独立bitmask | **主体关闭** |
|marker临时错误没有清除接口 |增加`clear_marker_degraded()` | **接口存在，但部分成功修复路径仍不会调用** |
|Plugin TCP stale peek错误锁 |锁后重新核对队头handle | **关闭直接错锁pop，未形成完整FIFO** |
|Plugin TCP单事件无限大 |增加1 MiB incoming event上限 | **入口改善，merge后的事件仍可超过上限** |
|HF send和reply分别使用完整timeout |使用单一absolute deadline | **已关闭** |
|HF Shutdown没有状态机 |增加Open/Requested/ControlSent/Terminated | **主体改善，失败后真实Worker状态仍不准确** |
|UserOffline没有原始时间 |增加`occurred_at`并用于playtime | **已关闭基础** |
|Disconnect/Auth时间可能回退 |Auth upsert使用GREATEST | **Auth改善，Disconnect部分字段仍会回退** |

---

# 4. 已真正关闭的问题

## 4.1 精确 Auth 重放完全 no-op

当 `mp_user_visits` 已有记录，并且以下字段全部匹配：

```text
event_id
session_id
user_id
connected_at
```

当前会立即结束事务并返回成功。

它不再继续：

- upsert `mp_users`；
-更新IP历史；
-重写playtime；
-修改Session代际；
-增加登录次数。

PMP40 的：

```text
Auth A ACK失败
→Offline A
→Auth B
→重启后Auth A重放覆盖Session B
```

主路径已经关闭。

## 4.2 WAL回滚点读取

当前：

```rust
file.metadata().await?
```

metadata失败时不会开始写入。

此前：

```text
metadata失败
→original_len=0
→写入失败
→截断整个既有WAL到0
```

的危险路径已经关闭。

## 4.3 WAL普通失败回滚

append的write、flush或sync失败后：

```text
set_len(original_len)
→sync_all()
```

普通回滚成功时，调用方得到Rejected，文件尾部也被持久恢复到写入前长度。

## 4.4 Admission原因化策略

当前新Admission在以下状态会被拒绝：

```text
DEGRADED_CORRUPTION
DEGRADED_COMPACT
```

以下状态仍允许Admission：

```text
DEGRADED_ACK
DEGRADED_MARKER
```

Marker失败仍以durable event处理，不再让业务回滚已fsync事件。

## 4.5 Plugin TCP stale-peek直接错误

多个worker先peek同一个handle并等待锁时，后取得锁的worker会再次检查队头。

如果队头已变化：

```text
释放错误handle锁
→重新循环
```

PMP40中“拿着A锁弹出B事件”的直接代码路径已经关闭。

## 4.6 HighFrequency timeout预算

Flush和Shutdown现在使用一个absolute deadline：

```text
send消耗一部分预算
→reply只使用剩余预算
```

不会再出现send等待一份完整timeout、reply再等待一份完整timeout。

---

# 5. P0-01：WAL进入corruption后，ACK仍继续写入

`admit()`已经会拒绝：

```text
DEGRADED_CORRUPTION
DEGRADED_COMPACT
```

但 `ack()` 只检查：

```text
replay_succeeded
```

然后继续调用：

```rust
append_frame()
```

## 5.1 确定性故障链

```text
数据库事件已经commit
→第一次ACK append失败
→rollback也失败
→WAL设置DEGRADED_CORRUPTION
→ACK进入pending_acks

下一轮ACK retry
→wal.ack()不检查corruption
→继续向不确定尾部append
```

如果原尾部是半条ACK或半条Admission，新ACK会追加在其后，形成：

```text
partial JSON + complete ACK JSON
```

该坏行不再是单纯的truncated tail，而是mid-file corruption。

下一次启动确定性fail-closed。

## 5.2 正确策略

reason-based策略必须同时覆盖：

- Admission；
- ACK；
- compact；
-list_pending。

建议：

```text
ACK degraded：
允许ACK retry

Marker degraded：
允许ACK

Corruption/Compact fatal：
禁止任何WAL append，包括ACK
→停止Worker推进
→报告fatal
```

---

# 6. P0-02：rollback失败仍存在ghost-event歧义

PMP41已经正确处理普通rollback成功路径。

但rollback失败时仍统一：

```text
设置corruption
→向调用方返回Err
```

它没有判断失败前的新frame是：

-完全没有写入；
-部分写入；
-完整写入但sync结果不确定；
-已经完整持久化。

## 6.1 完整frame可能在重启后执行

场景：

```text
write_all成功
sync_data返回Err
set_len或rollback sync失败
admit返回Err
业务拒绝认证或回滚命令
```

如果该frame实际完整存在，重启replay会：

-成功解析；
-checksum通过；
-执行事件。

最终再次出现：

```text
客户端/业务看到失败
数据库以后出现成功事实
```

## 6.2 正确分类

失败后应读取并验证写入区间：

### 完整frame存在且checksum正确

```text
再次尝试sync
→确认成功后视为AdmittedDegraded/FatalAdmitted
```

不得返回普通Rejected。

### frame不完整

```text
truncate回原长度
→sync_all
→确认后返回Rejected
```

### 无法读取、确认或回滚

```text
进入FatalUnknown
→阻止服务继续接受客户端事实
→不能让调用方按普通失败继续运行
```

---

# 7. P0-03：auto-compaction发现损坏后没有锁定WAL健康状态

`compact()`在以下错误中会返回Err：

- JSON解析失败；
-未来版本；
-checksum失败；
-temp写入/rename失败；
-parent同步失败。

但多数错误路径没有：

```text
mark_degraded(CORRUPTION/COMPACT)
```

Worker自动compact失败时只执行：

```text
warn!("auto-compaction failed")
```

## 7.1 风险

```text
auto-compaction首先发现WAL中间损坏
→只记warning
→WAL degraded bit仍为空
→新Admission继续append
```

新frame可能把当前可修复边界进一步复杂化。

## 7.2 修复

-解析、checksum、版本错误：`DEGRADED_CORRUPTION`；
-temp、rename、parent sync等compact事务错误：`DEGRADED_COMPACT`；
-compact成功后只清除可恢复的compact bit；
-corruption必须latched。

---

# 8. P0-04：运行期marker缺失被静默忽略

`mark_marker_active()`当前：

```rust
if !marker_path.exists() {
    return Ok(());
}
```

正常启动后marker应始终存在。

运行期marker缺失表示：

-被人工删除；
-目录部分丢失；
-挂载异常；
-文件系统故障。

## 8.1 当前后果

```text
marker运行期被删除
→后续Admission正常写WAL
→mark_marker_active返回Ok
→不设置marker degraded
```

如果之后WAL也丢失：

```text
下次启动无marker、无WAL
→被当作首次启动
→数据丢失无法检测
```

## 8.2 正确处理

marker不存在时应：

-在io_gate内重新创建active marker；
-写入当前high-water sequence；
-若重建失败则设置MARKER degraded；
-不能返回普通Ok。

---

# 9. P0-05：Marker degraded仍可能无法在运行期自恢复

`clear_marker_degraded()`已经存在。

但只有：

```text
marker当前clean
→clean→active重写成功
```

时才调用。

如果：

```text
rename已经成功
→parent fsync失败
→marker实际已经active
→MARKER degraded被设置
```

下一次Admission读取到：

```text
clean=false
```

会直接返回Ok，不会再次持久验证或清除marker degraded。

服务会一直：

```text
is_healthy=false
AdmissionOutcome::AdmittedDegraded
```

直到重启。

需要为active marker增加显式repair/verify流程。

---

# 10. P0-06：Plugin TCP仍没有真正的每连接FIFO

当前实现仍是：

```text
plugin级high/normal共享队列
→多个worker peek队头handle
→获取per-handle Mutex
→再次核对
→pop
```

二次核对修复了stale peek，但没有建立完整的TCP生命周期顺序。

## 10.1 disconnect可越过更早的receive

`pop()`永远：

```text
先high
后normal
```

场景：

```text
normal：
receive A

随后high：
disconnect
```

worker会先取disconnect。

插件观察：

```text
disconnect
→receive A
```

这不符合TCP字节流生命周期。

## 10.2 accept没有进入conn_handle排序

`tcp:accept` payload：

```json
{
  "listener_handle": ...,
  "conn_handle": ...
}
```

`peek_handle()`只读取：

```json
"handle"
```

因此accept没有per-connection锁。

后续receive可能与accept callback并发，甚至先完成。

## 10.3 disconnect前过早删除handle lock

当前在执行disconnect callback之前：

```text
remove_handle_lock(handle)
```

worker仍持有旧lock guard。

其他worker可以为同一handle创建一个新Mutex，导致：

- disconnect在旧锁下执行；
-残留receive在新锁下执行；
-二者并发。

## 10.4 正确架构

应改为：

```text
plugin级有界入口
→按conn_handle路由到每连接mailbox
→同一handle单消费者FIFO
→不同handle最多N并发
```

同一连接必须严格保证：

```text
accept/connect
→receive chunks按字节顺序
→error
→disconnect
```

---

# 11. P0-07：Plugin TCP生命周期事件仍可能被receive洪泛挤掉

虽然high和normal分开，但总字节预算共享。

如果normal receive已占满约4 MiB：

```text
disconnect/error到达
→total_bytes + lifecycle_bytes > budget
→生命周期事件被直接drop
```

因此receive洪泛仍能让插件丢失：

- accept；
-error；
-disconnect。

此外high queue本身满时会丢最旧生命周期事件。

## 11.1 修复

生命周期事件应有：

-独立保留字节预算；
-或在生命周期事件到达时优先驱逐normal receive；
-不能因为normal数据占满总预算而丢失disconnect。

---

# 12. P0-08：Plugin TCP字节预算仍不是严格原子

High和normal使用不同Mutex，共享：

```text
AtomicUsize total_bytes
```

Push逻辑：

```text
load total
→判断是否超限
→fetch_add
```

多个连接并发push时，可以同时基于旧值通过预算。

单次overshoot受事件大小限制，但多个连接并发时仍可能超过配置上限。

需要：

- CAS预留；
-或统一queue-state Mutex；
-失败时回滚预留。

---

# 13. P0-09：receive merge后的单事件仍可超过单事件上限

入口只检查：

```text
incoming_bytes <= 1 MiB
```

但queue满后可以反复把多个小receive合并到最后一条事件。

合并后的单条payload可增长到接近插件总预算：

```text
约4 MiB raw bytes
```

因此 `MAX_EVENT_RAW_BYTES=1 MiB` 并没有约束merge后的事件。

而payload使用JSON数字数组，实际堆内存显著高于raw bytes。

应在merge前同时检查：

-合并后的单事件raw bytes；
-单handle pending bytes；
-plugin总pending bytes。

---

# 14. P0-10：HighFrequency失败不影响最终进程退出状态

主Shutdown流程会记录：

```text
HF flush error
HF shutdown error
```

但不会把它们写入：

```text
persistence_ok
```

最终退出状态只取决于低频PersistenceWorker。

## 14.1 后果

以下情况都可能最终返回成功退出：

- HF Flush返回DataLoss；
-HF Shutdown返回timeout；
-HF Shutdown control未送达；
-accepted Touch/Judge仍未terminal。

日志有warning，但systemd、Docker和编排系统看到的是正常退出。

## 14.2 与项目目标冲突

HighFrequency允许明确、可观测的有限丢失，但不能把：

```text
未完成的graceful shutdown
```

报告成clean shutdown。

建议统一：

```text
persistence_ok =
  low_frequency_ok
  && high_frequency_ok
```

并在退出错误中分别列出：

- accepted；
-committed；
-dropped；
-pending；
-watermark；
-last accepted sequence。

---

# 15. P0/P1：HighFrequency状态机仍不能表达“Worker已退出但结果失败”

Worker处理Shutdown后，无论最终结果是：

- Ok；
-DataLoss；
-Timeout；
-Pending；

都会发送reply并：

```text
break
```

调用方如果收到Err，却把：

```text
shutdown_state = OPEN
```

但此时Worker通常已经退出，channel很快关闭。

这个状态不是Open，也不能真正重试。

更准确的状态应为：

```text
TerminatedClean
TerminatedDataLoss
TerminatedFailed
ControlNotSent
```

只有control根本没有发送时才允许重新请求。

---

# 16. P1：低频Flush/Shutdown发送阶段不受调用方deadline限制

低频PersistenceWorker：

```text
send_gate内
→tx.send(control).await
→发送成功后才开始timeout(rx)
```

如果channel满或Worker长时间不接收，control send本身可能超过调用方timeout。

Shutdown还会在等待期间保持：

- send_gate；
-closed状态。

建议与HighFrequency一致，使用单个absolute deadline覆盖：

- control send；
- Worker处理；
-reply。

---

# 17. P1：Rejected Admission可能留下clean marker + 空WAL

`append_frame_inner()`使用：

```text
create(true)
```

在首次Admission失败时，即使回滚成功到长度0，也可能留下空WAL文件。

如果进程随后异常退出，没有执行正常compact：

```text
clean marker
空WAL文件
```

下次启动的consistency check会把任何空WAL视为corruption并拒绝启动。

这是保守但不必要的启动失败。

安全规则可以调整为：

```text
clean marker + empty WAL
→等价于无WAL
→删除空文件并继续
```

active marker + empty WAL仍应fail-closed。

---

# 18. P1：Disconnect部分时间字段仍可回退

`last_disconnected_at`已经使用`GREATEST`。

但：

```text
last_seen_at = occurred_at
updated_at = occurred_at
```

延迟旧Disconnect在新Auth之后重放时，仍可能把字段回退到更早时间。

应全部使用：

```sql
GREATEST(existing, occurred_at)
```

或绑定Session代际更新。

---

# 19. P1：Auth精确no-op的测试仍不足

代码主逻辑已经正确。

但需要真实数据库测试证明：

```text
第一次Auth事务提交
→ACK失败
→离线
→第二Session认证
→旧Auth重放
```

重放前后以下表完全不变：

- `mp_user_visits`
- `mp_users`
- `user_ip_history`
- `playtime`

当前契约测试只检查事件kind和payload，无法证明数据库幂等。

---

# 20. 完成度总表

完成度表示距离正式生产闭环的程度，不是代码量比例。

| 大项 | PMP40 | PMP41 | 当前判断 |
|---|---:|---:|---|
|项目架构与能力建设 | 96% | **96%** |主体能力完整 |
|客户端协议与房间玩法 | 91% | **91%** |本轮无核心回归 |
|PostgreSQL数据模型 | 92% | **94%** |Auth精确重放明显改善 |
|低频持久化可靠性 | 87% | **89%** |普通rollback闭环，fatal边界仍需修 |
|HighFrequency持久化 | 91% | **91%** |预算改善，退出语义仍不足 |
|故障恢复与重启一致性 | 88% | **90%** |WAL原因状态和rollback改善 |
|插件宿主与WIT API | 90% | **90%** |主体稳定 |
|Plugin TCP | 70% | **72%** |stale peek改善，正式FIFO仍未形成 |
|Real Benchmark | 84% | **84%** |本轮无实质变化 |
|运维、代理与管理接口 | 90% | **90%** |主体稳定 |
|**综合生产完成度** | **约89%** | **约90%** |仍由WAL fatal边界、Plugin TCP和HF退出语义决定No-Go |

---

# 21. 核心项完成度

| 项目 | 完成度 | 状态 |
|---|---:|---|
|CI、构建与版本一致性 | **100%** |完成 |
|Room Actor核心状态所有权 | **92%** |基本完成 |
|JoinRoom协议顺序 | **92%** |基本完成 |
|RoundCompleted事务 | **93%** |基本完成 |
|UserAuthenticated首次事务 | **94%** |基本完成 |
|UserAuthenticated精确重放 | **95%** |代码主路径完成，缺故障测试 |
|WAL format与迁移 | **94%** |基本完成 |
|WAL ordered execution | **90%** |主体完成 |
|WAL普通append rollback | **90%** |普通错误路径完成 |
|WAL fatal未知状态 | **55%** |rollback失败仍有事实歧义 |
|WAL Admission原因策略 | **90%** |Admission已完成，ACK/compact未统一 |
|WAL多原因健康 | **91%** |bitmask完成，marker恢复仍不足 |
|Flush/Shutdown fence | **92%** |低频主体完成 |
|Recovery预算与ACK健康 | **92%** |主体完成 |
|Session代际 | **92%** |主体完成 |
|Playtime重放精度 | **89%** |Offline时间已补，Disconnect仍有回退 |
|HighFrequency admission/DB deadline | **94%** |主体完成 |
|HighFrequency退出语义 | **70%** |失败未进入最终退出状态 |
|Plugin TCP事件数量有界 | **92%** |基本完成 |
|Plugin TCP字节预算 | **82%** |有上限基础，但非严格原子 |
|Plugin TCP同连接FIFO | **58%** |stale peek修复，生命周期顺序未闭环 |
|Persistent Room recovery | **77%** |可执行但仍偏best-effort |
|DLQ recovery | **83%** |主要链路完整 |

---

# 22. 当前生产阻断项完成度

| 生产门禁 | 完成度 | 是否阻断 |
|---|---:|---|
|corruption状态下禁止ACK append | **35%** | **是** |
|rollback失败后的Admission事实分类 | **50%** | **是** |
|compact错误锁定degraded | **60%** | **是/条件阻断** |
|运行期marker缺失检测与修复 | **55%** | **是/条件阻断** |
|Plugin TCP每连接FIFO | **58%** | **是** |
|disconnect不越过receive | **45%** | **是** |
|accept进入连接排序 | **45%** | **是** |
|Plugin TCP生命周期保留预算 | **55%** | **是/正式能力阻断** |
|HF失败进入最终退出状态 | **60%** | **是** |
|HF终止状态准确性 | **70%** | P1/条件阻断 |
|低频control共享deadline | **75%** | P1 |
|clean marker + 空WAL处理 | **75%** | P1 |
|Disconnect时间完全单调 | **82%** | P1 |

---

# 23. PMP41 Core P0任务清单

## P0-A：WAL fatal状态禁止所有写入

- [ ] `ack()`检查degraded reason
- [ ] corruption/compact状态禁止ACK
- [ ] pending ACK在fatal状态停止重试
- [ ] Worker进入fatal/not-ready
- [ ]不得向坏尾部继续append
- [ ] ACK rollback失败后再次retry测试
- [ ] corruption状态Admission/ACK/compact一致

## P0-B：rollback失败后的事实分类

- [ ]读取original_len之后的尾部范围
- [ ]完整frame且checksum正确时确认Admission
- [ ]完整frame重新sync成功后返回AdmittedDegraded
- [ ]不完整frame执行durable rollback
- [ ]无法确认时进入FatalUnknown
- [ ]FatalUnknown阻止继续认证和业务变更
- [ ] Admission完整写入但rollback失败测试
- [ ] ACK完整写入但rollback失败测试

## P0-C：compact错误锁定WAL健康

- [ ] parse/checksum/version错误设置CORRUPTION
- [ ] temp/rename/sync错误设置COMPACT
- [ ] auto-compact失败后阻止不安全Admission
- [ ] compact成功后只清除可恢复COMPACT bit
- [ ] corruption不可自动清除
- [ ] auto-compact首先发现损坏测试

## P0-D：Marker运行期完整性

- [ ] marker缺失时重建active marker
- [ ]重建使用当前high-water
- [ ]重建失败设置MARKER degraded
- [ ]active marker验证成功后清除MARKER bit
- [ ] parent fsync失败后的自恢复测试
- [ ]运行期删除marker测试
- [ ] marker+WAL同时丢失检测测试

## P0-E：Plugin TCP每连接mailbox

- [ ]替换plugin级peek-lock-pop
- [ ]按conn_handle建立FIFO mailbox
- [ ]同handle单消费者
- [ ]不同handle有界并发
- [ ] accept/connect进入同一mailbox
- [ ] receive严格保持TCP流顺序
- [ ] disconnect晚于已接受receive
- [ ] mailbox完全drain后清理

## P0-F：Plugin TCP生命周期可靠性

- [ ]生命周期事件独立保留字节预算
- [ ] normal receive满时优先驱逐receive
- [ ] receive洪泛不能丢disconnect/error
- [ ] high queue满策略不能静默破坏状态
- [ ]生命周期drop必须触发连接关闭或插件重同步
- [ ] receive洪泛+disconnect测试

## P0-G：Plugin TCP严格字节预算

- [ ]总预算CAS预留或统一Mutex
- [ ] merge后单事件仍不超过上限
- [ ]单handle pending bytes限制
- [ ]生命周期保留预算
- [ ] JSON payload实际内存压力测试
- [ ]多连接并发push不可超预算

## P0-H：HighFrequency关闭结果进入进程状态

- [ ] HF flush失败设置整体persistence失败
- [ ] HF shutdown失败设置整体persistence失败
- [ ] DataLoss不得以clean exit结束
- [ ]输出accepted/committed/dropped/pending
- [ ]退出码反映HF不完整关闭
- [ ]channel满导致control send失败测试
- [ ] DB outage graceful shutdown测试

---

# 24. P1任务清单

## HighFrequency状态机

- [ ]区分ControlNotSent
- [ ]区分TerminatedClean
- [ ]区分TerminatedDataLoss
- [ ]区分TerminatedFailed
- [ ]Worker已退出时不得回到Open
- [ ]第二次Shutdown语义明确

## 低频控制

- [ ] control send受absolute deadline限制
- [ ] reply使用剩余预算
- [ ] send_gate等待纳入timeout
- [ ] shutdown deadline总墙钟测试

## WAL文件边界

- [ ] clean marker + 空WAL自动清理
- [ ] active marker + 空WAL仍fail-closed
- [ ]回滚后的total_bytes校准
- [ ] append_missing_newline后total_bytes校准
- [ ] parent同步错误一致处理

## 用户生命周期

- [ ] Disconnect的last_seen_at使用GREATEST
- [ ] Disconnect的updated_at使用GREATEST
- [ ] Session代际条件进入Disconnect SQL
- [ ] GenerationMismatch管理指标
- [ ] legacy缺代际DLQ进入quarantine

##测试

- [ ] Auth精确重放全表diff
- [ ] WAL逐阶段fault injection
- [ ] Plugin TCP顺序随机调度
- [ ]生命周期洪泛
- [ ] HF退出码测试

---

# 25. 必须新增的生产门禁测试

## 25.1 Auth完全no-op

```text
Auth A提交
ACK A失败
Offline A
Auth B
重启重放Auth A
```

断言重放前后：

```text
mp_users无变化
user_ip_history无变化
playtime保持Session B
login_count不变
```

## 25.2 ACK corruption停止写入

```text
ACK append失败
rollback失败
WAL标记corruption
pending ACK再次重试
```

断言文件长度不再变化，Worker进入fatal。

## 25.3 完整frame但rollback失败

```text
Admission完整写入
sync返回错误
truncate/rollback失败
```

断言不能向业务返回普通Rejected后继续服务。

## 25.4 Auto compact损坏

```text
WAL中间checksum损坏
触发auto compact
```

断言WAL进入CORRUPTION degraded，新Admission被拒绝。

## 25.5 Marker运行期删除

```text
服务正常运行
删除marker
写入新Admission
```

断言marker被重建或服务进入MARKER degraded，不能静默成功。

## 25.6 Plugin TCP完整顺序

```text
accept
receive A
receive B
disconnect
```

随机调度4个worker数千次，插件观察顺序必须始终一致。

## 25.7 Lifecycle预算

```text
normal receive占满总字节预算
随后发送disconnect
```

断言disconnect不会丢失，也不会先于更早receive。

## 25.8 HF DataLoss退出状态

```text
accepted HF item
数据库不可用
Flush/Shutdown返回DataLoss
```

断言主进程非零退出，并输出accepted/committed/dropped/pending对账。

---

# 26. Go / No-Go上线门槛

PMP只有满足以下条件才能进入Production Candidate。

## WAL

- [ ] corruption状态下不再进行任何append
- [ ] rollback失败能区分完整、部分和未知frame
- [ ]未知Admission状态阻止服务继续放行业务
- [ ]compact错误会锁定正确degraded原因
- [ ]marker运行期缺失可检测并修复

##数据库幂等

- [ ]精确Auth重放完全no-op测试通过
- [ ]旧Auth不能覆盖新Session
- [ ]Offline/Disconnect时间与代际保持单调
- [ ]访问、在线、playtime重启后可对账

## Plugin TCP

- [ ]每连接严格FIFO
- [ ]accept先于receive
- [ ]disconnect晚于receive
- [ ]生命周期事件不被receive洪泛丢失
- [ ]事件数、raw bytes和实际内存都严格有界

## HighFrequency

- [ ]关闭失败影响最终退出状态
- [ ]accepted item全部有terminal
- [ ]control send失败不可被视为graceful
- [ ]退出日志包含完整HF对账

---

# 27. 最终判断

PMP41已经关闭PMP40报告中的多个关键问题：

```text
精确Auth重放副作用
WAL回滚点默认0
普通truncate不持久
Admission忽略fatal degraded
Plugin TCP stale peek直接错锁
HF send/reply双重timeout
```

但生产边界仍未完全闭环：

- corruption后ACK仍继续写WAL；
-rollback失败后事件事实仍可能不确定；
-auto-compaction损坏不锁定degraded；
-marker运行期缺失被静默忽略；
-Plugin TCP仍不是每连接FIFO；
-生命周期事件仍可被receive预算挤掉；
-HF失败仍可能以正常进程退出结束。

最终结论：

> **PMP `(41)`：NO-GO，继续作为 Development Preview。**

当前综合生产完成度约为：

> **90%**

下一轮建议只验收四个闭环：

```text
WAL fatal状态与未知frame分类
→ Marker/compact健康闭环
→ Plugin TCP每连接mailbox与生命周期可靠性
→ HighFrequency关闭结果进入进程退出状态
```

完成这些定向故障测试后，PMP才适合重新评估Production Candidate。
