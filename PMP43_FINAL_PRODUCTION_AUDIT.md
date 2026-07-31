# PMP43 官方 Phira 客户端兼容性与生产就绪审计报告

**PMP版本：** `0.5.1991`  
**Plugin SDK版本：** `0.5.1991`  
**审计对象：** 本轮上传PMP源码  
**对比基线：** PMP42 / `0.5.1986`  
**参考实现：**

- 官方 `phira-mp` 服务端；
-未修改的官方Phira客户端及其实际依赖；
- gooophira参考服务端；
- PMP42之前的持久化、Session、HighFrequency、Plugin TCP和恢复链路审计结果。

**硬性兼容边界：**

```text
官方Phira客户端不可修改
PMP必须单方面兼容官方客户端
PMP必须尽量复现官方phira-mp的可观察行为
不得以修改客户端、扩展客户端协议或要求客户端配合为修复前提
```

**CI状态：** **已通过**。按用户确认，本次上传项目已经通过既有的check、tests、Clippy和release build CI门禁。  
**静态校验：** 11个TOML、6个YML、2个JSON均可解析；29个Markdown文件、32个本地链接未发现失效引用。  
**代码规模：** Server `src`目录148个Rust文件，约51,045行。  
**相对PMP42：** 排除随包附带的旧审计文档后，25个代码、配置、测试或依赖文件发生变化，约新增1,641行、删除171行。  
**最终结论：** **NO-GO，继续作为Development Preview。**

---

# 1. 执行摘要

PMP43不是功能扩张版本，而是首次系统增加官方客户端兼容层的版本。

新增的主要模块包括：

```text
official_client_compat/
├── mod.rs
├── timing.rs
├── response.rs
├── protocol_trace.rs
└── post_response.rs
```

本轮已经取得的真实进展包括：

-为官方客户端命令增加最低响应延迟基础；
-增加默认4.5秒Session命令deadline；
-限流不再简单静默吞掉部分命令；
-权限拒绝开始映射为官方命令对应错误响应；
-明确区分需要响应与无需响应的命令；
-对Authenticate、CreateRoom、JoinRoom、RequestStart、Ready、CancelReady、LeaveRoom等关键响应引入`send_and_flush`基础；
-将deadline向部分Room Actor命令传播；
-Join、Ready、RequestStart等处理顺序开始按官方服务端重新对齐；
-增加协议追踪计数和基础枚举兼容测试；
-增加PMP扩展补偿消息的延迟发送模块。

这说明PMP43已经开始正面解决：

```text
deadline has elapsed
Join超时后already-in-room
Ready点击后客户端卡住
```

等官方客户端兼容问题。

但是，当前兼容层仍停留在“入口补丁和局部时序修复”阶段，尚未形成端到端命令闭环。

最严重的剩余问题是：

```text
旧连接发起Join/Ready
→命令进入Session Actor或Room Actor
→用户建立新连接并替换user.session
→旧命令稍后完成
→响应和兼容补偿通过user.session发送
→实际被发到新连接
```

官方客户端的新连接可能：

-没有对应请求回调；
-尚未安装房间状态；
-正在等待Authenticate响应；
-已经处于另一房间状态。

迟到响应或补偿包可能再次触发：

-回调缺失；
-房间状态`unwrap()`；
-连接任务异常；
-错误状态覆盖；
-新的`deadline has elapsed`。

此外，PMP43的4.5秒外层deadline没有成为所有状态提交、响应发送和回滚动作共同使用的绝对deadline。

典型Join路径仍然是：

```text
外层Session命令deadline：4.5秒
Join内部响应flush timeout：5秒
```

因此可能发生：

```text
外层4.5秒先超时并关闭连接
→Join内部仍继续发送、等待、回滚
→服务器状态和客户端结果再次分叉
```

多个命令虽然在Session Actor入口检查deadline，但进入Room Actor或外部API之后，仍可能在deadline之后继续提交状态。

因此，PMP43尚未解决最关键的不变量：

> 一个官方客户端命令必须绑定发起它的具体连接，在官方客户端deadline之前，只能产生一次确定结果；超时、重连或连接代际变化后，旧命令不得继续提交，也不得向新连接发送任何响应或补偿消息。

---

# 2. 用户报告现象与PMP43剩余路径对应

| 用户现象 | PMP43中仍可能触发的路径 | 风险等级 |
|---|---|---:|
| `deadline has elapsed` |初始Authenticate未应用最低响应延迟；关键响应发送无统一remaining deadline；Actor或外部API迟于4.5秒；发送错误前先关闭连接 | **P0** |
| Join显示超时，重试提示already-in-room | Join在外层deadline后仍可能继续；响应flush内部使用独立5秒；状态已提交但响应未可靠闭环 | **P0** |
| Ready点击后客户端卡住 | Ready或check_all_ready被round持久化、插件或Actor延迟；旧命令响应可能发到新Session；全员Ready后round open失败仍返回Ok | **P0** |
| 重连后收到异常状态包 |新Session在Authenticate响应flush前已经替换`user.session`并可接收房间广播 | **P0** |
| 偶发错误响应与当前操作不对应 |命令不携带originating session ID，迟到响应通过当前`user.session`发送 | **P0** |
| 加入Persistent Room后房主按钮不正确 | Actor内部自动赋host，但官方Join响应不含`is_host`，兼容补偿路径没有可靠Session屏障 | **P0/P1** |
| WaitingForReady或Playing重连状态异常 | post-response hack逻辑与注释不一致，补偿包不flush且可能发到新连接 | **P0** |

