# PMP42 官方 Phira 客户端兼容性与生产审计报告（修正版）

**PMP 版本：** `0.5.1986`  
**审计对象：** 当前 PMP、官方 `phira-mp`、官方 Phira 客户端源码、gooophira 参考实现  
**CI 状态：** **已通过**。按用户确认，上传的 PMP 项目已通过既有 check、tests、Clippy 与 release build CI 门禁  
**硬性边界：**

```text
官方 Phira 客户端不可修改
客户端协议不可由 PMP 单方面升级
PMP 必须兼容官方客户端
PMP 的可观察行为应向官方 phira-mp 看齐
```

**最终结论：** **NO-GO。当前生产阻断集中在 PMP 对官方客户端的兼容时序与响应契约。**

---

# 1. 对上一版报告的纠正

上一版报告中以下建议全部撤回：

- 修改客户端回调注册顺序；
- 修改客户端网络任务；
- 修改客户端 UI；
- 给协议增加 request ID；
- 给协议增加 room revision；
- 增加客户端主动 resync 命令；
- 将重复 Join 改为服务器幂等成功；
- 改变官方 Ready、RequestStart、JoinRoom 的包顺序；
- 要求客户端配合 PMP 的扩展协议。

这些都不属于 PMP 可以要求的前提。

从本版开始，审计采用如下唯一标准：

> **官方客户端是不可变的兼容目标。客户端即使存在脆弱行为，PMP也必须在服务器侧规避、补偿和兼容。**

PMP不能以“客户端实现有问题”为理由把修复责任转移给客户端。

---

# 2. 当前三个现象仍然成立，但责任边界改为PMP兼容问题

用户报告：

1. `deadline has elapsed`；
2. JoinRoom显示超时，之后提示玩家已经在房间；
3.点击Ready后客户端卡住。

源码仍表明官方客户端具有以下既定行为：

```text
固定等待约7秒
按命令类型使用单个响应回调槽
发送命令后才安装回调
响应槽缺失时可能panic
网络任务死亡后传播能力有限
```

但客户端不可修改，因此这些行为必须视为PMP的兼容约束。

PMP需要保证：

-响应不能快到早于客户端回调安装；
-响应不能慢于客户端固定deadline；
-每个请求型命令都必须获得对应响应；
-不能在客户端已经超时后继续提交命令；
-不能发送官方客户端尚未准备处理的状态修正包；
-重连认证必须恢复与官方服务端一致的房间状态；
-不能依赖新增协议字段或客户端ACK。

---

# 3. 官方 phira-mp 是兼容行为基线

审计不能只看协议枚举相同，还必须比较官方服务端的**可观察行为**：

-响应类型；
-错误字符串类别；
-包顺序；
-状态提交顺序；
-断线保留时间；
-重连认证内容；
-房间事件顺序；
-自然处理延迟；
-同一请求只返回一次；
-哪些命令没有响应。

## 3.1 官方 JoinRoom 顺序

官方 `phira-mp` 的实际顺序为：

```text
检查当前是否已有房间
→找到房间
→检查锁定、状态、观战权限和容量
→add_user
→设置monitor/live
→广播OnJoinRoom
→发送Message::JoinRoom
→写入user.room
→构造JoinRoomResponse
→返回ServerCommand::JoinRoom
```

因此PMP不能擅自改成：

```text
先JoinRoom响应
→再发送官方原本位于响应前的房间消息
```

正式兼容应以官方顺序为准。

## 3.2 官方 RequestStart 顺序

```text
检查房间、房主和谱面
→reset game time
→发送Message::GameStart
→切换WaitForReady
→on_state_change
→check_all_ready
→返回RequestStart结果
```

## 3.3 官方 Ready 顺序

```text
写入started集合
→发送Message::Ready
→check_all_ready
→返回Ready结果
```

## 3.4 官方 CancelReady 顺序

```text
从started移除
→房主时CancelGame并切回SelectChart
  或普通用户发送CancelReady
→返回CancelReady结果
```

PMP必须复现这些可观察顺序，而不是按理想化协议重新设计。

---

# 4. 重要纠正：不能把重复Join改成成功

官方服务端在用户已经有房间时返回：

```text
already in room
```

因此PMP不应为了隐藏超时问题，把同房间重复Join改成成功。

这会造成：

-错误语义偏离官方；
-第三方客户端或工具看到不同结果；
-服务器端真实状态错误被掩盖；
-兼容性变成PMP私有行为。

