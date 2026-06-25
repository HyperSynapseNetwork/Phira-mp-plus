# phira-web-monitor 集成计划

## 现状

phira-web-monitor 是一个独立的代理服务，通过 RoomMonitor 二进制协议连接到 Phira 服务器。
我们的 phira-mp-plus-server 已支持 RoomMonitor 协议，但缺少 web-monitor 所需的 HTTP API 端点。

## 需要添加的 HTTP API 端点

### 房间信息

- [ ] `GET /rooms/info` — 返回所有房间列表（格式: `RoomListResponse`）
- [ ] `GET /rooms/info/{id}` — 返回指定房间详情（格式: `RoomInfoResponse`）
- [ ] `GET /rooms/user/{id}` — 查询用户所在房间
- [ ] `GET /rooms/listen` — SSE 房间事件流（已有 `/rooms/listen`，需确认格式兼容）

### 谱面代理

- [ ] `GET /chart/{id}` — 从 Phira API 代理谱面二进制数据

### 用户访问记录

- [ ] `GET /visited` — 查询曾访问过本服务器的用户（含 `count_only` 参数）
- [ ] 需要 PostgreSQL 存储（已添加 `db.rs` 模块）

### 认证代理

- [ ] `POST /auth/login` — 代理到 Phira API 登录，返回 JWT
- [ ] `GET /auth/me` — 返回当前用户资料（需 JWT 中间件）

## 数据结构

```rust
// GET /rooms/info 响应
pub struct RoomListResponse {
    pub total: usize,
    pub rooms: Vec<RoomInfoResponse>,
}

pub struct RoomInfoResponse {
    pub name: String,
    pub data: RoomData,  // 来自 phira_mp_common
}

// GET /visited 响应
pub struct VisitedUserListResponse {
    pub count: u64,
    pub users: Option<Vec<VisitedUserInfo>>,
}

pub struct VisitedUserInfo {
    pub phira_id: i32,
}
```

## 参考实现

- phira-web-monitor 的 `monitor-proxy/src/handlers/`
- phira-web-monitor 的 `monitor-proxy/src/services/room_service.rs`