---

# 3. PMP42 → PMP43修复状态

| PMP42问题 | PMP43状态 | 判断 |
|---|---|---|
|限流后无任何响应 | Chat/CreateRoom/JoinRoom/SelectChart开始映射对应Err | **已关闭主要静默路径** |
|权限拒绝返回None |增加官方命令错误响应映射 | **主体改善，仍需全命令覆盖测试** |
|没有官方客户端响应时序层 |新增`official_client_compat` | **方向正确** |
|响应过快放大客户端竞态 |增加默认10ms最小响应延迟 | **主体改善，初始Authenticate未覆盖** |
|Session命令可等待约60秒 |增加默认4.5秒命令deadline | **入口改善，提交点和响应发送尚未统一** |
|关键响应只进入发送队列 |部分命令使用`send_and_flush` | **部分关闭** |
|官方包顺序与PMP扩展混杂 |新增post-response延迟补偿模块 | **基础形成，不是真正Session屏障** |
|Join包顺序偏离官方 |重新向官方Join顺序靠拢 | **主要顺序改善** |
|Ready/CancelReady部分状态语义偏离官方 |增加官方兼容处理 | **改善，仍有round open失败卡死问题** |
|没有协议兼容追踪 |增加protocol trace计数 | **基础形成，尚未成为生产门禁** |
|没有官方枚举兼容测试 |增加首字节discriminant测试 | **不足，尚非完整golden packet测试** |
|旧命令可能发送到新Session |未解决 | **核心Blocker** |
|重连Authenticate前可能收到房间广播 |未解决 | **核心Blocker** |
|Join内部5秒flush超过外层4.5秒 |仍存在 | **核心Blocker** |
|所有命令提交点检查deadline |仅部分Room命令传播 | **未完成** |

---

# 4. 已真正完成的兼容改进

## 4.1 部分限流不再制造固定7秒超时

当前对以下命令触发限流时，开始返回对应的官方ServerCommand错误：

- Chat；
-CreateRoom；
-JoinRoom；
-SelectChart。

这比PMP42直接warning后return有实质改善。

官方客户端可以在正常响应路径中收到Err，而不是只能等待固定timeout。

## 4.2 权限拒绝开始映射为官方响应

PMP内部存在：

-普通游戏Session；
-控制台Session；
-房间监控Session；
-游戏监控Session。

PMP43开始将不允许的官方请求映射为对应错误包，而不是只返回内部`None`。

这避免普通客户端因为PMP扩展权限模型而进入超时。

## 4.3 增加最小响应延迟基础

默认配置：

```text
minimum_response_latency_ms = 10
```

目的不是改变官方线协议，而是模拟官方服务端及参考实现的自然调度延迟，避免PMP在低负载或本地环境中响应过快，早于官方客户端安装请求回调。

方向符合“客户端不可修改，PMP负责兼容”的原则。

## 4.4 增加Session命令绝对deadline基础

默认：

```text
session_command_deadline_ms = 4500
```

Session mailbox发送与回复开始共享同一个绝对deadline。

相较此前约30秒发送+30秒回复，已经明显接近官方客户端约7秒的等待窗口。

## 4.5 部分关键响应使用send_and_flush

PMP43开始对以下关键命令使用更强发送语义：

- Authenticate；
-CreateRoom；
-JoinRoom；
-RequestStart；
-Ready；
-CancelReady；
-LeaveRoom。

相比只把响应放入服务器mpsc，这可以更早发现socket发送失败。

## 4.6 官方核心顺序开始显式建模

代码开始明确区分：

```text
官方服务端原始消息序列
PMP扩展补偿消息
```

并增加post-response兼容模块，尝试将PMP扩展包延迟到官方响应后发送。

这比把Persistent Room、host修正或额外状态包直接插入Join/Ready核心路径更安全。

---

# 5. P0-01：初始Authenticate成功未应用最低响应延迟

最低响应延迟主要在认证完成后的普通命令分发中应用。

但正常初始Authenticate成功路径会直接：

```rust
send_and_flush(ServerCommand::Authenticate(Ok(...)))
```

没有使用：

```text
CompatTiming::wait_until_minimum(received_at)
```

## 5.1 直接风险

Authenticate通常是连接建立后的第一条请求。

如果：

-用户信息已缓存；
-数据库响应很快；
-PMP与客户端同机或同内网；
-服务器负载低；

PMP可能极快返回Authenticate响应。

官方客户端仍采用：

```text
发送Authenticate
→安装Authenticate callback
```

所以最重要的第一条请求仍可能触发快速响应竞态。

## 5.2 认证失败与成功语义不一致

认证拒绝路径存在人为延迟基础，但成功路径没有统一兼容时序。

实际生产中成功认证远多于失败认证，因此该缺口影响更大。

## 5.3 修复要求

-从读取完整Authenticate命令时记录`received_at`；
-成功和失败均使用同一个minimum response latency；
-响应发送和flush使用认证绝对deadline；
-不得让远程Phira API、数据库或WAL处理无限接近客户端7秒；
-认证compat trace必须纳入统计。

---

# 6. P0-02：mailbox错误先关闭传输，再尝试发送错误响应

当Session Actor出现：