正确修复不是改变第二次Join的结果，而是保证第一次Join：

-在客户端deadline内得到响应；
-响应不会早于客户端准备；
-超时后重连认证能恢复官方房间状态；
-旧Session不会误触发房间清理或重复加入。

---

# 5. P0：PMP的静默限流会直接制造客户端deadline

当前PMP对以下命令增加限流：

- Chat；
- CreateRoom；
- JoinRoom；
- SelectChart。

限流失败时：

```rust
warn!("command rate limited")
return;
```

不会返回相应的：

- `ServerCommand::Chat(Err(...))`
- `CreateRoom(Err(...))`
- `JoinRoom(Err(...))`
- `SelectChart(Err(...))`

官方客户端会等待约7秒，之后显示timeout。

官方服务端没有这条“静默吞掉请求”的可观察行为。

## 修复要求

PMP可以保留限流能力，但对客户端必须返回原命令对应的官方Result包：

```text
Chat → Chat(Err)
CreateRoom → CreateRoom(Err)
JoinRoom → JoinRoom(Err)
SelectChart → SelectChart(Err)
```

错误文本可以本地化，但不能无响应。

---

# 6. P0：权限拒绝和部分错误路径静默返回None

PMP在SessionCategory权限不匹配时：

```rust
return None;
```

无room、QueryRoomInfo失败以及部分Actor异常路径也可能返回`None`。

`None`同时承担：

-无需响应的Touches/Judges；
-请求已在内部发送响应；
-权限拒绝；
-内部错误；
-无房间；
-mailbox错误。

这对官方客户端不可接受，因为请求型命令等待的是固定ServerCommand变体。

## 修复要求

内部可以继续使用扩展SessionCategory，但普通官方客户端的每个请求型命令必须获得对应响应。

应建立服务器内部映射：

```rust
fn official_error_response(
    cmd: &ClientCommand,
    error: String,
) -> Option<ServerCommand>
```

Touches和Judges可以无响应。

其他官方请求命令不得因PMP内部扩展而静默。

---

# 7. P0：PMP SessionActor等待预算远大于官方客户端deadline

当前PMP大致存在：

```text
向SessionActor mailbox发送：最长约30秒
等待Actor响应：最长约30秒
```

官方客户端约7秒后就会判定timeout。

因此可能发生：

```text
客户端7秒显示deadline has elapsed
→PMP Actor仍在等待
→稍后才修改房间状态
→迟到发送响应
```

这会直接产生“客户端认为失败、服务器以后成功”。

## 修复要求

PMP普通客户端命令的完整服务器预算必须小于客户端deadline。

建议兼容预算：

```text
总业务处理deadline：4至5秒
响应排队和socket写预算：最多1秒
必须在客户端约7秒前完成明确结果
```

Actor命令元数据应携带绝对deadline。

在真正修改状态前再次检查：

```text
deadline已过
→不得提交
→返回对应命令Err
```

不能只让Session调用方超时，而Actor继续执行。

---

# 8. P0：PMP响应过快也会放大官方客户端竞态

官方客户端是既定不可变实现。

其请求函数存在：

```text
先send
→后安装callback
```

因此PMP不能在收到请求后立即以极低延迟返回响应。

服务器越快，响应越可能早于客户端安装回调。

PMP的Actor、内存房间状态和本地部署可能比官方服务端路径更快，从而更频繁触发问题。

## 服务器侧兼容方案

增加官方客户端兼容响应下限：

```yaml
compatibility:
  official_phira_client: true
  minimum_response_latency_ms: 10
```

推荐初始值：

```text
10ms
```

原因：

- gooophira也明确实现了ProtocolHack；
-其注释说明原实现存在约2ms延迟语义；
-Go实现选择默认10ms作为保守客户端补偿；
-10ms通常低于用户可感知范围；
-足以让客户端发送函数安装callback。

这不是改变协议，只是模拟官方服务端及参考实现的调度时序。

## 适用命令

所有具有客户端callback的官方命令：

- Authenticate；
- Chat；
- CreateRoom；
- JoinRoom；
- LeaveRoom；
- LockRoom；
- CycleRoom；
- SelectChart；
- RequestStart；
- Ready；
-CancelReady；
-Played；
-Abort。

## 重要限制

不能无条件sleep在Actor锁内。

正确做法：

```text
记录请求接收时间
→按官方顺序完成状态操作
→发送响应前等待至minimum_response_latency
→发送并flush响应
```

