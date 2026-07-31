# PMP `(40)` 最终生产就绪审计与完成度报告

**PMP 版本：** `0.5.1972`  
**Plugin SDK 版本：** `0.5.1972`  
**审计对象：** 本轮上传源码包  
**对比基线：** PMP `(39)` / `0.5.1958`  
**CI 状态：** **已通过**。按用户确认，本次上传项目已通过项目既有的 check、tests、Clippy 与 release build CI 门禁  
**静态校验：** 11 个 TOML、6 个 YML、2 个 JSON 均可解析；26 个 Markdown 文件、32 个本地链接未发现失效引用  
**代码规模：** Server 143 个 Rust 文件，约 47,931 行  
**相对 `(39)`：** 排除附带审计文档后，12 个代码或依赖文件变化，约新增 302 行、删除 131 行  
**发布结论：** **NO-GO，继续作为 Development Preview**

---

# 1. 最终结论

PMP40 是一次有效、集中且方向正确的稳定性迭代。

PMP39 的主要问题中，以下项目已经有真实代码修复：

- 增加 `AdmissionOutcome::AdmittedDegraded`；
- WAL 已 fsync、marker 更新失败时，不再把事件作为普通 admission failure 返回；
- WAL degraded 从单字符串改成独立 bitmask；
- ACK 成功只清除 ACK degraded，不再直接清除 corruption/marker/compact；
- marker 使用临时文件、fsync、rename 和父目录 fsync；
- WAL append 失败开始尝试回滚到写入前长度；
- Plugin TCP 增加统一 `pop()`，消费时会减少 `pending_bytes`；
- receive merge 已纳入总字节预算；
- Plugin TCP 增加 handle lock 回收；
-认证访问冲突开始核验 `event_id/session_id/user_id/connected_at` 全字段；
-HighFrequency control send 增加 timeout；
- `UserDisconnect` 时间字段使用 `GREATEST` 基础。

这些修改使综合生产完成度从 PMP39 的约 88% 上升到约 89%。

但本轮深入追踪后，仍存在四组直接阻断 Production Candidate 的问题：

```text
UserAuthenticated 已完整提交数据库
→ WAL ACK 失败
→稍后用户离线并建立新 Session
→旧 UserAuthenticated 在重启后精确幂等重放
→访问记录冲突被识别为同一事件
→代码仍继续执行用户 upsert 和 playtime upsert
→旧 Session 再次被设置为 online
```

```text
WAL append 开始前读取文件长度失败
→ original_len 使用 unwrap_or(0)
→随后写入失败
→回滚可能把整个既有 WAL 截断到 0
```

```text
WAL write/flush/sync失败
→ set_len(original_len)成功
→没有对truncate执行sync_all
→调用方收到Rejected
→崩溃后完整或部分frame仍可能恢复
→业务事实和WAL事实再次分叉
```

```text
Plugin TCP多个worker同时peek同一个队头handle
→分别等待同一handle锁
→第一个worker处理并弹出队头
→第二个worker获得旧handle锁后弹出新的、可能属于另一个handle的事件
→事件在错误的handle锁下执行
→同连接FIFO仍不成立
```

同时：

- high-priority disconnect仍可越过更早的normal receive；
- `tcp:accept` payload使用`conn_handle`而不是`handle`，不会进入当前handle锁；
-HighFrequency Shutdown控制消息发送超时后，`closed`保持true，但Worker并未收到Shutdown，后续也无法重试；
-精确Auth重放和UserOffline时间语义仍可能污染playtime；
-marker degraded在临时错误恢复后没有清除路径。

因此最终判定仍为：

> **PMP40：NO-GO。**

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

-代码可以编译；
-现有测试通过；
-Clippy门禁通过；
-Release产物可构建。

但当前测试仍未覆盖：

-已提交Auth的ACK失败后重放；
-旧Auth重放覆盖新Session；
-WAL metadata读取失败；
-WAL write成功但sync失败；
-truncate成功但未持久化；
-WAL corruption degraded后继续admit；
-marker临时故障恢复；
-Plugin TCP多worker stale peek；
-disconnect越过receive；
-accept未进入handle排序；
-HF Shutdown control send timeout；
-UserOffline延迟重放期间的停机时间。

CI通过不改变当前生产No-Go判定。

