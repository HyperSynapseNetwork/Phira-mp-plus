# 从 HSNPhira (user.py) 迁移到 Phira-mp+

本文档说明如何将 HSNPhira 后端（`user.py` + SQLite）的数据迁移到 Phira-mp+。

## 数据对比

| 数据类型 | HSNPhira (user.py) | Phira-mp+（PostgreSQL） | Phira-mp+（JSON 回退） |
|----------|-------------------|------------------------|----------------------|
| 游玩时间 | SQLite `user_playtime` + `user_room_duration` | PostgreSQL `playtime` 表 | `data/playtime-tracker.json` |
| 房间记录 | SQLite `rooms` + `user_room_activity` | PostgreSQL `room_history` 表 | 内置 `room history` 命令 |
| 游戏轮次 | SQLite `game_rounds` | 轮次数据按 UUID 文件存储 | `data/rounds/` 目录下按 UUID 存储 |
| 用户数据 | Phira API 实时查询 | 认证缓存 `data/extensions.json` | `data/extensions.json` |

**注意：** Phira-mp+ 优先使用 PostgreSQL（需在 `server_config.yml` 中配置 `database_url`）。
未配置时自动回退 JSON 文件存储。

## 迁移到 PostgreSQL

### 1. 在 Phira-mp+ 配置中启用 PostgreSQL

编辑 `server_config.yml`：

```yaml
database_url: "postgres://user:password@localhost:5432/phira_mp_plus"
```

服务器启动时会自动创建数据库和表（`playtime`、`room_history`）。

### 2. 导出 HSNPhira 数据

```bash
# 从 HSNPhira 的 SQLite 导出游玩时间
sqlite3 phira_stats.db -json "SELECT user_id, SUM(play_duration) as total_seconds FROM user_playtime GROUP BY user_id" > playtime_export.json
```

### 3. 导入到 PostgreSQL

```python
import json
import psycopg2

# 连接 PostgreSQL
conn = psycopg2.connect("postgres://user:password@localhost:5432/phira_mp_plus")
cur = conn.cursor()

# 读取导出的数据
with open('playtime_export.json') as f:
    data = json.load(f)

# 导入游玩时间
for entry in data:
    cur.execute(
        "INSERT INTO playtime (user_id, total_secs, session_start) VALUES (%s, %s, NULL) "
        "ON CONFLICT (user_id) DO UPDATE SET total_secs = EXCLUDED.total_secs",
        (entry['user_id'], int(entry['total_seconds']))
    )

conn.commit()
cur.close()
conn.close()
print("导入完成")
```

### 4. 验证

```bash
# 连接 PostgreSQL 查看数据
psql "postgres://user:password@localhost:5432/phira_mp_plus"
SELECT * FROM playtime ORDER BY total_secs DESC;
```

## 迁移到 JSON 文件（无 PostgreSQL）

### 1. 导出 HSNPhira 数据

```bash
sqlite3 phira_stats.db -json "SELECT user_id, SUM(play_duration) as total_seconds FROM user_playtime GROUP BY user_id" > playtime_export.json
```

### 2. 转换为 Phira-mp+ JSON 格式

```python
import json
import requests

with open('playtime_export.json') as f:
    playtime_data = json.load(f)

result = {}
for entry in playtime_data:
    uid = str(entry['user_id'])
    total_secs = int(entry['total_seconds'])
    result[uid] = {
        "total_secs": total_secs,
        "session_start": None
    }

with open('playtime-tracker.json', 'w') as f:
    json.dump(result, f, indent=2)
```

### 3. 放置到服务器

```bash
kill $(pgrep phira-mp-plus-server)
cp data/playtime-tracker.json data/playtime-tracker.json.bak
cp playtime-tracker.json data/
# 重启（确保 database_url 未配置，使用 JSON 回退）
nohup ./target/debug/phira-mp-plus-server > log/server-output.log 2>&1 &
```

## PostgreSQL 表结构

```sql
-- 游玩时间
CREATE TABLE playtime (
    user_id INTEGER PRIMARY KEY,
    total_secs BIGINT NOT NULL DEFAULT 0,
    session_start BIGINT
);

-- 房间访问历史
CREATE TABLE room_history (
    id SERIAL PRIMARY KEY,
    user_id INTEGER NOT NULL,
    room_id TEXT NOT NULL,
    room_uuid TEXT NOT NULL,
    joined_at BIGINT NOT NULL
);
```

## 相关命令

```bash
# 查看游玩时间
playtime <user_id>

# 查看房间历史
room history <room_id>

# 查看欢迎语配置（含占位符列表）
welcome-config
```