兼容延迟必须可配置、可测试、可观测。

---

# 9. P0：响应必须进入socket，而不仅是服务器mpsc

PMP多数响应当前通过：

```rust
send_tx.send(response)
```

成功只表示：

```text
放入服务器内部发送队列
```

不表示：

```text
已写入socket
```

对于官方客户端这种单回调、固定deadline模型，关键请求响应必须更强。

PMP已经有：

```rust
send_and_flush()
```

应对官方请求型响应使用：

```text
minimum response latency
→send_and_flush(response)
```

## 关键命令

优先覆盖：

- Authenticate；
- CreateRoom；
- JoinRoom；
- RequestStart；
- Ready；
-CancelReady；
-LeaveRoom。

这不会改变包格式或官方顺序，只提高响应到达确定性。

---

# 10. P0：PMP必须严格复现官方包顺序

上一版报告曾建议“Ready Ack先于广播”，该建议撤回。

官方顺序是：

```text
Ready消息/状态变化
→Ready结果响应
```

PMP应保持官方顺序。

真正需要解决的是：

-响应不能早于callback安装；
-响应不能晚于7秒；
-官方顺序中的每个包都不能被PMP扩展包插队；
-加入房间后，不能提前发送官方服务端不会发送的ChangeHost/ChangeState补偿；
-扩展事件必须延后到客户端处理完官方响应之后。

## 兼容屏障

对于PMP额外产生、但官方服务端原路径没有立即发送的状态修正消息：

```text
先完成官方包序列
→flush官方响应
→等待protocol_hack_delay
→发送PMP补偿消息
```

gooophira默认使用10ms补偿延迟。

PMP不能让插件、Persistent Room恢复、回放模拟或额外状态同步包插入官方核心序列中间。

---

# 11. P0：JoinRoom超时后显示already-in-room的PMP侧解释

完整链路应按以下方式理解：

```text
PMP已add_user并写入user.room
→JoinRoom响应未在客户端deadline内完成
  或响应过早触发客户端竞态
→客户端显示timeout
→服务器仍保持用户在房间
→客户端或用户再次发JoinRoom
→PMP按官方语义返回already in room
```

第二次错误本身符合官方。

真正的PMP问题是第一次命令没有可靠闭环。

## PMP修复重点

-Join完整流程必须在5秒内完成；
-Join响应遵守minimum response latency；
-Join响应使用send_and_flush；
-PMP扩展状态包不能插入官方Join序列；
-Actor超时后不能继续完成Join；
-连接断开时Session generation必须防止旧Session清理新Session；
-重连Authenticate必须返回官方格式的room state；
-重连认证响应完成前不得发送PMP扩展修正消息。

---

# 12. P0：Ready卡死的PMP侧处理

官方Ready顺序不能改变：

```text
写ready
→发送Message::Ready
→check_all_ready
→返回Ready响应
```

PMP需要保证：

1. Ready处理总时长小于客户端deadline；
2.返回Ready响应前达到minimum response latency；
3.Ready响应必须进入socket；
4.Plugin、数据库和事件总线不能阻塞客户端响应；
5.PMP额外事件不能插入官方Ready核心序列；
6.Actor超时后不能稍后再把用户设为Ready；
7.重复点击时必须返回官方错误，而不能静默；
8.任何限流或权限错误必须返回Ready对应结果。

## 关键隔离

Ready的客户端响应不能等待：

-普通审计事件；
-插件事件处理完成；
-非关键数据库遥测；
-慢订阅者；
-HighFrequency写入。

关键房间状态可以在Actor内完成，但旁路工作必须异步有界。

---

# 13. P0：官方重连行为必须作为唯一恢复方案

官方服务端在重新Authenticate时可以返回：

```text
UserInfo
Option<ClientRoomState>
```

PMP已经具备类似基础。

由于客户端不能增加新命令，PMP的状态收敛必须完全依靠官方重连认证。

## PMP必须保证

-同一用户新Session替换旧Session；
-旧Session之后的disconnect不会清理新Session；
-重连取消dangle timer；
-房间成员关系保持官方语义；
-认证响应中的room state完整、准确；
-认证响应先完成；
-PMP额外ChangeState/ChangeHost修正延迟发送；
-Playing重连行为按官方客户端能处理的方式返回；
-不要求客户端发送QueryRoomInfo等PMP扩展命令。

---