---

# 3. PMP39 → PMP40修复状态

| PMP39问题 | PMP40状态 | 判断 |
|---|---|---|
|WAL fsync后marker失败返回普通Err |增加`AdmittedDegraded`，不再回滚已durable事件 | **已关闭主路径** |
|WAL append失败没有回滚 |记录原长度并尝试`set_len`回滚 | **部分修复，回滚自身不安全** |
|WAL degraded原因互相覆盖 |改为bitmask | **已关闭主体** |
|marker直接写入 |改为tmp+rename+parent fsync | **主体关闭** |
|Plugin TCP消费不减少pending bytes |统一`pop()`减少计数 | **已关闭主体** |
|receive merge绕过预算 |merge前检查总预算 | **主体改善，预算并发仍非严格原子** |
|Plugin TCP同连接无序 |增加peek+handle mutex | **未真正闭环** |
|handle锁不回收 |disconnect时删除entry | **基础完成，但删除时机有问题** |
|认证冲突未核验event_id |增加全字段核验 | **字段核验关闭，但精确重放仍有状态副作用** |
|HF control send无timeout |增加send timeout | **改善，但Shutdown失败状态错误** |
|Disconnect时间可回退 |`last_disconnected_at`使用GREATEST | **部分关闭，last_seen/updated仍可回退** |

---

# 4. 已真正关闭的问题

## 4.1 Marker失败不再产生普通业务回滚

WAL frame成功append、flush、fsync后，如果：

```text
mark_marker_active失败
```

当前会：

-保留Admission成功；
-设置`DEGRADED_MARKER`；
-记录warning；
-Worker返回`AdmittedDegraded`或`WalOnly`；
-调用方不会把事件当作未进入持久化系统。

PMP39报告中的marker ghost event主路径已经关闭。

## 4.2 WAL健康原因独立

当前使用bitmask：

```text
ACK
CORRUPTION
MARKER
COMPACT
```

ACK恢复只清除ACK bit。

corruption不再因为后续ACK成功而被自动清除。

## 4.3 Plugin TCP消费字节回收

`PluginEventChannel::pop()`会：

-从high或normal queue弹出事件；
-按事件payload重新计算大小；
-从`total_bytes`中扣除。

累计正常流量不再必然在4MiB后永久停止callback。

## 4.4 receive merge纳入总预算

normal queue满时，只有：

```text
total_bytes + incoming_bytes <= budget
```

才允许merge。

PMP39中merge无条件突破预算的直接路径已关闭。

## 4.5 Auth冲突全字段核验

冲突后查询已包含：

- user_id；
-session_id；
-connected_at；
-event_id。

不再只比较user_id。

---

# 5. P0-01：精确幂等Auth重放仍会重写旧Session状态

这是PMP40当前最严重的数据库状态问题。

## 5.1 访问记录识别为精确幂等

`mp_user_visits`插入冲突后，如果已有记录与incoming完全匹配：

```text
event_id相同
session_id相同
user_id相同
connected_at相同
```

代码将：

```rust
is_new_visit = false
```

这证明该事件对应的原事务已经提交。

PostgreSQL事务的原子性意味着：

- visit插入；
-用户upsert；
-IP；
-playtime online；

已经在同一次事务中全部完成。

## 5.2 但代码仍继续重新执行后续状态更新

即使是精确幂等重放，代码仍继续：

```text
upsert mp_users
→last_connected_at写回旧时间
→upsert playtime
→session_start写回旧连接时间
→server_instance_id写回旧实例
→session_id写回旧Session
```

只是不再增加`login_count`。

## 5.3 确定性故障场景

```text
Auth A数据库事务成功
→WAL ACK失败

Offline A成功并ACK
Auth B成功并ACK
→用户当前处于Session B

服务器重启
→WAL只剩Auth A
→精确幂等冲突
→代码重新执行playtime upsert
→Session A重新online
```

最终：

-当前真实Session B被旧Session A覆盖；
-playtime session_start回退；
-用户出现phantom online；
-后续Offline B可能因generation不匹配而无法关闭。

## 5.4 正确修复

精确匹配已有visit时应直接：

```text
rollback当前空事务或commit no-op
→返回成功
```

不能再次修改用户和playtime。

