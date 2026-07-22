# 插件示例

本文档收录 PMP 官方插件示例。每个示例展示一种典型用法：

- [欢迎插件](#欢迎插件) — 玩家加入房间时发送欢迎消息
- [游玩时间追踪](#游玩时间追踪) — 记录玩家总游玩时间
- [轮次结果输出](#轮次结果输出) — 每轮结束将结果写入 JSON 文件
- [房间信息 Web API](#房间信息-web-api) — 注册 HTTP 端点暴露房间信息
- [玩家触控追踪](#玩家触控追踪) — 追踪玩家实时触控数据

---

## 欢迎插件

当玩家加入房间时发送系统消息：

```rust
fn on_event(&mut self, event: PluginEvent) -> Result<bool, String> {
    match event {
        PluginEvent::RoomJoin(info) => {
            let msg = format!("欢迎 {} 加入房间！", info.user_id);
            self.api_call("send".into(), vec![
                json!(0),    // user_id = 0 (system)
                json!(msg),
            ]);
            Ok(true)
        }
        _ => Ok(false),
    }
}
```

## 游玩时间追踪

通过 `ext`（扩展 KV 存储）跨会话累加游玩时间：

```rust
fn on_api(&mut self, method: String, args: Vec<JsonValue>) -> ApiResult {
    match method.as_str() {
        "playtime.get" => {
            let uid = args[0].as_i64().unwrap_or(0);
            let key = format!("playtime:{}", uid);
            let data = self.api_call("ext.get".into(), vec![json!(key)]);
            ApiResult::Ok(data)
        }
        _ => ApiResult::Error("unknown method".into()),
    }
}
```

## 轮次结果输出

游戏结束后将结果写入文件：

```rust
fn on_event(&mut self, event: PluginEvent) -> Result<bool, String> {
    if let PluginEvent::RoundComplete(info) = event {
        let filename = format!("round_{}.json", info.round_id);
        let content = serde_json::to_string(&info).unwrap_or_default();
        self.api_call("file.write".into(), vec![
            json!(filename),
            json!(content),
        ]);
    }
    Ok(false)
}
```

## 房间信息 Web API

注册 `/api/rooms/info` 端点返回房间状态：

```rust
fn init(&mut self) -> Result<(), String> {
    self.register_route("GET", "/api/rooms/info")?;
    Ok(())
}

fn on_api(&mut self, method: String, _args: Vec<JsonValue>) -> ApiResult {
    if method == "room_info" {
        let info = self.api_call("get_room_info".into(), vec![]);
        return ApiResult::Ok(info);
    }
    ApiResult::Error("not found".into())
}
```

## 玩家触控追踪

通过 `player_touches` 事件追踪实时触控数据：

```rust
fn on_event(&mut self, event: PluginEvent) -> Result<bool, String> {
    if let PluginEvent::PlayerTouches(info) = event {
        let total = info.data.len();
        println!("玩家 {} 发送了 {} 个触控点", info.user_id, total);
    }
    Ok(false)
}
```

> 完整源码：[HSNPhira-v2-PMP-plugin](https://github.com/FireflyF09/HSNPhira-v2-PMP-plugin)