# 14. P0：PMP的Join快照必须与官方字段语义一致

官方 `JoinRoomResponse` 只包含：

```text
state
users
live
```

官方客户端会把：

```text
locked=false
cycle=false
is_host=false
is_ready=false
```

作为Join后的初始本地值。

PMP不能擅自扩展官方JoinRoomResponse字段，否则破坏线协议。

但PMP内部可能存在：

-锁房间；
-cycle；
-Persistent Room；
-重连Ready；
-额外host状态。

## 兼容处理

必须按官方惯例使用后续官方ServerCommand补偿：

- `LockRoom`
- `CycleRoom`
- `ChangeHost`
- `ChangeState`

并遵循：

```text
JoinRoom官方响应完成并flush
→protocol_hack_delay
→按固定顺序发送必要补偿
```

不能在Join响应前发送会让客户端访问尚未安装room状态的命令。

---

# 15. P0：PMP扩展命令不得影响普通客户端枚举兼容

PMP追加了：

- ConsoleAuthenticate；
- RoomMonitorAuthenticate；
- GameMonitorAuthenticate；
- QueryRoomInfo；
-管理ServerCommand。

基础枚举顺序当前总体保持。

但必须建立门禁：

-所有官方ClientCommand discriminant完全一致；
-所有官方ServerCommand discriminant完全一致；
-官方字段编码完全一致；
-PMP扩展只能追加，不能插入；
-普通客户端Session不能收到管理扩展包；
-官方客户端未知包不能被PMP主动发送。

---

# 16. 官方行为对齐矩阵

| 命令 | 官方核心顺序 | PMP要求 |
|---|---|---|
| Authenticate | 认证/恢复→Authenticate响应 | 完全一致；响应延迟下限+flush |
| CreateRoom | 建房/设room→CreateRoom响应 | 不提前发送PMP扩展房间包 |
| JoinRoom | 加成员→OnJoinRoom→Join消息→Join响应 | 完全一致；响应flush；扩展补偿延后 |
| LeaveRoom | 清房间→广播→Leave响应 | 维持官方顺序 |
| RequestStart | GameStart→状态变化→检查→响应 | 完全一致，不改成响应优先 |
| Ready | Ready消息→检查全部Ready→响应 | 完全一致；不能被旁路阻塞 |
| CancelReady | Cancel消息/状态→响应 | 完全一致 |
| SelectChart | 选谱消息/状态→响应 | 完全一致 |
| Played | 记录/状态→响应 | 不能因数据库旁路超过客户端deadline |
| Abort | 中止状态→响应 | 完全一致 |
| Touches/Judges | 无响应 | 保持无响应 |

---

# 17. PMP必须新增的兼容层

建议增加独立模块：

```text
official_client_compat/
  timing.rs
  response.rs
  protocol_trace.rs
  post_response.rs
  command_contract.rs
```

## 17.1 Timing

负责：

-最低响应延迟；
-总命令deadline；
-响应写入deadline；
-补偿消息延迟。

## 17.2 Response

负责：

-每个请求型命令的对应响应；
-限流错误；
-权限错误；
-mailbox错误；
-shutdown错误；
-禁止静默None。

## 17.3 Protocol trace

记录：

```text
request received
official events emitted
response queued
response flushed
compat messages emitted
```

便于与官方服务端抓包逐项比较。

## 17.4 Post-response

只处理PMP扩展补偿：

- host修正；
-lock/cycle修正；
-Persistent Room额外状态；
-回放模拟；
-扩展监控状态。

不能介入官方核心序列。

---

# 18. 必须建立“官方服务端差分测试”

不能继续只用PMP内部Benchmark客户端验证。

应对同一组官方ClientCommand分别连接：

```text
官方phira-mp
PMP
```

捕获：

-ServerCommand序列；
-Message序列；
-错误结果；
-连接关闭行为；
-响应耗时；
-断线保留时间；
-重连Authenticate内容。

## 测试命令

- Authenticate；
-CreateRoom；
-JoinRoom；
-LeaveRoom；
-LockRoom；
-CycleRoom；
-SelectChart；
-RequestStart；
-Ready；
-CancelReady；
-Played；
-Abort；
-重复命令；
-错误状态命令；
-断线重连。

## 通过标准

除明确记录的PMP扩展外：

```text
包类型顺序一致
官方字段一致
错误类别一致
无PMP静默请求
PMP响应均早于客户端deadline
扩展包不插入官方核心序列
```