由于原事务是原子的，visit存在就是完整处理成功的幂等凭证。

---

# 6. P0-02：WAL原长度读取失败可能截断整个既有WAL

`append_frame_inner()`记录原长度：

```rust
file.metadata().await.map(|m| m.len()).unwrap_or(0)
```

## 6.1 metadata错误被解释为长度0

如果metadata读取因：

-文件系统I/O错误；
-临时权限问题；
-底层存储异常；

返回Err，`original_len`被设置为0。

随后write/flush/sync再失败时，代码执行：

```rust
file.set_len(0)
```

可能把已经存在的完整历史WAL全部截断。

## 6.2 影响

这不是单个新事件失败，而是可能删除：

-所有未ACK Admission；
-所有ACK历史；
-当前恢复顺序信息；
-高水位上下文。

## 6.3 正确修复

metadata读取失败必须在写入前立即返回错误：

```rust
let original_len = file.metadata().await?.len();
```

不能使用0作为默认回滚点。

---

# 7. P0-03：WAL回滚没有durable确认

写入、flush或sync失败后，当前执行：

```rust
file.set_len(original_len)
```

但没有：

```text
flush
sync_all
父目录sync
```

## 7.1 调用方收到失败，但回滚可能只在内存/缓存中

场景：

```text
frame完整写入
sync_data返回错误
set_len(original_len)返回成功
admit返回Err
业务回滚
进程随后崩溃
```

如果truncate元数据没有durable落盘，重启后完整frame仍可能存在并被执行。

## 7.2 半frame同样存在风险

如果write_all部分写入后失败，set_len虽然逻辑回滚，但没有同步。

崩溃后可能保留半frame。

后续Admission若已经append，会把半frame变成mid-file corruption。

## 7.3 ACK路径也使用同一append函数

数据库事务已经成功后，ACK失败路径同样可能：

-留下完整ACK但返回失败；
-留下半ACK；
-随后重试ACK；
-损坏WAL尾部；
-或错误重复重放。

## 7.4 正确修复

失败回滚必须：

```text
set_len(original_len)
→sync_all
→必要时父目录sync
```

如果rollback durability无法确认：

-设置fatal corruption；
-停止新Admission；
-不能返回普通可继续错误。

---

# 8. P0-04：WAL corruption后仍允许新的Admission

append回滚失败时会设置：

```text
DEGRADED_CORRUPTION
```

但`admit()`只检查：

```text
replay_succeeded
```

不检查：

```text
corruption/compact fatal degraded
```

## 8.1 后果

如果WAL尾部已经无法确认：

```text
下一条Admission仍会继续append
```

可能把原本可识别的truncated tail变成mid-file坏行，下一次replay确定性fail-closed。

## 8.2 原因化Admission策略

建议：

```text
ACK degraded
→可继续Admission

MARKER degraded
→可继续Admission并返回AdmittedDegraded

CORRUPTION/COMPACT degraded
→拒绝新Admission并进入not-ready
```

不能只判断是否有任何degraded，也不能完全忽略degraded。

---

# 9. P0-05：Plugin TCP的peek-lock-pop不是原子FIFO

PMP40尝试通过：

```text
peek队头handle
→获取该handle锁
→pop队头事件
→执行callback
```

保证同连接顺序。

但peek、获取锁和pop不是同一原子操作。

## 9.1 多worker stale peek

假设队头为handle A事件A1，4个worker被唤醒：

```text
worker1 peek A
worker2 peek A
worker3 peek A
```

worker1先取得A锁并pop A1。

worker2随后取得A锁，但此时队头可能已经是：

```text
handle B事件B1
```

worker2会在持有A锁时pop并执行B1。

与此同时另一个worker可能持有B锁执行B2。

结果：

-同handle事件可并发；
-事件在错误handle锁下执行；
-FIFO无法证明。

## 9.2 lock必须与具体事件绑定

正确方式不是先窥视共享队头，而是：

```text
先原子取出具体事件
→按事件handle进入每handle mailbox
```

或直接：

```text
每handle一个串行队列
不同handle之间固定并发
```

---

# 10. P0-06：high-priority disconnect仍会越过receive

PluginEventChannel每次pop：

```text
先high
后normal
```

场景：

```text
normal queue已有receive A
之后high queue进入disconnect
```