- mailbox不存在；
-mailbox关闭；
-入队超时；
-reply channel关闭；
-reply超时；

当前路径会先调用类似：

```text
close_uncertain_session()
→session.stream.close()
```

随后返回对应官方错误响应。

外层Session再尝试：

```text
send_dispatch_response(error_response)
```

但原连接已经被关闭或发送任务已被中止。

## 6.1 结果

代码表面上已经实现：

```text
超时→对应命令Err
```

但客户端实际上收不到。

最终仍表现为：

```text
deadline has elapsed
```

## 6.2 必须区分两类错误

### 确定未提交

例如命令没有成功进入Actor mailbox。

应：

```text
在原连接deadline内flush对应Err
→再决定是否关闭连接
```

### 提交状态不确定

例如Actor可能已经取得命令，但reply超时。

此时不能假装返回业务Err后继续使用连接。

应：

```text
关闭发起命令的原始Session
→阻止该命令之后提交
→由官方重连Authenticate恢复状态
```

关键不是“先关还是先发”本身，而是必须消除提交不确定性。

---

# 7. P0-03：命令只绑定User，不绑定发起它的Session代际

当前Session Actor命令主要携带：

- User引用；
-command；
-command metadata。

但没有稳定携带：

- originating session ID；
-session generation；
-原始响应发送器；
-原始Session弱引用。

响应发送和post-response补偿通常通过：

```text
user.session
```

重新查找当前Session。

## 7.1 旧命令响应发送到新连接

场景：

```text
Session A发起Join
→命令进入Room Actor

连接A断开
→Session B重新Authenticate
→user.session替换为B

旧Join命令完成
→user.send_and_flush()
→实际发送到Session B
```

Session B并没有发起Join。

官方客户端B可能：

-没有Join回调；
-正在处理Authenticate；
-尚未安装room；
-已经进入其他UI流程。

## 7.2 旧命令关闭新连接

`close_uncertain_session()`如果通过`user.session`关闭当前连接，旧命令超时可能关闭刚建立的Session B。

## 7.3 Post-response补偿发到新Session

当前post-response item通常保存：

```text
Weak<User>
```

延迟后再取当前Session。

因此Session A命令产生的：

- GameStart；
-ChangeState；
-ChangeHost；
-lock/cycle修正；

可能发送到Session B。

## 7.4 修复要求

`CommandMeta`必须增加：

```rust
origin_session_id
origin_session_generation
origin_received_at
absolute_deadline
```

所有响应必须绑定：

```text
发起命令的原始Session发送器
```

所有权威状态提交前验证：

```text
origin Session仍然是当前有效代际
```

过时代际：

```text
不得提交
不得响应到新Session
不得关闭新Session
不得投递兼容补偿
```

---

# 8. P0-04：重连Authenticate响应完成前，新Session已接收房间广播

重连流程会较早执行：

```text
existing_user.set_session(new_session)
```

从这一刻开始，Room广播通过User查找Session时，可能直接送到新连接。

但官方客户端还没有：

-收到Authenticate(Ok)；
-完成Authenticate回调；
-安装认证返回的ClientRoomState；
-进入房间UI。

## 8.1 可能提前到达的包

- ChangeState；
-ChangeHost；
-LockRoom；
-CycleRoom；
-LeaveRoom；
-Ready/CancelReady相关Message；
-插件或PMP扩展状态包。

官方客户端对多个房间事件直接访问当前room。

如果room尚未安装，可能出现：

-接收任务异常；
-后续所有命令超时；
-界面状态分叉。

## 8.2 PMP侧修复

需要增加Session outbound activation barrier：

```text
新连接进入Authenticating
→允许只发送Authenticate响应
→房间广播暂存或抑制
→Authenticate响应经过minimum latency并flush
→Session标记Active
→按官方兼容顺序释放必要房间消息
```

该屏障必须完全由PMP实现，不要求客户端配合。

---

# 9. P0-05：Join外层4.5秒deadline与内部5秒flush冲突

外层Session命令deadline：

```text
4500ms
```

Join内部成功响应却单独使用：

```text
timeout(5s, send_and_flush(JoinRoom Ok))
```

## 9.1 时间线

```text
T=0：收到Join
T=4.5s：外层route_via_mailbox超时
→关闭Session或返回超时错误

Join内部仍可继续到T=5s或更晚：
→尝试发送Join Ok
→执行回滚
→广播Leave
→返回Join Err
```

## 9.2 后果

-状态可能在外层超时后提交；
-客户端已断线后仍进行发送；
-可能向新Session发送迟到响应；
-Join成功包、Leave包、Err包可能产生冲突；
-用户重试时服务器已处于房间中。

## 9.3 修复

所有Join阶段必须使用同一个绝对deadline：

```text
Actor入队
Actor处理
房间提交
响应minimum latency
send_and_flush
```

每一步只使用：

```text
remaining = absolute_deadline - now
```

不得再使用独立5秒常量。

---

# 10. P0-06：Join在响应写失败后执行事务式回滚并不安全

当前Join大致会先完成：

- Actor AddUser；
-room connection mapping；
-user.room；
-room history；
-OnJoinRoom；
-Message::JoinRoom；
-插件/运行时事件。

随后发送JoinRoom(Ok)。

如果`send_and_flush`失败，代码尝试：

```text
remove_user
→回滚房间关系
```

## 10.1 send_and_flush不是客户端应用层确认

