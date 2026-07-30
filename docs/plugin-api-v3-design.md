# Plugin API v3 — 能力域扩展设计

> 目标：把当前事件触发插件模型升级为能力工具箱，
> 让插件可以实现 PDFP 联邦、自定义网关、匹配服务等任意长期运行功能。

## 当前缺口

| 能力 | v2 状态 | v3 需要 |
|---|---|---|
| TCP connect | `connect(addr) -> handle` | ✅ 已有 |
| TCP listen | `listen(addr) -> handle` | ✅ 已有 |
| TCP accept | ❌ 无 | `accept(listener) -> conn` |
| TCP 数据接收 | ❌ 无 | 异步事件 `tcp:data` / `tcp:accept` |
| UDP | ❌ 无 | 可选的 `sendto` / `recvfrom` |
| Ed25519 签名 | `sign(payload) -> sig` | ✅ 已有 |
| SHA-256 | `sha256(data) -> hash` | ✅ 已有 |
| 房间状态查询 | JSON blob | 结构化 `room-state` / `player-list` / `round-info` |
| 动态命令注册 | ❌ 无 | `register-handler(method, callback)` |
| 后台持久任务 | ❌ 无 | `spawn-task` / 事件驱动 |
| 定时器 | `set-timer` | ✅ 已有 |
| CBOR 编解码 | ❌ | WASM 侧自行编译；不进入宿主 API |
| ULEB128 | ❌ | 不进入宿主 API（WASM 中几行代码） |

## 设计原则

1. **能力工具箱，不绑定用途**：不出现 `federate-peer()` 或 `join-global-room()` 这种函数名
2. **事件驱动不改**：插件不需要轮询，宿主通过 `on-api` 推送 TCP 事件
3. **结构化数据**：房间状态返回 typed records，不是 JSON blob
4. **权限声明**：每个新能力对应一个 capability 字符串，由管理员授权

## 新增接口

### 1. `phira-tcp` — 补齐 accept + 数据接收

```wit
/// TCP networking — extended with accept and event-driven receive.
interface phira-tcp-v3 {
    /// Connect to a remote TCP endpoint. Returns a connection handle.
    connect: func(addr: string) -> result<u64, string>;

    /// Start a TCP listener. Returns a listener handle.
    listen: func(addr: string) -> result<u64, string>;

    /// Accept an inbound connection from a listener (non-blocking).
    /// Returns null if no pending connection.
    /// Host dispatches `on-api("tcp:accept", [listener, conn])` when
    /// a new connection arrives.
    accept: func(handle: u64) -> result<option<u64>, string>;

    /// Send raw bytes on an established connection.
    send: func(handle: u64, bytes: list<u8>) -> result<_, string>;

    /// Receive buffered data from a connection (non-blocking).
    /// Returns null if no data available.
    /// Host dispatches `on-api("tcp:data", [conn, data])` when data arrives.
    recv: func(handle: u64, max-bytes: u32) -> result<option<list<u8>>, string>;

    /// Close a connection or stop a listener by handle.
    close: func(handle: u64) -> result<_, string>;

    /// Get the remote address of a connection.
    peer-addr: func(handle: u64) -> result<string, string>;
}
```

**事件流**（宿主 → 插件 `on-api`）：

```
on-api("tcp:accept", [listener_handle, new_conn_handle])
  → 插件在 listener 上有新连接
on-api("tcp:data", [conn_handle, data_bytes])
  → 已有连接上收到数据
on-api("tcp:close", [conn_handle])
  → 对端关闭连接
```

插件通过 `set-timer` + `tcp:data` 事件即可维护一个长期 TCP 会话，完全不需要轮询。

### 2. `phira-room-state` — 结构化房间状态

当前 `get-room` 返回 JSON blob，插件必须手动解析。改为结构化查询：

```wit
/// Structured room state query.
interface phira-room-state {
    record room-player {
        user-id: u32,
        display-name: string,
        is-monitor: bool,
        is-ready: bool,
        is-host: bool,
        is-finished: bool,
        score: option<u32>,
        accuracy: option<f32>,
    }

    record room-round {
        round-id: string,
        chart-id: option<u32>,
        chart-name: option<string>,
        phase: string,
    }

    record room-state {
        room-id: string,
        room-uuid: string,
        host-id: option<u32>,
        locked: bool,
        hidden: bool,
        persistent-empty: bool,
        player-count: u32,
        monitor-count: u32,
        players: list<room-player>,
        current-round: option<room-round>,
    }

    /// Get structured room state.
    get-room-state: func(room-id: string) -> result<room-state, string>;

    /// Get all players in a room.
    get-room-players: func(room-id: string) -> result<list<room-player>, string>;

    /// Check a player's membership and status in a room.
    get-player-status: func(room-id: string, user-id: u32) -> result<option<room-player>, string>;
}
```