下一次worker会先处理disconnect。

插件观察到：

```text
disconnect
→receive A
```

这违反TCP流生命周期顺序。

生命周期事件应受到保护，但不能越过同一连接已经接受的字节事件。

需要按连接保证：

```text
accept/connect
→receive chunk FIFO
→error
→disconnect
```

优先级只能用于不同连接或入口配额，不应改变同连接顺序。

---

# 11. P0-07：`tcp:accept`没有进入当前handle排序

accept payload使用：

```json
{
  "listener_handle": ...,
  "conn_handle": ...
}
```

`peek_handle()`只读取：

```text
payload["handle"]
```

因此accept事件得到：

```text
None
```

不会获取connection handle锁。

后续receive可能由另一个worker并发执行，甚至先于accept callback完成。

修复方式：

-统一所有connection事件使用`handle`；
-或提取handle时兼容`conn_handle`；
-更推荐每连接mailbox统一处理生命周期和数据事件。

---

# 12. P0-08：disconnect前删除handle lock会创建新锁代际

Worker处理disconnect时，在callback之前：

```text
remove_handle_lock(handle)
```

但当前worker仍持有旧lock的Arc guard。

如果队列中还有同handle receive，另一个worker调用：

```text
get_handle_lock(handle)
```

会创建一个新Mutex。

于是：

- disconnect callback在旧锁下执行；
-receive callback在新锁下执行；
-二者可并发。

这进一步破坏同连接顺序。

handle lock只能在：

-该连接mailbox完全drain；
-disconnect callback完成；
-确认不会再入队；

之后清理。

---

# 13. P0-09：HighFrequency Shutdown发送超时后状态不可恢复

`HighFrequencyWriter::shutdown()`：

```text
持有admission gate
→closed.swap(true)
→发送Shutdown control，受timeout限制
```

如果channel满导致send timeout：

```text
函数返回Err
closed仍保持true
Worker没有收到Shutdown
```

后续再次调用shutdown：

```text
closed.swap(true)返回true
→直接返回Ok
```

但Worker实际上仍在运行，也没有执行最终fence。

## 13.1 数据风险

主进程可能认为第二次Shutdown成功并继续退出。

队列中的accepted item未必已经全部提交数据库。

## 13.2 正确状态

应区分：

```text
Open
ShutdownRequested
ShutdownControlSent
Terminated
```

control发送失败时：

-恢复为Open以允许重试；
-或进入明确Fatal并由外部abort worker；
-不能返回后续假成功。

---

# 14. P1：HF send和reply没有共享同一个remaining budget

当前：

```text
timeout(timeout, tx.send())
→成功后再次timeout(timeout, reply)
```

总墙钟时间可能超过调用方预算。

虽然Worker内部持有原始absolute deadline，但channel调度延迟仍可能让调用方等待超过预期。

应使用单个deadline：

```text
remaining1用于send
remaining2用于reply
```

---

# 15. P1：Plugin TCP字节预算仍不是严格原子

normal queue和high queue使用不同Mutex，但共享：

```text
AtomicUsize total_bytes
```

push逻辑是：

```text
load total
→判断预算
→fetch_add
```

high和normal可并发通过同一旧值，最终略超总预算。

建议：

-统一队列状态Mutex；
-或使用CAS预留字节；
-失败时原子回滚。

---

# 16. P1：单条receive事件可能非常大

receive merge可以让一条事件增长到接近4MiB raw bytes。

但payload是：

```text
serde_json::Value数组
```

每个byte不是1字节内存，而是JSON Value/Number对象。

4MiB raw bytes的实际堆内存可能远高于4MiB。

需要：

-单事件上限；
-分块；
-内部使用`Vec<u8>`或二进制表示；
-进入WASM边界时再编码；
-避免巨型JSON数字数组。

---

# 17. P0/P1：精确Auth幂等应是完全no-op

除第5节playtime问题外，精确重放还会：

-重新写`last_seen_at`；
-重新写`updated_at`；
-重新增加IP history `use_count`；
-可能回退`last_connected_at`；
-重写语言、名称和IP。

真正幂等应保证：

```text
重复执行一次
=
数据库状态完全不变
```

访问记录匹配后应直接结束事务。

---

# 18. P1：UserOffline缺少occurred_at