---

# 19. 必须使用未修改官方客户端做端到端测试

测试客户端必须直接使用用户提供的官方Phira源码和实际依赖版本。

不能：

-修补callback；
-增加request ID；
-改UI；
-改timeout；
-增加resync；
-替换为PMP内部Benchmark客户端。

## 必测场景

### 19.1 快速服务器

PMP在本机、低负载下快速处理。

验证10ms兼容响应下限是否能消除：

- callback竞态；
- `deadline has elapsed`；
-Ready卡死。

### 19.2 Join

连续执行：

```text
登录
Join
Leave
Join
断线重连
```

验证：

-首次Join不timeout；
-正常重试语义与官方一致；
-重连Authenticate恢复房间；
-不会错误出现already-in-room。

### 19.3 Ready

多人同时Ready，验证：

-官方消息顺序；
-客户端不冻结；
-全部客户端进入Playing；
-没有7秒timeout。

### 19.4 限流

快速触发PMP限流。

验证客户端收到明确错误，不显示deadline。

### 19.5 Actor拥塞

人为堵塞Room Actor。

验证：

```text
5秒内明确Err
Actor之后不再提交
```

### 19.6 扩展消息隔离

开启：

-插件；
-Persistent Room；
-回放；
-管理接口；
-额外房间状态。

验证官方客户端看到的核心包序列仍与官方服务端一致。

---

# 20. PMP42其他审计项继续保留

客户端兼容问题不能替代服务器内部审计。

## 20.1 WAL

PMP42已经增加：

-AppendOutcome分类；
-完整frame确认；
-durable rollback；
-FatalUnknown；
-fatal状态禁止Admission和ACK；
-marker repair；
-compact错误标记；
-WAL故障测试基础。

当前完成度：

| 项目 | 完成度 |
|---|---:|
|WAL格式与迁移|95%|
|Admission线性化|94%|
|失败分类与rollback|92%|
|fatal状态锁定|93%|
|marker完整性|91%|
|真实文件系统故障注入|80%|

仍需真实I/O故障门禁。

## 20.2 HighFrequency

当前完成度：

| 项目 | 完成度 |
|---|---:|
|Admission/Shutdown线性化|94%|
|DB deadline|94%|
|terminal对账|94%|
|退出状态|95%|
|真实数据库故障测试|82%|

## 20.3 Plugin TCP

PMP42已经转向：

```text
每连接mailbox
同连接FIFO
不同连接有界并发
```

并增加字节预算和生命周期保护。

当前完成度：

| 项目 | 完成度 |
|---|---:|
|每连接FIFO|91%|
|事件数量有界|93%|
|字节预算|91%|
|生命周期可靠性|89%|
|真实慢插件压力测试|80%|

## 20.4 Room Actor与数据库

主体稳定，但必须增加新的端到端不变量：

```text
Room Actor权威状态
=
PMP发送给官方客户端的最终状态
```

服务器内部正确但客户端不可观察到，不应视为生产完成。

---

# 21. 修正后的完成度表

完成度表示距离PMP正式生产闭环的程度。

| 大项 | 完成度 | 判断 |
|---|---:|---|
|PMP服务器架构与功能|96%|主体完整|
|Room Actor内部一致性|93%|基本完成|
|PostgreSQL与低频持久化|93%|基本完成|
|HighFrequency|94%|主体完成|
|Plugin TCP|90%|主体完成|
|官方线协议二进制兼容|86%|需golden packet门禁|
|官方命令包顺序兼容|70%|需差分测试和补偿隔离|
|所有请求必响应|55%|静默路径仍多|
|官方客户端时序兼容|45%|缺响应下限和完整barrier|
|Actor deadline兼容|35%|当前最长明显超过客户端|
|Join端到端可靠性|55%|首请求闭环不足|
|Ready端到端可靠性|48%|仍可能timeout/冻结|
|官方重连兼容|68%|有快照基础，需严格顺序验证|
|真实官方客户端测试|35%|当前Benchmark不能替代|
|**PMP综合生产完成度**|**约76%**|内部成熟，客户端兼容成为主要短板|

该完成度不再把“修改客户端”计入PMP任务。

---

# 22. 当前PMP生产阻断项