socket write成功或失败不能证明：

-客户端完全没有收到；
-客户端只收到部分frame；
-客户端已经处理Join Ok；
-客户端接收任务是否存活。

所以“写失败→回滚”无法建立严格事务。

## 10.2 回滚消息本身可能击穿客户端

若回滚触发：

```text
Message::LeaveRoom
```

而客户端尚未安装room，官方客户端可能访问空room状态。

## 10.3 外部副作用无法完整回滚

以下内容未必能同步撤销：

- room history；
-插件事件；
-运行时事件；
-其他用户已经看到的Join广播；
-房主变化；
-Persistent Room状态。

## 10.4 兼容策略

PMP应以官方服务端提交语义为基线。

一旦官方可观察Join序列已经发出，就不能仅因为后续socket写状态模糊而假装所有事实都未发生。

建议：

-绑定原始Session；
-响应写入失败后关闭原始Session；
-保持权威room状态；
-让重连Authenticate恢复官方状态；
-不向新Session发送旧命令响应；
-不在状态已经对外可见后执行不完整事务回滚。

---

# 11. P0-07：多数命令只在Actor入口检查deadline

PMP43为部分Room Actor命令加入deadline：

- AddUser；
-HostStart；
-SetReady；
-CancelReady。

但其他官方请求仍可能在外层deadline之后继续执行。

## 11.1 未完整覆盖的命令

- Chat；
-CreateRoom；
-LeaveRoom；
-LockRoom；
-CycleRoom；
-SelectChart；
-Played；
-Abort。

## 11.2 典型迟到提交

### SelectChart

可能等待远程Phira API，然后在客户端已经timeout后设置谱面。

### Played

可能等待远程记录获取或校验，之后再提交played状态。

### CreateRoom

可能创建房间、写历史、调用插件后才返回。

### Lock/Cycle

Actor已经改变状态后，外层才发现deadline。

### LeaveRoom

客户端已经显示timeout后，服务器稍后才把用户移出房间。

## 11.3 修复要求

每个请求型命令必须定义明确的：

```text
authoritative commit point
```

在commit前检查：

- absolute deadline；
-origin Session generation；
-服务器shutdown状态。

过期后不得再提交。

---

# 12. P0-08：关键响应发送没有统一remaining deadline

通用响应发送逻辑中：

-关键响应调用`send_and_flush().await`；
-普通响应调用`send().await`。

但并未始终套用本命令剩余deadline。

## 12.1 风险

服务器outbound queue拥塞、socket写慢或客户端不读时：

```text
响应发送占用单连接处理任务
→后续Ping和命令被阻塞
→客户端7秒超时
→heartbeat继续失败
```

## 12.2 普通响应仍只保证进入mpsc

以下官方请求响应仍可能只使用队列send：

- Chat；
-LockRoom；
-CycleRoom；
-SelectChart；
-Played；
-Abort。

官方客户端同样为这些命令等待响应。

## 12.3 修复要求

所有请求型响应统一采用：

```text
absolute deadline
→minimum response latency
→bounded send_and_flush
```

不应只按“关键/非关键”区分是否可靠送入socket。

---

# 13. P0-09：PMP扩展工作仍可能阻塞官方响应

客户端兼容要求：

```text
官方核心状态和官方包序列
```

不能等待PMP扩展工作完成。

当前部分路径仍可能等待：

- room history持久化；
-插件事件gate；
-运行时事件；
-远程Phira API；
-非关键统计；
-Persistent Room操作。

## 13.1 示例

### CreateRoom

可能等待历史持久化和插件分发之后才完成响应。

### JoinRoom

可能等待历史、room事件、插件事件之后才响应。

### Lock/Cycle

Room Actor内可能先await插件分发。

### HostStart/Ready

check_all_ready可能等待round持久化。

## 13.2 兼容要求

PMP扩展工作应分类：

### 官方提交前必须完成

仅限影响官方权威状态正确性的关键事务。

### 官方响应后执行

-审计事件；
-普通插件通知；
-遥测；
-非关键历史；
-扩展补偿。

任何response-after工作都必须：

-有界；
-不阻塞Actor；
-绑定正确Session代际；
-不能插入官方核心序列中间。

---

# 14. P0-10：全员Ready时Round持久化失败会留下“全Ready但不开始”

`check_all_ready()`在所有用户Ready后，会尝试：

```text
open_round()
```

如果数据库或持久化失败：

- round_id被清理；
-函数返回；
-用户仍全部在started集合；
-房间仍可能停留WaitingForReady；
-触发Ready的命令仍可能返回Ok。

## 14.1 用户表现

客户端看到：

-自己Ready成功；
-其他人也Ready；
-游戏迟迟不开始；
-再次点击可能提示already ready；
-界面像卡死。

这与用户报告的Ready问题高度相关。

## 14.2 修复要求

若round open属于进入Playing前的正式提交条件：

-在触发命令deadline内失败时，Ready或RequestStart必须返回明确Err；
-恢复started集合到可重新触发状态；
-或进入有自动重试的显式状态；
-不得留下全员Ready但永远没有新触发事件的死状态。

---

# 15. P0-11：Persistent Room首次加入房主状态不能可靠送达

Room Actor在空房间加入第一个非monitor用户时，可能内部直接设置：

```text
host_id = user
```

随后外层`assign_room_host_if_missing()`发现已经有host，不再发送ChangeHost。

