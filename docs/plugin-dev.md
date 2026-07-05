# Phira-mp+ 插件开发文档

Phira-mp+ 支持 **WASM 插件** — 编译为 WebAssembly 组件后通过 wasmtime 组件模型动态加载。

> 旧版 JSON 内存桥 ABI（abi-json-v1）已移除。所有插件必须使用 WIT 组件模型（abi-wit-v2）。
> 插件 SDK crate: `phira-plugin-sdk`

## WASM 组件插件

WASM 插件通过 wasmtime 组件模型加载，部署在 `plugins/` 目录下（可通过 `-d` 参数自定义）。

### WIT ABI v2

插件使用 WIT（WebAssembly Interface Types）定义宿主与插件之间的接口。WIT 文件位于 [`wit/phira-plugin.wit`](../wit/phira-plugin.wit)。

**World:** `phira-plugin-v2`

#### 插件导出（插件需实现）

| 导出函数 | 签名 | 说明 |
|---------|------|------|
| `init` | `func() -> result<_, string>` | 初始化插件，返回 ok 或 error |
| `get-info` | `func() -> plugin-info` | 返回插件元数据（名称、版本、作者、描述） |
| `cleanup` | `func()` | 插件卸载时清理 |
| `on-event` | `func(event: plugin-event) -> result<bool, string>` | 处理事件，返回 true=已处理 |
| `on-api` | `func(method: string, args: list<json-value>) -> api-result` | API 调用入口 |

#### 宿主导入（宿主提供，共 12 个接口）

**phira-host** — 基础宿主功能

| 函数 | 签名 | 说明 |
|------|------|------|
| `log` | `func(level: string, message: string)` | 记录日志（级别: error/warn/info/debug/trace） |
| `generate-uuid` | `func() -> string` | 生成 UUID v4 |
| `current-time-ms` | `func() -> u64` | 获取 Unix 毫秒时间戳 |
| `api-call` | `func(method: string, args: list<json-value>) -> api-result` | 通用 API 查询（rooms.* / runtime.* / simulation.* 等） |
| `send-chat` | `func(user-id: u32, message: string)` | 发送聊天消息 |
| `http-request` | `func(url, method, headers, body) -> result<http-response, string>` | 发起 HTTP 请求（受 WASM 沙箱限制） |

**phira-query** — 数据查询

| 函数 | 说明 |
|------|------|
| `get-user(user-id) -> api-result` | 查询用户信息 |
| `get-room(room-id) -> api-result` | 查询房间信息 |
| `list-rooms() -> api-result` | 列出活跃房间 |
| `list-online-users() -> api-result` | 列出在线用户 |
| `is-user-online(user-id) -> bool` | 用户是否在线 |
| `get-user-extra / set-user-extra` | 扩展数据读写 |
| `get-room-extra` | 房间扩展数据读取 |

**phira-room-mgmt** — 房间管理

| 函数 | 说明 |
|------|------|
| `create-empty-room(room-id, endpoint?) -> api-result` | 创建空房间 |
| `kick-from-room(room-id, target-id) -> api-result` | 踢出房间 |
| `transfer-host(room-id, target-id) -> api-result` | 转让房主 |
| `set-host(room-id, target-id?) -> api-result` | 设置房主 |
| `set-room-lock / set-room-hidden / close-room` | 房间设置 |

**phira-user-mgmt** — 用户管理

| 函数 | 说明 |
|------|------|
| `kick-user(user-id, reason) -> api-result` | 踢出用户 |
| `ban-user / unban-user` | 封禁/解封 |
| `get-ban-list -> api-result` | 封禁列表 |
| `is-banned(user-id) -> bool` | 是否被封禁 |

**phira-messaging** — 消息发送

| 函数 | 说明 |
|------|------|
| `send-to-user(user-id, message) -> api-result` | 私信 |
| `send-to-room(room-id, message) -> api-result` | 房间广播 |
| `send-to-all(message) -> api-result` | 全服广播 |

**phira-persistence** — 持久化查询

| 函数 | 说明 |
|------|------|
| `query-events(since, limit, kind?, room-id?, user-id?) -> api-result` | 增量事件查询 |
| `query-room-snapshots(since, limit) -> api-result` | 房间快照 |
| `query-touches / query-judges` | 触控/判定数据 |
| `get-playtime / top-playtime` | 游玩时间 |

**phira-admin** — 管理员管理

| 函数 | 说明 |
|------|------|
| `list-admin-ids -> api-result` | 管理员列表 |
| `is-admin(user-id) -> bool` | 是否管理员 |
| `add-admin-id / remove-admin-id / set-admin-ids` | 管理操作 |

**phira-config** — 插件配置

| 函数 | 说明 |
|------|------|
| `get-config / set-config / list-config` | 配置读写 |
| `reload-config` | 重新加载 config.json |
| `poll-config-changes` | 轮询配置变更 |

**phira-simulation** — 模拟器

| 函数 | 说明 |
|------|------|
| `status -> api-result` | 当前状态 |
| `run(preset, users?, rooms?, duration?) -> api-result` | 启动模拟 |
| `stop / cleanup` | 停止/清理 |

**phira-runtime** — 运行时诊断

| 函数 | 说明 |
|------|------|
| `status -> api-result` | 运行时状态 |
| `events(limit?) -> api-result` | 事件总线统计 |
| `commands -> api-result` | 命令注册表统计 |

### 完整 API 参考

WIT 接口完整定义见 [`wit/phira-plugin.wit`](../wit/phira-plugin.wit)。

### 插件 SDK

`phira-plugin-sdk` crate 提供 Rust 插件的开发支持：

```toml
[dependencies]
phira-plugin-sdk = { path = "../phira-plugin-sdk" }
```

SDK 提供 WIT bindgen 生成宏和类型定义，编译后自动生成符合 `phira-plugin-v2` world 的 WASM 组件。

### 插件生命周期

1. 服务器启动时扫描 `plugins/` 目录下的 `.wasm` 文件
2. WASM 组件编译并实例化
3. `init()` 被调用 → 返回 ok 表示初始化成功
4. 运行期间事件通过 `on-event()` 分发
5. 插件之间可通过 `on-api()` 互相调用
6. 服务器关闭 / 插件重载时调用 `cleanup()`