`UserDisconnect`已有`occurred_at`，但`UserOffline`仍没有。

`set_offline()`使用数据库处理时当前时间。

如果Offline已进入WAL但服务器崩溃，重启后才处理：

```text
total_secs += replay_now - session_start
```

服务器停机时间会被计入playtime。

虽然instance heartbeat recovery可以处理没有Offline的旧Session，但这里Offline在Recovery前被重放，因此会绕过stale cleanup的heartbeat截止逻辑。

应让UserOffline携带：

```text
occurred_at
```

并按断线实际时间关闭playtime。

---

# 19. P1：Disconnect部分时间字段仍会回退

`last_disconnected_at`使用`GREATEST`。

但：

```text
last_seen_at = occurred_at
updated_at = occurred_at
```

仍可能被延迟旧Disconnect回退。

应全部使用单调更新或Session代际条件。

---

# 20. P1：Marker degraded没有自恢复清除路径

如果marker更新临时失败：

-设置`DEGRADED_MARKER`；
-未来Admission可能成功修复marker；
-但没有`clear_marker_degraded()`。

服务将持续：

```text
is_healthy=false
AdmissionOutcome::AdmittedDegraded
```

直到重启。

marker成功验证或成功重写后，应只清除marker bit。

---

# 21. P1：marker JSON parse失败被当作无需更新

`mark_marker_active()`：

```text
read marker成功
serde_json parse失败
→直接返回Ok
```

运行期损坏marker没有设置degraded。

应把parse error视为：

```text
DEGRADED_MARKER
```

并返回Err，让Admission继续以AdmittedDegraded语义完成。

---

# 22. 完成度总表

完成度表示距离正式生产闭环的程度，不是代码量比例。

| 大项 | PMP39 | PMP40 | 当前判断 |
|---|---:|---:|---|
|项目架构与能力建设 | 96% | **96%** |主体能力已经完整 |
|客户端协议与房间玩法 | 91% | **91%** |本轮无核心回归 |
|PostgreSQL数据模型 | 92% | **92%** |事务基础完整，精确重放副作用待修 |
|低频持久化可靠性 | 86% | **87%** |marker语义改善，append rollback仍阻断 |
|HighFrequency持久化 | 92% | **91%** |control send有timeout，但Shutdown失败状态有新缺口 |
|故障恢复与重启一致性 | 87% | **88%** |WAL健康bitmask改善 |
|插件宿主与WIT API | 90% | **90%** |主体稳定 |
|Plugin TCP | 66% | **70%** |字节回收修复，FIFO仍未闭环 |
|Real Benchmark | 84% | **84%** |本轮无实质变化 |
|运维、代理与管理接口 | 89% | **90%** |健康原因可枚举 |
|**综合生产完成度** | **约88%** | **约89%** |仍由WAL原子边界、Auth重放和Plugin TCP决定No-Go |

---

# 23. 核心项完成度

| 项目 | 完成度 | 状态 |
|---|---:|---|
|CI、构建与版本一致性 | **100%** |完成 |
|Room Actor核心状态所有权 | **92%** |基本完成 |
|JoinRoom协议顺序 | **92%** |基本完成 |
|RoundCompleted事务 | **93%** |基本完成 |
|UserAuthenticated首次事务 | **93%** |主体完成 |
|UserAuthenticated精确重放 | **55%** |仍有状态副作用 |
|WAL format与迁移 | **94%** |基本完成 |
|WAL ordered execution | **89%** |主体完成 |
|WAL append原子边界 | **58%** |回滚长度与durability仍危险 |
|Admission结果语义 | **88%** |marker失败主路径修复 |
|WAL多原因健康 | **88%** |bitmask完成，marker恢复待补 |
|Flush/Shutdown fence | **92%** |低频主体完成 |
|HighFrequency admission/DB deadline | **93%** |主体完成 |
|HighFrequency Shutdown状态 | **65%** |send timeout后假关闭 |
|Session代际 | **91%** |主体完成 |
|Playtime replay精度 | **76%** |Auth重放和Offline时间仍有问题 |
|Plugin TCP事件数量有界 | **92%** |基本完成 |
|Plugin TCP字节计数 | **88%** |消费回收完成，严格原子预算待补 |
|Plugin TCP同连接FIFO | **45%** |peek-lock-pop方案不成立 |
|Persistent Room recovery | **77%** |可执行但仍偏best-effort |
|DLQ recovery | **83%** |主要链路完整 |