但官方JoinRoomResponse不包含：

```text
is_host
```

官方客户端加入后默认：

```text
is_host = false
```

## 15.1 结果

服务器认为该用户是房主，但客户端没有房主权限UI。

## 15.2 不能简单提前发送ChangeHost

若Join响应前发送ChangeHost，官方客户端可能尚未安装room。

## 15.3 修复

-将首次host变更纳入官方兼容post-response队列；
-必须绑定原始Session；
-JoinRoom响应flush完成后再发送ChangeHost；
-保证不被后续房间事件越过；
-不能使用当前`user.session`动态解析。

---

# 16. P0-12：post-response模块不是可靠Session屏障

当前post-response补偿通过：

```text
tokio::spawn
→sleep
→Weak<User>
→user.try_send
```

实现。

它不能保证：

-目标仍是原始Session；
-发送成功；
-消息完成flush；
-任务未在shutdown时取消；
-与其他Room广播保持顺序；
-新连接不会收到旧命令补偿。

## 16.1 需要的真正机制

应改为每Session有序兼容队列：

```text
Official response
→flush completion fence
→compatibility queue
→extension correction packets
```

队列项必须携带：

- origin session ID；
-origin command ID；
-room ID；
-order sequence；
-expire deadline。

Session代际变化时直接丢弃旧补偿。

---

# 17. P0-13：WaitingForReady与Playing兼容补偿逻辑不一致

代码注释表达的目标大致是：

```text
先让官方客户端以SelectChart状态安装房间
→响应后发送GameStart
→恢复WaitingForReady
```

但实际分支存在不一致：

-普通WaitingForReady加入可能直接在Join响应中返回WaitingForReady，而不是携带谱面ID的SelectChart；
-随后补GameStart时客户端可能缺少正确chart；
-Playing late join可能返回SelectChart但没有可靠修正到Playing；
-补偿是否执行依赖布尔条件，与注释不完全一致。

## 修复要求

必须通过未修改客户端和官方/参考服务端抓包验证，确定每个场景的精确服务器序列：

-加入SelectChart房间；
-加入WaitingForReady房间；
-Playing期间monitor加入；
-Playing期间普通用户重连；
-Persistent Room恢复后加入。

不能只依靠注释或固定延迟猜测。

---

# 18. P1：CreateRoom包顺序仍可能偏离官方

官方CreateRoom路径通常先发送房间创建消息/状态，再返回CreateRoom响应。

PMP当前兼容层部分路径可能：

```text
CreateRoom响应
→Message::CreateRoom
```

或者将扩展状态混入。

这可能不一定触发客户端崩溃，但属于官方可观察行为偏差。

必须纳入逐命令差分抓包测试。

---

# 19. P1：最低10ms响应延迟尚未由真实客户端验证

10ms具有合理参考依据，但目前仍是配置默认值，不是由实际未修改客户端测试确定的生产门槛。

必须测试：

```text
0ms
1ms
2ms
5ms
10ms
20ms
```

场景包括：

-本机客户端；
-Android真机；
-低负载服务器；
-高性能服务器；
-认证缓存命中；
-Ready和Join快速路径。

目标是找出稳定下限，而不是盲目增加延迟。

---

# 20. P1：协议兼容测试只检查首字节枚举值

PMP43增加了官方ClientCommand/ServerCommand第一字节discriminant测试。

这只能证明：

```text
主要枚举序号未发生明显插入
```

不能证明：

-完整字段顺序；
-整数编码；
-Option编码；
-Vec和字符串边界；
-Message嵌套布局；
-PMP追加字段被旧客户端正确忽略；
-错误包和房间状态字段兼容。

需要使用官方客户端实际依赖commit建立完整golden packet双向测试。

---

# 21. P1：compatibility配置可以被关闭

当前compatibility配置可设置：

```text
official_phira_client = false
minimum_response_latency_ms = 0
```

但PMP产品目标就是兼容官方客户端。

生产配置不应允许误关闭核心兼容层后仍进入Ready。

建议：

-生产模式强制官方客户端兼容；
-开发模式才允许关闭；
-启动日志明确输出兼容参数；
-不安全配置标记not-ready或至少高等级警告。

---

# 22. P1：协议追踪尚未成为生产指标

当前已有：

- request received；
-response queued；
-response flushed；
-silent response paths；
-late commit；
-latency histogram。

但仍需：

-暴露到TUI/OpenUDS/metrics；
-按命令类型拆分；
-记录Session generation；
-记录官方序列偏差；
-将`s​​ilent_response_paths > 0`作为生产告警；
-将`late_commit > 0`作为发布阻断；
-将response latency P99与客户端deadline比较。

---

# 23. 官方命令行为对齐矩阵

