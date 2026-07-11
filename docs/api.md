# Phira-mp+ 内部 HTTP API 文档

> 这些端点只供 PPB、可信反向代理或受控运维网络使用。PMP 不承担公共 Web API、统一认证、TLS、边缘限流和公网暴露；生产部署不得直接把该端口开放给不可信客户端。

## HTTP API（端口 12347）

HTTP API 由 `plugin_http` 提供。

### SSE 事件流

#### `GET /api/events`

SSE（Server-Sent Events）实时事件流。连接后推送 `ready` 事件，随后转发所有广播事件。心跳每 15 秒。

### WebSocket

#### `GET /api/ws`

WebSocket 实时事件流。与 SSE 相同的事件内容，通过二进制 WebSocket 连接传输。

### 插件 SSE 端点

插件通过 `sse.register_stream` 注册 SSE 事件流后，宿主自动创建对应路由。
连接后推送 `ready` 事件，事件通过对应插件的 `on_api("sse:translate", ...)` 翻译后推送。
插件返回 `null` 的事件会被跳过。`event_types` 为空时接收全部事件，否则宿主会先按事件类型过滤。内置房间事件名为 `create_room`、`update_room`、`join_room`、`leave_room`、`new_round`。插件启用或重载后新增的 SSE 端点即时生效。心跳每 15 秒。

示例：HSNPhira 插件注册了 `/api/rooms/listen`。

### 插件路由

WASM 插件可在运行时通过 `http.register_route` 动态注册路由。路径缺少前导 `/` 时自动补全，重复注册同一路径会替换旧处理器；插件重载后无需重启 HTTP 服务。插件注册的路由通过 `/{*path}` 通配符派发。
