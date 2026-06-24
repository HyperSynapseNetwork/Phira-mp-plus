# Phira-mp+ 插件开发文档

Phira-mp+ 支持 **WASM 插件** — 编译为 WebAssembly 后通过 wasmtime 动态加载。

> 内置插件系统（NativePlugin trait）已合并入服务器核心代码，不再支持独立原生插件注册。
> 外部扩展请使用 WASM 插件。

## WASM 插件

WASM 插件通过 wasmtime 运行时动态加载，部署在 `plugins/` 目录下（可通过 `-d` 参数自定义）。

### JSON ABI

WASM 插件通过 JSON 字符串与宿主通信：

**导出函数：**

| 函数 | 签名 | 说明 |
|------|------|------|
| `phira_init` | `() -> i32` | 初始化插件，返回 0 表示成功 |
| `phira_get_info` | `() -> ()` | 填写插件元数据到内存偏移 0 |
| `phira_cleanup` | `() -> ()` | 插件卸载时清理 |
| `phira_on_event` | `(ptr: i32, len: i32) -> i32` | 事件处理，返回 0=已处理 1=未处理 |
| `phira_alloc` | `(size: i32) -> i32` | 分配线性内存，返回指针 |
| `phira_dealloc` | `(ptr: i32, size: i32)` | 释放内存 |

**导入函数（宿主提供）：**

| 函数 | 说明 |
|------|------|
| `phira:host/log(level_ptr, level_len, msg_ptr, msg_len)` | 记录日志 |
| `phira:host/uuid(out_ptr, out_len)` | 生成 UUID v4 |
| `phira:host/time() -> i64` | 获取 Unix 时间戳（毫秒） |
| `phira:host/api(method_ptr, method_len, args_ptr, args_len, out_ptr, out_len) -> i32` | 通用 API 桥接 |

### 通用 API (`phira:host/api`)

所有方法通过 JSON 参数调用，返回 JSON 结果。返回码：0=成功，非0=错误。

| 方法 | 参数 | 返回 | 说明 |
|------|------|------|------|
| `state.query` | `{ method, params }` | 查询结果 | 统一状态查询入口 |
| `player.touches` | `{ user_id }` | 最近触控数据 | 查询用户最近触控帧 |
| `player.judges` | `{ user_id }` | 最近判定数据 | 查询用户最近判定事件 |
| `round.data` | `{ round_uuid, player_id }` | 完整 Touches/Judges | 轮次数据查询 |
| `round.list` | `{}` | 轮次列表 | 所有已记录轮次 |
| `send.to_user` | `{ user_id, message }` | `"ok"` | 发送消息给指定用户 |
| `send.to_room` | `{ room_id, message }` | `"ok"` | 向房间广播消息 |
| `send.to_all` | `{ message }` | `"ok"` | 向所有用户广播 |
| `ext.get_user` | `{ user_id, key }` | 字段值 | 获取用户扩展数据 |
| `ext.set_user` | `{ user_id, key, value }` | `"ok"` | 设置用户扩展数据 |
| `ext.get_room` | `{ room_id, key }` | 字段值 | 获取房间扩展数据 |
| `ext.set_room` | `{ room_id, key, value }` | `"ok"` | 设置房间扩展数据 |
| `room.kick` | `{ room_id, target_id }` | `{"ok": true}` | 从房间踢出用户 |
| `room.transfer_host` | `{ room_id, target_id }` | `{"ok": true}` | 转移房主 |
| `room.set_lock` | `{ room_id, locked }` | `{"ok": true}` | 锁定/解锁房间 |
| `room.close` | `{ room_id }` | `{"ok": true}` | 解散房间 |
| `admin.kick_user` | `{ user_id, reason }` | `{"ok": true}` | 从服务器踢出用户 |
| `admin.ban_user` | `{ user_id, reason }` | `{"ok": true}` | 封禁用户 |
| `admin.unban_user` | `{ user_id }` | `{"ok": true}` | 解封用户 |
| `admin.is_banned` | `{ user_id }` | `{"banned": bool}` | 检查封禁状态 |
| `admin.ban_list` | `{}` | 封禁列表 | 获取所有封禁 |
| `admin.list_users` | `{}` | 用户列表 | 列出所有在线用户 |
| `plugin.api_call` | `{ plugin, method, args }` | 调用结果 | 调用其他插件注册的 API |
| `plugin.api_register` | `{ method }` | 注册确认 | 注册本插件 API 供其他插件调用 |
| `config.get` | `{ key }` | 配置值 | 获取插件配置 |
| `config.set` | `{ key, value }` | `"ok"` | 设置插件配置 |
| `http.get` | `{ url }` | 响应正文 | HTTP GET 请求 |
| `http.post` | `{ url, body, content_type }` | 响应正文 | HTTP POST 请求 |
| `file.read` | `{ path }` | 文件内容 | 读取插件数据文件 |
| `file.write` | `{ path, content }` | `"ok"` | 写入插件数据文件 |
| `uuid.v4` | `{}` | UUID 字符串 | 生成 UUID |
| `time.now` | `{}` | Unix 时间戳 | 获取当前时间 |