| 命令 | 官方核心行为 | PMP43现状 | 结论 |
|---|---|---|---|
| Authenticate |认证/恢复后响应 |初始成功未应用minimum latency | **P0** |
| Chat |处理后Chat(Result) |限流已返回Err，发送可靠性仍不足 | **P1/P0** |
| CreateRoom |创建、官方消息、响应 |有deadline基础，包序列和扩展工作仍需差分 | **P0/P1** |
| JoinRoom |AddUser→Join事件→响应 |顺序改善，但deadline、rollback、Session代际未闭环 | **P0** |
| LeaveRoom |移除/广播→响应 |可能迟于deadline提交 | **P0** |
| LockRoom |改变状态/消息→响应 |可能等待插件，普通响应不flush | **P0/P1** |
| CycleRoom |改变状态/消息→响应 |同上 | **P0/P1** |
| SelectChart |选择谱面/消息→响应 |外部API可能造成迟到提交 | **P0** |
| RequestStart |GameStart/状态→响应 |顺序改善，round持久化失败可留下死状态 | **P0** |
| Ready |Ready消息/check all→响应 |顺序改善，round open失败和迟到命令仍存在 | **P0** |
| CancelReady |状态/消息→响应 |deadline只部分传播 | **P0/P1** |
| Played |提交结果→响应 |外部API/数据库可能超过deadline | **P0** |
| Abort |中止→响应 |普通响应可靠性与deadline需补齐 | **P0/P1** |
| Touches/Judges |无请求响应 |保持无响应 | **符合官方** |

---

# 24. PMP43服务器内部审计延续

客户端兼容问题不能掩盖此前服务器内部生产门禁。

本轮WAL、HighFrequency和Plugin TCP核心实现相对PMP42没有发现新的静态回归，但仍需继续验证。

## 24.1 WAL和低频持久化

PMP42已形成：

-AppendOutcome分类；
-AdmittedDegraded；
-Rejected；
-FatalUnknown；
-完整frame确认；
-durable rollback；
-fatal状态禁止Admission和ACK；
-marker修复；
-compact错误锁定；
-多原因健康状态。

当前估计完成度：

| 项目 | 完成度 |
|---|---:|
|WAL格式和迁移|95%|
|Admission线性化|94%|
|append失败分类|92%|
|fatal状态锁定|93%|
|marker完整性|91%|
|真实文件系统故障注入|80%|

仍要求：

-真实partial write；
-fsync失败；
-truncate失败；
-marker rename失败；
-父目录fsync失败；
-崩溃重启验证。

## 24.2 HighFrequency

PMP42已形成：

-Admission/Shutdown gate；
-absolute deadline；
-数据库单次调用timeout；
-terminal intervals；
-accepted/committed/dropped对账；
-失败影响进程退出状态。

当前估计完成度：

| 项目 | 完成度 |
|---|---:|
|Admission/Shutdown线性化|94%|
|DB deadline|94%|
|terminal对账|94%|
|退出状态|95%|
|真实数据库故障测试|82%|

## 24.3 Plugin TCP

PMP42已形成：

-每连接mailbox；
-同连接FIFO；
-不同连接有界并发；
-总字节预算；
-单连接预算；
-单事件预算；
-生命周期保护。

当前估计完成度：

| 项目 | 完成度 |
|---|---:|
|每连接FIFO设计|91%|
|事件数量有界|93%|
|字节预算|91%|
|生命周期可靠性|89%|
|真实慢插件压力测试|80%|

## 24.4 Persistent Room

启动顺序和基础恢复已经形成，但仍需：

-完整重启集成测试；
-host、lock、cycle、chart对账；
-恢复后官方客户端实际状态验证；
-required模式下字段失败not-ready。

---

# 25. 完成度总表

完成度是工程审计估算，表示距离生产闭环的程度，不代表代码量比例。

| 大项 | PMP42 | PMP43 | 当前判断 |
|---|---:|---:|---|
|服务器架构与功能能力|96%|**96%**|主体完整|
|Room Actor内部一致性|93%|**93%**|主体稳定|
|PostgreSQL与低频持久化|93%|**93%**|本轮无回归|
|HighFrequency|94%|**94%**|本轮无回归|
|Plugin TCP|90%|**90%**|本轮无回归|
|官方二进制协议兼容|86%|**87%**|增加基础测试|
|请求错误响应覆盖|55%|**78%**|限流和权限明显改善|
|官方响应时序兼容|45%|**65%**|新增minimum latency和flush基础|
|Actor deadline兼容|35%|**50%**|入口deadline形成，提交点未闭环|
|Join端到端可靠性|55%|**58%**|顺序改善，Session代际和rollback仍危险|
|Ready/Start端到端可靠性|48%|**62%**|顺序改善，round open失败仍阻断|
|重连和Session代际兼容|68%|**45%**|发现旧命令发送到新Session的核心风险|
|未修改官方客户端测试|35%|**35%**|仍缺真实门禁|
|**综合生产完成度**|约76%|**约81%**|兼容层进步明显，但命令代际闭环不足|

---

# 26. 当前生产阻断项

| 生产门禁 | 完成度 | 是否阻断 |
|---|---:|---|
|初始Authenticate minimum latency|45%|**是**|
|错误响应在关闭连接前可靠送达|40%|**是**|
|命令绑定origin Session generation|20%|**是**|
|重连Authenticate outbound barrier|20%|**是**|
|Join统一absolute deadline|45%|**是**|
|Join响应失败后的确定语义|35%|**是**|
|所有命令commit point deadline|40%|**是**|
|所有请求响应使用remaining deadline|45%|**是**|
|扩展工作不阻塞官方响应|55%|**是**|
|全Ready后round open失败恢复|45%|**是**|
|Persistent Room首次host补偿|45%|**是/条件阻断**|
|可靠per-session post-response barrier|30%|**是**|
|WaitingForReady/Playing补偿序列|35%|**是**|
|完整官方差分抓包测试|40%|**是**|
|未修改客户端零超时压力测试|35%|**是**|
|WAL/HF内部可靠性|93%|继续故障测试|
|Plugin TCP|90%|继续压力测试|