---

# 24. 当前生产阻断项完成度

|生产门禁|完成度|是否阻断|
|---|---:|---|
|精确Auth重放完全no-op | **55%** | **是** |
|WAL original_len安全读取 | **35%** | **是** |
|WAL失败回滚durability | **45%** | **是** |
|corruption后Admission策略 | **55%** | **是** |
|Plugin TCP peek/pop FIFO | **40%** | **是** |
|disconnect不越过receive | **45%** | **是** |
|accept进入connection排序 | **45%** | **是** |
|HF Shutdown send失败状态 | **55%** | **是** |
|Plugin TCP严格原子字节预算 | **78%** | P1/条件阻断 |
|UserOffline原始时间 | **70%** | P1/数据准确性阻断 |
|Marker临时错误自恢复 | **70%** | P1 |
|Disconnect时间单调性 | **82%** | P1 |

---

# 25. PMP40 Core P0任务清单

## P0-A：精确Auth重放完全no-op

- [ ] visit全字段精确匹配后立即返回成功
- [ ] 不再upsert用户
- [ ] 不再更新IP history
- [ ] 不再重写playtime
- [ ] 不再改变时间字段
- [ ] ACK失败→Offline→新Auth→重启测试
- [ ] 精确重放前后数据库全表diff为0
- [ ] login_count保持不变

## P0-B：安全获取WAL回滚点

- [ ] metadata错误立即返回
- [ ]禁止`unwrap_or(0)`
- [ ]打开文件后精确获取原长度
- [ ]原长度不可确认时不得写入
- [ ] metadata故障注入测试
- [ ]已有pending WAL不得被截断

## P0-C：WAL失败回滚durable

- [ ] set_len后sync_all
- [ ]必要时parent fsync
- [ ] rollback失败设置fatal corruption
- [ ] rollback durability不可确认时停止Admission
- [ ] Admission write/flush/sync逐阶段故障测试
- [ ] ACK write/flush/sync逐阶段故障测试
- [ ]失败后重启不出现ghost frame
- [ ]失败后继续append仍可解析

## P0-D：原因化Admission策略

- [ ] ACK degraded允许继续
- [ ] Marker degraded允许AdmittedDegraded
- [ ] Corruption degraded拒绝新Admission
- [ ] Compact fatal拒绝新Admission
- [ ] readiness与Admission策略一致
- [ ]错误原因进入Stats/TUI
- [ ] corruption后继续enqueue测试

## P0-E：Plugin TCP每连接mailbox

- [ ]先原子取出具体事件
- [ ]按handle投递到独立FIFO
- [ ]同handle串行
- [ ]不同handle最多N并发
- [ ]不再使用peek共享队头再加锁
- [ ]多worker stale peek压力测试
- [ ]同连接1000个chunk严格顺序测试

## P0-F：Plugin TCP生命周期顺序

- [ ] accept先于receive
- [ ] receive严格字节流顺序
- [ ] disconnect晚于已接受receive
- [ ] error顺序语义明确
- [ ] `conn_handle`统一映射为handle
- [ ] disconnect完成后再清理mailbox/lock
- [ ] high priority不得越过同handle早期receive
- [ ]不同handle仍允许并发

## P0-G：HighFrequency Shutdown状态机

- [ ] Open/Requested/ControlSent/Terminated
- [ ] control send timeout不得留下假terminated
- [ ]发送失败允许重试或显式abort worker
- [ ]第二次shutdown不能假Ok
- [ ]send timeout后accepted item对账
- [ ]channel full shutdown故障测试
- [ ]Worker task最终退出验证

---

# 26. P1任务清单

## Plugin TCP字节预算

- [ ]CAS或统一Mutex预留预算
- [ ]单事件最大raw bytes
- [ ]单handle pending bytes
- [ ]JSON实际内存风险控制
- [ ]生命周期独立保留预算
- [ ] pending_bytes不变量测试

## Playtime与生命周期

- [ ] UserOffline增加occurred_at
- [ ] set_offline使用原始断线时间
- [ ] last_seen/updated使用GREATEST
- [ ] generation mismatch指标
- [ ] delayed Offline重放不计停机时间

