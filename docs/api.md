# Phira-mp+ API 文档

## HTTP API（端口 12347）

HTTP API 由 `plugin_http` 提供。

### SSE 事件流

#### `GET /api/events`

SSE（Server-Sent Events）实时事件流。连接后推送 `ready` 事件，随后转发所有广播事件。心跳每 15 秒。

### WebSocket

#### `GET /api/ws`

WebSocket 实时事件流。与 SSE 相同的事件内容，通过二进制 WebSocket 连接传输。

### 插件 SSE 端点

#### `GET /api/rooms/listen`

由插件通过 `sse.register_stream` 注册的 SSE 事件流。
连接后推送 `ready` 事件，事件通过对应插件的 `on_api("sse:translate", ...)` 翻译后推送。
插件返回 `null` 的事件会被跳过。心跳每 15 秒。

### 插件路由

WASM 插件可在运行时通过 `http.register_route` 动态注册路由。插件注册的路由通过 `/{*path}` 通配符派发，当前可查询服务器状态确认已加载的插件路由。