---

# 27. PMP43 Core P0任务清单

## P0-A：命令绑定原始Session

- [ ] `CommandMeta`增加origin session ID；
- [ ]增加origin generation；
- [ ]保存原始Session弱引用或专用response sink；
- [ ]所有响应只能发送到origin Session；
- [ ]旧命令不得关闭当前新Session；
- [ ]所有commit point验证generation；
- [ ]重连后旧命令自动失效；
- [ ]post-response item绑定origin Session。

## P0-B：Authenticate兼容屏障

- [ ]初始Authenticate成功应用minimum latency；
- [ ]失败和成功使用一致的timing；
- [ ]Authenticate使用absolute deadline；
- [ ]新Session在Authenticate flush前不可接收room广播；
- [ ]Authenticate flush后激活Session；
- [ ]缓存期间到达的官方房间包按顺序释放；
- [ ]管理Session和普通Session路径明确区分；
- [ ]认证trace纳入指标。

## P0-C：统一绝对deadline

- [ ]从网络收到命令时创建absolute deadline；
- [ ]mailbox发送使用remaining；
- [ ]Actor reply使用remaining；
- [ ]外部API调用使用remaining；
- [ ]官方commit point前检查remaining；
- [ ]minimum latency使用remaining；
- [ ]send_and_flush使用remaining；
- [ ]禁止Join内部独立5秒timeout。

## P0-D：关闭与错误响应语义

- [ ]确定未提交时先flush对应Err；
- [ ]不确定状态不得伪装业务Err；
- [ ]不确定状态关闭origin Session；
- [ ]关闭操作只能作用于origin Session；
- [ ]回复发送失败进入明确连接终态；
- [ ]禁止先关闭再尝试发送；
- [ ]所有路径记录terminal outcome。

## P0-E：Join命令闭环

- [ ]Join所有阶段共享absolute deadline；
- [ ]AddUser前验证origin generation；
- [ ]官方Join序列与官方抓包一致；
- [ ]响应只发origin Session；
- [ ]响应写失败后不执行不完整事务回滚；
- [ ]重连Authenticate恢复权威room；
- [ ]不把旧Join响应发送到新Session；
- [ ]超时后不允许迟到加入。

## P0-F：Ready与RequestStart闭环

- [ ]Ready/Start每个commit point检查deadline；
- [ ]响应不等待非关键插件或遥测；
- [ ]round open失败不得返回Ok并留下全Ready死状态；
- [ ]失败后恢复started集合或显式可重试状态；
- [ ]旧Session Ready不得作用于新Session；
- [ ]响应只发origin Session；
- [ ]多人同时Ready压力测试；
- [ ]数据库故障下客户端获得明确结果。

## P0-G：所有官方请求响应可靠交付

- [ ] Chat使用bounded send_and_flush；
- [ ] LockRoom使用bounded send_and_flush；
- [ ] CycleRoom使用bounded send_and_flush；
- [ ] SelectChart使用bounded send_and_flush；
- [ ] Played使用bounded send_and_flush；
- [ ] Abort使用bounded send_and_flush；
- [ ]所有响应使用同一remaining deadline；
- [ ] outbound拥塞时在客户端deadline前终止。

## P0-H：扩展工作隔离

- [ ]官方核心序列不能等待普通插件事件；
- [ ]普通历史和遥测移动到response-after；
- [ ]插件event gate不得阻塞官方响应；
- [ ]Persistent Room补偿进入per-session compatibility queue；
- [ ]补偿绑定origin generation；
- [ ]补偿发送失败可观测；
- [ ]补偿不得越过后续官方事件；
- [ ]shutdown时兼容队列有明确处理。

## P0-I：官方状态补偿序列

- [ ]首次host变更在Join response flush后发送；
- [ ] lock/cycle修正顺序固定；
- [ ] WaitingForReady加入序列由真实客户端验证；
- [ ] Playing重连序列由真实客户端验证；
- [ ]不在客户端安装room前发送ChangeHost/ChangeState；
- [ ]所有补偿使用官方ServerCommand；
- [ ]不扩展客户端协议；
- [ ]逐场景golden trace。

---

# 28. P1任务清单

## 协议门禁

- [ ]完整ClientCommand golden packet；
- [ ]完整ServerCommand golden packet；
- [ ]Message嵌套字段golden packet；
- [ ]Option/Vec/String边界；
- [ ]PMP扩展只能追加；
- [ ]普通Session禁止收到管理扩展包；
- [ ]官方客户端commit双向编解码。

## 兼容配置

- [ ]生产模式强制官方兼容；
- [ ]不允许minimum latency为0；
- [ ]启动输出最终兼容参数；
- [ ]不安全配置not-ready；
- [ ]响应延迟值来自真实测试。

## 可观测性

- [ ]protocol trace暴露到metrics；
- [ ]按命令拆分延迟；
- [ ]按Session generation记录；
- [ ]silent response路径必须为0；
- [ ]late commit必须为0；
- [ ]cross-session response必须为0；
- [ ]compat queue drop必须为0；
- [ ]P99响应小于客户端deadline。

## Persistent Room

- [ ]恢复后官方客户端状态对账；
- [ ]空房首次host补偿；
- [ ]required模式恢复失败not-ready；
- [ ]lock/cycle/chart/host完整验证。

