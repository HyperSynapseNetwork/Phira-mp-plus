# 配置参考

PMP 配置支持 YAML 文件、环境变量、CLI 参数三层覆盖（优先级 CLI > 环境变量 > YAML）。

## 配置文件

默认读取工作目录的 `server_config.yml`，可通过 `--config <FILE>` 指定。

## 配置文件版本

```yaml
config_version: 1
```

当前版本 1。版本不匹配时服务器会拒绝启动。

## 网络

| 字段 | 默认值 | 说明 |
|------|--------|------|
| `port` | `12346` | TCP 游戏端口 |
| `http_port` | `12347` | HTTP/SSE 内部端口 |
| `http_bind_address` | `"127.0.0.1"` | HTTP 监听地址（生产环境应保持 loopback） |
| `proxy_protocol_port` | `0` | X-Forwarded-For 兼容端口（0=禁用） |

## 认证与会话

| 字段 | 默认值 | 说明 |
|------|--------|------|
| `max_sessions` | `4096` | 最大在线会话数 |
| `max_pending_auth` | `256` | 最大并发认证数 |
| `admin_token` | `null` | 管理员 API 令牌（支持 ENV: `PM_ADMIN_TOKEN`） |
| `admin_phira_ids` | `[]` | 游戏内管理员 Phira ID 列表 |

## 数据库

| 字段 | 默认值 | 说明 |
|------|--------|------|
| `database_url` | `null` | PostgreSQL 连接串（支持 ENV: `PM_DATABASE_URL`） |
| `allow_database_degraded_mode` | `false` | 数据库初始化失败时是否允许降级启动 |
| `persistence_retention_days` | `30` | 事件数据保留天数 |
| `touch_judge_retention_days` | `null` | 遥测数据独立保留天数 |

## 插件/WASM

| 字段 | 默认值 | 说明 |
|------|--------|------|
| `plugins_dir` | `plugins` | WASM 插件目录 |
| `wasm_runtime.max_memory_mb` | `64` | 每插件最大内存 (MB) |
| `wasm_runtime.fuel_per_call` | `10000000` | 每次调用燃料上限 |
| `wasm_runtime.call_timeout_ms` | `2000` | 插件调用超时 (ms) |

## 运行时

| 字段 | 默认值 | 说明 |
|------|--------|------|
| `runtime_v2.persistence_queue_capacity` | `4096` | 持久化队列容量 |
| `runtime_v2.persistence_wal_path` | `data/persistence-worker.wal.jsonl` | WAL 文件路径 |
| `runtime_v2.persistence_dead_letter_path` | `data/persistence-dead-letter.jsonl` | Dead-letter 路径 |
| `runtime_v2.telemetry_batcher.enabled` | `true` | 遥测批处理开关 |

## Secrets 管理

敏感字段支持三种来源（优先级递降）：

1. `PM_DATABASE_URL_FILE` / `PM_ADMIN_TOKEN_FILE` — 从文件读取
2. `PM_DATABASE_URL` / `PM_ADMIN_TOKEN` — 环境变量
3. `database_url` / `admin_token` — YAML 文件

推荐生产环境使用文件或环境变量，不将 secret 写入版本控制的 YAML。
