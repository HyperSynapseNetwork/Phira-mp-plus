# HSNPhira V2 → PMP 插件迁移指南

## 概述

HSNPhira V2 是一个独立的 Python Flask 后端，为 HSNPhira 前端提供 Web API 和 SSE 房间事件流。现在它以 WASM 插件形式运行在 Phira-mp+ 上，无需独立部署、不需要 Python 运行时。

本文档说明从 HSNPhira V2 迁移到 `hsnphira-v2-pmp-plugin` 的核心变化。

## 架构变化

| 项目 | HSNPhira V2 | PMP 插件 |
|------|------------|----------|
| 运行方式 | 独立 Python 进程 | WASM 组件模型，运行在 PMP 进程内 |
| 依赖 | Python 3 + Flask + SQLite | 无额外依赖（PMP 提供运行环境） |
| 部署 | 单独启动/停止 | 复制 .wasm 到 plugins/ 目录即可 |
| HTTP 路由 | 通过 Flask 路由 | 通过 `http.register_route` 注册 |
| 鉴权 | 独立的 token/API key | 使用 PMP 的 Shared Key 鉴权（HSN_SECRET_KEY） |
| SSE 事件 | Flask SSE 端点 | PMP SSE Hub + 插件 `sse:translate` 回调 |
| 数据存储 | SQLite（phira_stats.db） | PostgreSQL（由 PMP 统一管理） |

## 快速迁移

### 1. 替换部署方式

**之前（HSNPhira V2）：**
```bash
# 需要 Python 环境
pip install flask
python api.py --port 5000
nginx 反向代理 /api/* → localhost:5000
```

**之后（PMP 插件）：**
```bash
# 只需要一个 .wasm 文件
mkdir -p plugins/hsnphira-v2
cp hsnphira_v2_pmp_plugin.component.wasm plugins/hsnphira-v2/plugin.wasm
# 重启 PMP 即生效
systemctl restart phira-mp-plus
# PMP 自动管理 /api/* 路由
```

### 2. API 端点变化

| 端点 | HSNPhira V2 | PMP 插件 | 变化 |
|------|------------|----------|------|
| 访客统计 | `/api/auth/visited/count` | `/api/auth/visited/count` | 无变化 |
| 房间列表 | `/api/rooms/info` | `/api/rooms/info` | 无变化 |
| 房间详情 | `/api/rooms/info/:name` | `/api/rooms/info/:name` | 无变化 |
| 房间事件流 | `/newapi/rooms/listen` | `/api/rooms/listen` | **路径变化** |
| 游玩排行 | `/rankapi/playtime_leaderboard` | `/rankapi/playtime_leaderboard` | 无变化 |

### 3. SSE 事件格式变化

#### 之前（HSNPhira V2 Python 后端）

```json
// 事件类型名使用 PascalCase
event: RoomCreate
data: {"room": "test_room", "data": {...}}

event: RoomJoin
data: {"room": "test_room", "user": 16}

event: RoomLeave
data: {"room": "test_room", "user": 16}

event: RoomModify
data: {"room": "test_room", "data": {...}}

event: RoundComplete
data: {"room": "test_room"}
```

#### 之后（PMP 插件）

```json
// 事件类型名使用 snake_case，type 字段仅出现在 event: 行
event: create_room
data: {"room": "test_room", "data": {...}}

event: join_room
data: {"room": "test_room", "user": 16}

event: leave_room
data: {"room": "test_room", "user": 16}

event: update_room
data: {"room": "test_room", "data": {...}}

event: start_round
data: {"room": "test_room"}
```

#### 变化对照表

| 旧字段/类型 | 新字段/类型 | 说明 |
|-----------|-----------|------|
| `RoomCreate` | `create_room` | 事件名改为 snake_case |
| `RoomJoin` | `join_room` | — |
| `RoomLeave` | `leave_room` | — |
| `RoomModify` | `update_room` | — |
| `RoundComplete` | `start_round` | 更准确的语义 |
| `room_id`（data 内） | `room` | 字段名简化 |
| `user_id`（data 内） | `user` | 字段名简化 |
| data 包含 `type` | 无 `type` | `type` 仅存在于 SSE `event:` 行 |

### 4. 鉴权变化

HSNPhira V2 使用独立的 API token 鉴权。PMP 插件使用 PMP 的 Shared Key 鉴权机制：

```bash
# 在 PMP 配置中设置共享密钥
# 该密钥会与 "room_monitor" 组合生成鉴权令牌
export HSN_SECRET_KEY="your-secret-key"
```

前端连接 SSE 时通过 `RoomMonitorAuthenticate` 命令验证：

```json
{
  "command": "RoomMonitorAuthenticate",
  "key": "room_monitor:..."  // 由 HSN_SECRET_KEY 派生
}
```

### 5. 前端适配

如果您的前端直接消费 SSE 事件流，需要更新以下内容：

**SSE 端点 URL：**
```javascript
// 之前
const source = new EventSource("/newapi/rooms/listen");

// 之后
const source = new EventSource("/api/rooms/listen");
```

**事件类型处理：**
```javascript
// 之前
source.addEventListener("RoomCreate", (e) => { ... });
source.addEventListener("RoomJoin", (e) => { ... });

// 之后
source.addEventListener("create_room", (e) => { ... });
source.addEventListener("join_room", (e) => { ... });
```

## 构建指引

从源码构建插件的 .wasm 文件：

```bash
# 1. 安装 Rust + WASM 目标
rustup target add wasm32-unknown-unknown
cargo install wasm-tools

# 2. 获取 SDK
# 从 PMP Release 下载 phira-plugin-sdk.tar.gz
wget https://github.com/HyperSynapseNetwork/Phira-mp-plus/releases/latest/download/phira-plugin-sdk.tar.gz
tar xzf phira-plugin-sdk.tar.gz

# 3. 构建
cargo build --target wasm32-unknown-unknown --release
wasm-tools component new \
  target/wasm32-unknown-unknown/release/hsnphira_v2_pmp_plugin.wasm \
  -o target/hsnphira-v2-pmp-plugin.component.wasm
```

## 常见问题

### SSE 事件不显示

如果前端收到空的 SSE 事件（`event:\ndata: {}`），说明插件版本与服务器不匹配。请确保：

1. 插件 WASM 已更新到最新版本
2. 服务器 PMP 版本 ≥ 0.5.1650

旧版插件使用 `"RoomCreate"` 等事件名，但服务器发送 `"create_room"`。新版插件已适配服务器的事件名。

### 鉴权失败

PMP 的 RoomMonitorAuthenticate 使用基于 HSN_SECRET_KEY 的共享密钥。确保：

1. 服务器配置了 `HSN_SECRET_KEY` 环境变量
2. 前端使用正确的密钥生成鉴权令牌

### 数据不兼容

HSNPhira V2 的 SQLite 数据（如游玩时间排行）不会自动迁移到 PMP 的 PostgreSQL。参考 [hsnphira-v2-migration.md](hsnphira-v2-migration.md) 进行数据迁移。