---

# 29. 必须新增的生产门禁测试

所有客户端测试必须使用**未修改的官方Phira客户端及其实际依赖**。

## 29.1 初始Authenticate快速响应

```text
本机PMP
用户缓存命中
数据库低延迟
```

断言：

- Authenticate无timeout；
-客户端接收任务不异常；
-后续Join/Ready正常。

## 29.2 旧命令跨Session

```text
Session A发起Join并阻塞
Session B重连Authenticate
释放旧Join
```

断言：

- Join响应不发送给B；
-B不收到旧补偿；
-旧命令不关闭B；
-服务器状态符合generation规则。

## 29.3 认证广播屏障

```text
用户重连
Authenticate响应尚未flush
房间同时产生ChangeState/ChangeHost
```

断言新Session先收到Authenticate，完成屏障后才收到房间包。

## 29.4 Join deadline

```text
Room Actor或outbound故意阻塞4秒以上
```

断言：

-4.5秒deadline内得到确定结果或连接关闭；
-deadline后Join不再提交；
-不存在超时后already-in-room。

## 29.5 Join响应写失败

模拟：

-队列满；
-socket write失败；
-flush超时。

断言：

-不会对已可观察状态执行危险回滚；
-不会发送双响应；
-重连Authenticate可以恢复。

## 29.6 Ready round open失败

```text
所有玩家Ready
PostgreSQL round open失败
```

断言：

-客户端获得明确结果；
-房间不会永久全Ready但不开始；
-恢复后可以重新Ready或自动重试。

## 29.7 扩展工作阻塞

阻塞：

-插件event gate；
-room history；
-遥测。

断言官方Ready/Join响应仍在deadline内完成。

## 29.8 Persistent Room首次host

第一个玩家加入空Persistent Room。

断言：

-Join成功；
-客户端最终显示房主；
-ChangeHost不早于room安装；
-没有room空状态异常。

## 29.9 WaitingForReady和Playing

分别测试：

-普通加入；
-monitor加入；
-断线重连；
-host重连。

记录并验证官方客户端实际可接受的完整包序列。

## 29.10 长时间官方客户端压力

至少：

-100客户端；
-反复Join/Leave；
-多人Ready；
-RequestStart；
-弱网；
-重连；
-发送队列拥塞；
-运行1小时。

门禁：

```text
deadline has elapsed = 0
Join首次超时 = 0
Ready卡死 = 0
cross-session response = 0
silent response = 0
late commit = 0
client recv task异常 = 0
```

---

# 30. Go / No-Go生产门槛

PMP只有满足以下条件，才能重新评估Production Candidate。

## Session代际

- [ ]每个命令绑定origin Session；
- [ ]旧命令不能响应到新Session；
- [ ]旧命令不能关闭新Session；
- [ ]重连前命令在commit前自动失效；
- [ ]补偿消息不跨Session代际。

## 响应和deadline

- [ ]初始Authenticate应用兼容延迟；
- [ ]所有请求使用同一absolute deadline；
- [ ]所有commit point检查deadline；
- [ ]所有响应在remaining deadline内flush；
- [ ]deadline后不再迟到提交；
- [ ]不存在先关闭再发送错误响应。

## Join和Ready

- [ ]Join官方序列完全一致；
- [ ]Join超时后不形成already-in-room；
- [ ]Ready/RequestStart官方顺序一致；
- [ ]round open失败可恢复；
- [ ]Persistent Room host状态可正确到达客户端。

## 重连

- [ ]Authenticate响应前不发送房间包；
- [ ]认证快照准确；
- [ ]屏障后按官方顺序释放消息；
- [ ]无需客户端新增命令或协议字段。

## 测试

- [ ]官方服务端差分测试；
- [ ]未修改官方客户端测试；
- [ ]跨Session迟到命令测试；
- [ ]弱网和拥塞测试；
- [ ]插件/数据库故障测试；
- [ ]连续运行零timeout门禁。

---

# 31. 最终判断

PMP43已经完成了官方客户端兼容层的第一轮实质建设：

```text
静默限流修复
权限错误响应
minimum response latency
4.5秒Session deadline
部分关键响应flush
官方序列显式建模
post-response补偿基础
协议追踪
```

这些修改方向正确，也说明此前用户报告的客户端问题已经被正式纳入PMP生产设计。

但当前兼容层仍未解决最关键的连接代际问题：

```text
命令绑定User
而不是绑定发起命令的具体Session
```

这使旧命令、迟到响应、错误关闭和兼容补偿都可能作用于重连后的新连接。

同时，4.5秒deadline还没有覆盖：

-所有Actor提交点；
-外部API；
-响应minimum latency；
-send_and_flush；
-Join内部独立5秒timeout；
-失败回滚。

PMP扩展事件、插件和持久化仍可能阻塞官方核心响应。

因此最终结论：

> **PMP43：NO-GO，继续作为Development Preview。**

当前综合生产完成度约为：

> **81%**

下一轮只建议验收四个闭环：

```text
命令绑定origin Session generation
→ Authenticate outbound activation barrier
→所有提交与响应共享absolute deadline
→ Join/Ready在未修改官方客户端下零timeout
```

只有这些闭环通过真实官方客户端、官方服务端差分以及故障注入测试，PMP才适合重新评估Production Candidate。