| PMP阻断项 | 完成度 | 是否阻断 |
|---|---:|---|
|限流必须返回官方响应|35%|**是**|
|权限拒绝必须返回官方响应|30%|**是**|
|请求命令禁止模糊None|45%|**是**|
|总Actor deadline小于客户端deadline|35%|**是**|
|响应最低延迟兼容callback|20%|**是**|
|关键响应socket flush|55%|**是**|
|官方包序列差分验证|40%|**是**|
|PMP扩展包不得插队|50%|**是**|
|Join首次请求可靠闭环|55%|**是**|
|Ready首次请求可靠闭环|45%|**是**|
|官方重连快照顺序|68%|**是/条件阻断**|
|真实官方客户端测试|35%|**是**|
|WAL/HF内部可靠性|93%|继续故障测试|
|Plugin TCP|90%|继续压力测试|

---

# 23. PMP侧P0任务清单

## P0-A：官方请求完整响应

- [ ]为所有官方请求型ClientCommand建立响应映射；
- [ ]限流返回对应ServerCommand Err；
- [ ]权限拒绝返回对应ServerCommand Err；
- [ ]无room返回对应ServerCommand Err；
- [ ]mailbox满、关闭和timeout返回对应Err；
- [ ]服务器关闭中返回对应Err；
- [ ]Touches/Judges明确标记为NoResponseExpected；
- [ ]请求型命令禁止返回模糊None。

## P0-B：官方客户端时间兼容

- [ ]配置`minimum_response_latency_ms`；
- [ ]默认从10ms开始；
- [ ]响应时间从收到命令开始计算；
- [ ]不得在锁内sleep；
- [ ]关键响应发送前等待至下限；
- [ ]所有响应必须早于官方客户端deadline；
- [ ]记录response latency histogram；
- [ ]用未修改客户端验证。

## P0-C：Actor deadline

- [ ]普通客户端命令总deadline设置为4至5秒；
- [ ]mailbox发送和reply使用同一deadline；
- [ ]Actor执行前检查deadline；
- [ ]状态提交前再次检查deadline；
- [ ]过期命令不得继续提交；
- [ ]返回官方命令对应Err；
- [ ]迟到Actor故障测试。

## P0-D：官方序列对齐

- [ ]Join顺序与官方逐包一致；
- [ ]RequestStart顺序与官方一致；
- [ ]Ready顺序与官方一致；
- [ ]CancelReady顺序与官方一致；
- [ ]SelectChart顺序与官方一致；
- [ ]PMP扩展包全部移至官方序列之后；
- [ ]不得把响应提前到官方事件之前；
- [ ]建立抓包golden trace。

## P0-E：关键响应可靠写入

- [ ]Authenticate使用兼容延迟后send_and_flush；
- [ ]CreateRoom响应send_and_flush；
- [ ]JoinRoom响应send_and_flush；
- [ ]RequestStart响应send_and_flush；
- [ ]Ready响应send_and_flush；
- [ ]CancelReady响应send_and_flush；
- [ ]LeaveRoom响应send_and_flush；
- [ ]flush失败立即关闭Session并进入官方断线处理。

## P0-F：Join与重连

- [ ]保留官方already-in-room语义；
- [ ]修复第一次Join的响应闭环；
- [ ]Actor timeout后不得迟到加入；
- [ ]重连Authenticate返回准确room state；
- [ ]认证响应前不发送PMP扩展状态包；
- [ ]旧Session不得清理新Session；
- [ ]dangle和官方10秒语义做差分验证；
- [ ]未修改客户端Join/重连压力测试。

## P0-G：Ready与RequestStart

- [ ]保持官方事件→响应顺序；
- [ ]响应不等待插件和非关键持久化；
- [ ]每个错误路径都返回Ready/RequestStart Err；
- [ ]Actor过期不得稍后写Ready；
- [ ]PMP扩展事件不得插队；
- [ ]多人同时Ready官方客户端测试；
- [ ]Ready响应丢失模拟不能靠改变客户端修复；
- [ ]通过服务器时序和断线恢复收敛。

---

# 24. P1任务清单

## 协议兼容门禁

- [ ]官方ClientCommand discriminant golden test；
- [ ]官方ServerCommand discriminant golden test；
- [ ]官方字段编码golden test；
- [ ]旧客户端解码PMP基础包；
- [ ]PMP解码官方客户端基础包；
- [ ]扩展枚举只能追加；
- [ ]普通Session禁止收到扩展管理包。

## ProtocolHack