## Marker健康

- [ ]成功修复marker后清除MARKER bit
- [ ] marker parse失败设置degraded
- [ ] marker健康自动重试
- [ ] health reason进入管理接口

## HighFrequency预算

- [ ] control send/reply使用单一absolute deadline
- [ ]发送阶段消耗从reply预算扣除
- [ ]共享shutdown总墙钟测试

## WAL测试

- [ ]真实fault-injection文件层
- [ ]metadata失败
- [ ]partial write
- [ ]flush失败
- [ ]sync失败
- [ ]truncate失败
- [ ]crash-after-rollback

---

# 27. 必须新增的生产门禁测试

## 27.1 Auth ACK失败精确重放

```text
Auth A提交
ACK A失败
Offline A提交
Auth B提交
崩溃重启
```

断言：

```text
Session B保持online
Auth A重放完全no-op
playtime不回退
```

## 27.2 metadata失败

```text
WAL已有多个pending事件
metadata读取失败
尝试新Admission
```

断言既有WAL长度和内容完全不变。

## 27.3 append sync失败

```text
frame写入完成
sync_data失败
回滚
立即崩溃
```

断言重启后该frame不存在，既有frame全部存在。

## 27.4 ACK部分失败

```text
数据库commit
ACK写入中途失败
retry
重启
```

断言WAL可解析且数据库不重复变更。

## 27.5 Plugin TCP stale peek

```text
4个worker
队列：
A1(handle A)
B1(handle B)
A2(handle A)
B2(handle B)
```

随机调度数千次，断言每个handle内部严格FIFO。

## 27.6 disconnect顺序

```text
receive A已入normal
disconnect后入high
```

断言插件仍观察：

```text
receive A
disconnect
```

## 27.7 accept顺序

```text
accept callback阻塞
远端立即发送receive
```

断言receive不能先于accept完成。

## 27.8 HF Shutdown send timeout

```text
HF channel占满
Shutdown control发送超时
再次调用Shutdown
```

断言不会假成功，Worker最终明确退出或允许可靠重试。

---

# 28. Go / No-Go上线门槛

PMP只有满足以下条件才能进入Production Candidate。

##数据库幂等

- [ ]精确Auth重放完全不改变数据库
- [ ]旧Auth不能覆盖新Session
- [ ]Offline使用原始断线时间
- [ ]访问、在线、playtime可完整对账

## WAL原子性

- [ ]回滚点必须可靠读取
- [ ]失败回滚必须durable
- [ ]无法回滚时停止新Admission
- [ ]ACK部分失败不会损坏WAL
- [ ]业务返回值与WAL事实严格一致

## Plugin TCP

- [ ]同handle严格FIFO
- [ ]accept先于receive
- [ ]disconnect晚于receive
- [ ]不同handle有界并发
- [ ]事件数、raw bytes和实际内存都受控

## HighFrequency

- [ ]Shutdown control发送失败不产生假关闭
- [ ]accepted item都有明确terminal
- [ ]共享deadline覆盖send、处理和reply
- [ ]Worker task退出可验证

---

# 29. 最终判断

PMP40已经关闭PMP39报告中的多个核心问题：

```text
marker失败后的普通业务回滚
WAL degraded原因覆盖
Plugin TCP pending bytes不回收
receive merge无预算
认证冲突字段不完整
marker原子替换基础
```

但进一步检查发现：

-精确幂等Auth重放仍会重写旧Session；
-WAL回滚点读取使用危险默认0；
-truncate回滚没有durable确认；
-corruption后仍允许继续Admission；
-Plugin TCP peek-lock-pop不能建立真正FIFO；
-HighFrequency Shutdown发送失败后状态不可恢复。

这些问题仍位于数据一致性和正式网络能力的最底层边界。

最终结论：

> **PMP `(40)`：NO-GO，继续作为 Development Preview。**

当前综合生产完成度约为：

> **89%**

下一轮建议只验收四个闭环：

```text
Auth精确重放完全no-op
→ WAL append/ACK失败确认与durable回滚
→ Plugin TCP每连接mailbox FIFO
→ HighFrequency Shutdown状态机
```

完成这些定向故障测试后，PMP才适合重新评估Production Candidate。