完整 WIT 定义见 `wit/phira/mpplus.wit`。

### 插件事件

WASM 插件通过 `phira_on_event` 接收事件，事件以 JSON 格式传入。type 字段标识事件类型：

| type | 数据字段 | 触发时机 |
|------|---------|---------|
| `user_connect` | user_id, user_name, user_ip | 用户认证通过后 |
| `user_disconnect` | user_id, user_name | 用户断开连接 |
| `room_create` | user_id, room_id | 创建房间 |
| `room_join` | user_id, room_id, is_monitor | 加入房间 |
| `room_leave` | user_id, room_id | 离开房间 |
| `room_modify` | user_id, room_id, data | 修改房间设置 |
| `game_start` | user_id, room_id | 游戏开始 |
| `game_end` | user_id, user_name, room_id, score, accuracy, perfect, good, bad, miss, max_combo, full_combo | 玩家提交成绩 |
| `player_touches` | user_id, room_id, data | 玩家触控事件 |
| `player_judges` | user_id, room_id, data | 玩家判定事件 |
| `round_complete` | room_id, chart_id, chart_name | 一轮游戏完成 |

示例 — 监听用户连接事件：

```javascript
// 在 phira_on_event 中：
function phira_on_event(event_ptr, event_len) {
  let json = memory_to_string(event_ptr, event_len);
  let event = JSON.parse(json);
  if (event.type === 'user_connect') {
    console.log(`User ${event.user_name}(${event.user_id}) connected from ${event.user_ip}`);
  }
  return 0; // 0 = handled
}
```

### 插件元数据

WASM 插件可通过两种方式声明元数据：

1. **导出 `phira_get_info`** — 插件自行将 JSON 元数据写入线性内存
2. **内存偏移 0** — 在 WASM 模块的偏移 0 处放置长度前缀的 JSON（兼容模式）

```json
{
  "name": "my-plugin",
  "version": "0.1.0",
  "author": "me",
  "description": "My WASM plugin"
}
```

## 插件间 API 调用

WASM 插件可以通过 `plugin.api_call` 调用其他插件注册的 API：

```javascript
// 调用 playtime-tracker 插件的 count 方法
let result = api.call('plugin.api_call', {
  plugin: 'player-tracker',
  method: 'count',
  args: []
});
// result = {"count": 42}
```

通过 `plugin.api_register` 注册自己的 API：

```javascript
api.call('plugin.api_register', {
  method: 'my_custom_api'
});
```

> 注意：完整双向 WASM 回调目前为 stub，注册后可被调用但返回固定响应。
> 原生 Rust 注册的 API（通过 PluginManager::register_plugin_api）可被 WASM 插件正常调用。

## 实时数据流

Touches/Judges 数据通过 `player_touches` 和 `player_judges` 事件推送到 WASM 插件的 `phira_on_event` 处理器，无需主动轮询。

## 构建 WASM 插件

WASM 插件需编译为目标 `wasm32-unknown-unknown`：

```bash
rustup target add wasm32-unknown-unknown
cargo build --target wasm32-unknown-unknown --release
```

生成的 `.wasm` 文件放在服务器的 `plugins/` 目录下，下次启动或执行 `plugin reload` 时加载。

## 服务端配置

WASM 插件可通过 `config.get` / `config.set` API 读写自己的配置（内存中，重启消失）。
持久化配置建议使用 `file.read` / `file.write` 操作 `data/plugins/<plugin_name>/` 目录。

## WIT 接口定义

完整的 WIT 接口定义见 `wit/phira/mpplus.wit`，包含以下接口：

- `user-events` — 用户事件监听
- `user-info` — 用户信息查询
- `room-info` — 房间信息查询
- `messaging` — 消息发送
- `room-management` — 房间管理
- `user-management` — 用户管理
- `utilities` — 工具函数
- `database` — 数据库接口（预留）
- `plugin-config` — 插件配置
- `plugin` — 插件主入口
- `cli` — CLI 命令接口
