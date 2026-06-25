# 从 HSNPhira (user.py) 迁移到 Phira-mp+

本文档说明如何将 HSNPhira 后端（`user.py` + SQLite）的数据迁移到 Phira-mp+ 的内置数据格式。

## 数据对比

| 数据类型 | HSNPhira (user.py) | Phira-mp+ |
|----------|-------------------|-----------|
| 游玩时间 | SQLite `user_playtime` + `user_room_duration` | JSON `data/playtime-tracker.json` |
| 房间记录 | SQLite `rooms` + `user_room_activity` | 内置 `room history` 命令 + JSON 轮次存储 |
| 游戏轮次 | SQLite `game_rounds` | `data/rounds/` 目录下按 UUID 存储 |
| 用户数据 | Phira API 实时查询 | 认证缓存 `data/extensions.json` |

## 迁移步骤

### 1. 导出 HSNPhira 数据

```bash
# 从 HSNPhira 的 SQLite 导出游玩时间
sqlite3 phira_stats.db -json "SELECT user_id, SUM(play_duration) as total_seconds FROM user_playtime GROUP BY user_id" > playtime_export.json
```

### 2. 转换为 Phira-mp+ 格式

Phira-mp+ 的 `data/playtime-tracker.json` 格式：

```json
{
  "<user_id>": {
    "total_secs": 61432,
    "session_start": null
  }
}
```

转换脚本（Python）：

```python
import json

# 从 HSNPhira 导出的数据
with open('playtime_export.json') as f:
    playtime_data = json.load(f)

# 从 Phira API 获取用户信息（可选）
# 用于填充 user_name
import requests

result = {}
for entry in playtime_data:
    uid = str(entry['user_id'])
    total_secs = int(entry['total_seconds'])  # play_duration 累加值
    
    try:
        resp = requests.get(f'https://phira.5wyxi.com/users/{uid}', timeout=5)
        name = resp.json().get('name', f'user_{uid}')
    except:
        name = f'user_{uid}'
    
    result[uid] = {
        "total_secs": total_secs,
        "session_start": None,
        "user_name": name
    }

with open('playtime-tracker.json', 'w') as f:
    json.dump(result, f, indent=2)
```

### 3. 放置到 Phira-mp+ 服务器

```bash
# 停止服务器
kill $(pgrep phira-mp-plus-server)

# 备份原有数据
cp data/playtime-tracker.json data/playtime-tracker.json.bak

# 放入转换后的数据
cp path/to/converted/playtime-tracker.json data/

# 重启服务器
nohup ./target/debug/phira-mp-plus-server > log/server-output.log 2>&1 &
```

### 4. 验证迁移

连接服务器后，使用 `/welcome-config` 命令查看配置；
触发欢迎语（重新连接服务器），查看 `[playtime]` 和 `[top_playtime]` 是否正确显示。

## 数据格式参考

### Phira-mp+ playtime-tracker.json

```json
{
  "16": {
    "total_secs": 81282,
    "session_start": null
  }
}
```

| 字段 | 类型 | 说明 |
|------|------|------|
| `total_secs` | integer | 累计游玩秒数 |
| `session_start` | integer or null | 当前会话开始时间戳（Unix 秒），离线时为 null |
| `user_name` | string (可选) | 用户名，仅用于参考，代码未使用 |

### Phira-mp+ 轮次数据

轮次数据存储在 `data/rounds/` 目录下，格式为 JSON 文件，每条记录包含：

```json
{
  "room_id": "房间名",
  "chart_name": "谱面名",
  "started_at": 1712345678,
  "records": [
    {
      "user_id": 16,
      "score": 9980000,
      "perfect": 1000,
      "good": 50,
      "bad": 10,
      "miss": 5,
      "max_combo": 500,
      "accuracy": 99.5,
      "full_combo": true,
      "std_score": 980000.0
    }
  ]
}
```

## 相关命令

```bash
# 查看游玩时间
playtime <user_id>

# 查看房间历史
room history <room_id>

# 查看最后轮次
round-last <count>

# 查看欢迎语配置（含占位符列表）
welcome-config
```