### 3. `phira-handler` — 动态命令/服务注册

插件可以在运行时注册自己的命令处理器，代替当前 `on-api` 的静态匹配：

```wit
/// Dynamic handler registration.
interface phira-handler {
    record handler-descriptor {
        method: string,
        description: string,
        request-schema: option<string>,
        response-schema: option<string>,
    }

    /// Register a handler for a custom API method.
    /// When called, host dispatches `on-api(method, args)` to this plugin.
    /// Methods are auto-namespaced as `plugin_name.method` to prevent cross-plugin collisions.
    /// Registering a method already owned by another plugin returns an error.
    register-handler: func(desc: handler-descriptor) -> result<_, string>;

    /// Unregister a previously registered handler.
    unregister-handler: func(method: string) -> result<_, string>;

    /// List all handlers registered by this plugin.
    list-handlers: func() -> result<list<handler-descriptor>, string>;

    /// Set the handler for a protocol-assigned on-api event family.
    /// Built-in families: "tcp:*", "timer:*" are always delivered.
    /// Plugin-defined families delivered only after registration.
    set-event-family: func(family: string, enable: bool) -> result<_, string>;
}
```

### 4. 结构化房间成员查询（已有 `phira-query` 扩展）

当前的 `phira-query` 保持不动，新增结构化接口在 `phira-room-state` 中。`get-room` 作为通用 JSON 入口保留。

### 权限声明（capability 清单）

```wit
/// v3 新增 capability 字符串
///
/// "phira:tcp:client"      — 允许 TCP connect
/// "phira:tcp:server"      — 允许 TCP listen + accept
/// "phira:room-state:read" — 允许读取结构化房间状态
/// "phira:handler:register" — 允许注册自定义命令处理器
```

## 插件生命周期变化

当前：`init → on-event / on-api → cleanup`

v3 扩展后：

```
init
  → register-handler("my-service", ...)    # 注册服务
  → tcp:listen / tcp:connect               # 建立网络入口
  → set-timer(...)                          # 周期性维护

运行时：
  → on-api("tcp:data", [conn, bytes])      # 处理入站数据
  → on-api("tcp:accept", [lst, conn])      # 接受新连接  
  → on-api("timer:fired", [timer-id])      # 定时触发
  → on-api("my-service", [args])           # 自定义命令

cleanup
  → close / clear-timer                    # 清理所有资源
```

## 非宿主能力（插件侧自行处理）

以下功能直接在 WASM 侧用库解决，不进宿主 API：

| 功能 | 原因 |
|---|---|
| CBOR 确定性编码 | tiny crate ~2KB WASM |
| ULEB128 帧编码 | 10 行代码 |
| 会话管理状态机 | 业务逻辑，属于插件自身 |
| 事件链哈希验证 | 已暴露 sha256，链由插件维护 |
| 心跳/Ping | `set-timer` + TCP send |
| Peer 路由表 | 插件内存数据 |

## 实现路径

1. 加 `tcp.accept` / `tcp.recv` 宿主实现 + WIT 定义
2. 加 `tcp:*` 事件通过 `on-api` 投递
3. 加 `phira-room-state` 结构化查询接口
4. 加 `phira-handler` 动态注册接口
5. 更新权限检查
6. 示例插件：一个 50 行的 TCP echo server 验证完整链路

## 与 PDFP 的关系（仅供参考，不进入 API）

```text
PDFP FederationAdapter 插件 = 
  phira-tcp-v3     ← 联邦连接传输
  phira-crypto     ← 节点签名、证书验证
  phira-timer      ← 心跳、租约刷新
  phira-room-state ← 规范化房间状态导出
  phira-handler    ← ServiceMethod 注册
  插件自身逻辑     ← CBOR、ULEB128、事件链、会话状态机

没有任何一个 API 是专为 PDFP 设计的。
PDFP 只是这些通用积木的一个组合结果。
```