- [ ]集中管理补偿消息；
- [ ]默认延迟10ms；
- [ ]按官方响应flush后调度；
- [ ]不同补偿消息固定顺序；
- [ ]不可阻塞Room Actor；
- [ ]可配置为0以做差分测试；
- [ ]记录每条补偿原因。

## 可观测性

- [ ]request received时间；
- [ ]Actor开始和结束时间；
- [ ]响应入队时间；
- [ ]响应flush时间；
- [ ]客户端deadline前剩余预算；
- [ ]静默响应路径计数必须为0；
- [ ]late commit计数必须为0；
- [ ]官方序列偏差计数。

---

# 25. 必须新增的生产门禁测试

## 25.1 官方客户端快速响应

```text
未修改官方客户端
PMP本机运行
服务器极低负载
```

断言所有请求无`deadline has elapsed`和接收任务panic。

## 25.2 响应下限

分别测试：

```text
0ms
2ms
5ms
10ms
20ms
```

确定官方客户端稳定所需的最低兼容值。

生产默认值必须来自测试，而不是猜测。

## 25.3 Join官方差分

同一操作分别连接官方服务端和PMP，比较：

```text
OnJoinRoom
Message::JoinRoom
JoinRoom response
```

类型和顺序必须一致。

## 25.4 Ready官方差分

比较：

```text
Message::Ready
ChangeState/StartPlaying
Ready response
```

不得由PMP重新排序。

## 25.5 限流

触发PMP限流，客户端必须收到明确Err，不能等待7秒。

## 25.6 Actor过期

让Room Actor超过5秒。

断言：

-客户端在7秒前收到Err；
-Actor后续不修改状态。

## 25.7 Join响应写失败

在房间已提交后模拟socket write失败。

断言Session立即断开，重连Authenticate返回官方兼容房间状态。

## 25.8 扩展包隔离

打开PMP所有扩展功能，验证官方核心包序列不受影响。

## 25.9 官方客户端长时间压力

至少测试：

-100个客户端；
-反复Join/Leave；
-多人Ready；
-断线重连；
-弱网；
-发送队列拥塞；
-持续1小时。

目标：

```text
deadline has elapsed = 0
Ready卡死 = 0
首次Join超时 = 0
静默请求 = 0
Actor迟到提交 = 0
```

---

# 26. Go / No-Go门槛

PMP只有满足以下条件，才能重新评估Production Candidate。

## 官方行为

- [ ]所有核心命令包序列与官方一致；
- [ ]错误类型和响应类型一致；
- [ ]already-in-room等官方语义不被私自修改；
- [ ]扩展包不插入官方核心序列。

## 客户端兼容

- [ ]未修改官方客户端无callback竞态表现；
- [ ]无`deadline has elapsed`；
- [ ]Join首次响应稳定；
- [ ]Ready无卡死；
- [ ]重连Authenticate状态准确；
- [ ]无需客户端新增命令或字段。

## PMP响应

- [ ]所有请求型命令必有响应；
- [ ]限流和权限错误不静默；
- [ ]Actor总deadline小于客户端deadline；
- [ ]过期命令绝不迟到提交；
- [ ]关键响应完成socket flush。

## 测试

- [ ]官方服务端差分测试；
- [ ]官方客户端真实端到端测试；
- [ ]弱网和拥塞测试；
- [ ]扩展功能隔离测试；
- [ ]连续运行稳定性测试。

---

# 27. 最终判断

本次纠正后的结论是：

> **客户端不可修改，PMP必须承担全部兼容责任。**

客户端源码中观察到的时序脆弱点，只能用于帮助PMP设计兼容层，不能转化为客户端修改任务。

PMP需要做的不是重新设计协议，而是：

```text
严格复现官方phira-mp可观察行为
→补齐PMP新增的静默错误路径
→把Actor预算压到客户端deadline以内
→模拟官方服务端自然时序
→对关键响应执行可靠flush
→把PMP扩展消息放到官方序列之后
→用未修改官方客户端和官方服务端做差分测试
```

当前PMP服务器内部完成度较高，但官方客户端兼容层尚未完成。

最终判定：

> **PMP42：NO-GO。当前最大阻断是PMP没有完整复现官方phira-mp的响应时序、错误响应和客户端兼容行为。**

当前PMP综合生产完成度约为：

> **76%**

下一轮只需要验收四个服务器侧闭环：

```text
所有请求必响应
→Actor deadline小于客户端deadline
→官方包序列逐命令对齐
→未修改官方客户端端到端零超时
```
