# Phira-mp+ 配置说明

本文档说明 `server_config.yml`、运行时数据文件和常见环境变量。示例配置见项目根目录的 [`server_config.yml`](../server_config.yml)。

## 配置加载规则

- 默认读取项目当前工作目录下的 `server_config.yml`。
- 可用 `--config <FILE>` 指定其他 YAML 配置文件。
- 配置文件不存在时使用内置默认值；配置文件解析失败时会记录 warning 并回退默认值。
- 命令行参数会覆盖 YAML 中对应字段：`--port`、`--http-port`、`--proxy-port`、`--monitor`、`--plugins-dir`、`--ext-file`、`--no-cli`。
- `RUST_LOG`、`NO_COLOR`、`TERM`、`STY`、`TMUX` 等环境变量只影响日志或终端显示，不会覆盖业务配置项。

## 最小可用配置

```yaml
port: 12346
http_port: 12347
monitors:
  - 2
plugins_dir: plugins
chat_enabled: true
cli_enabled: true
connection_rate_limit: 30
connection_rate_window: 10
round_data_retention_days: 7
```

## 完整配置示例

```yaml
# ---- 网络 ----
port: 12346
http_port: 12347

# ---- 认证 / Phira API ----
monitors:
  - 2
phira_api_endpoint: "https://phira.5wyxi.com"

# ---- 压测 ----
# 默认使用 Simulation（隔离本地压测，不访问 Phira，不需要 token）。
# Real Benchmark 是显式真实网络测试，详见 docs/benchmark-real.md。

# ---- 插件 / 数据 ----
plugins_dir: plugins
extensions_file: data/extensions.json
# database_url: "postgres://user:password@localhost:5432/phira_mp_plus"
persistence_retention_days: 30
# touch_judge_retention_days: 7

# ---- 功能开关 ----
chat_enabled: true
cli_enabled: true

# ---- 容量与限速 ----
# max_rooms: 100
# max_users_per_room: 8
connection_rate_limit: 30
connection_rate_window: 10
round_data_retention_days: 7

# ---- 展示 / 管理 ----
# server_name: "My Phira Server"
# admin_token: "your-secret-token"
admin_phira_ids: []

# ---- WASM 运行时限制 ----
wasm_runtime:
  max_memory_mb: 64
  fuel_per_call: 10000000
  max_stack_bytes: 2097152
  http_timeout_secs: 10
  max_http_response_bytes: 2097152
  max_file_bytes: 4194304
  allow_private_network: false
  max_event_concurrency: 8
```

## 配置项说明

| 配置项 | 类型 | 默认值 | 说明 |
|---|---:|---:|---|
| `port` | `u16` | `12346` | TCP 游戏协议监听端口。Phira 客户端连接这个端口。 |
| `http_port` | `u16` | `12347` | HTTP API、SSE、WebSocket 监听端口。 |
| `proxy_protocol_port` | `u16` | `0` | PROXY protocol 监听端口。0=禁用。启用后 TrustForwardedFor 中间件在此端口解析 X-Forwarded-For。直连端口不走此中间件。典型值: 12344 |
| `monitors` | `Vec<i32>` | `[2]` | 允许用 room monitor 协议旁观的 Phira 用户 ID。 |
| `phira_api_endpoint` | `String` | `https://phira.5wyxi.com` | 全局 Phira API 地址。认证默认访问它；房间未配置覆盖时，查谱面、查成绩也访问它。 |
| `plugins_dir` | `String` | `plugins` | WASM 插件目录。服务端启动时会自动创建。 |
| `extensions_file` | `String?` | CLI 默认 `data/extensions.json` | 扩展数据持久化 JSON 路径。 |
| `cli_enabled` | `bool` | `true` | 是否启用交互式 TUI/CLI 管理控制台。`--no-cli` 会覆盖为 false。 |
| `chat_enabled` | `bool` | `false`（示例建议 `true`） | 是否允许聊天。未写入配置时采用结构体默认值；建议在配置中显式写出。 |
| `max_rooms` | `usize?` | 不限制 | 最大房间数。达到上限后会拒绝继续创建房间。 |
| `max_users_per_room` | `usize?` | `8` | 每个房间最大玩家数。 |
| `connection_rate_limit` | `u32` | `30` | 每个统计窗口内允许的连接次数。 |
| `connection_rate_window` | `u32` | `10` | 连接限速窗口，单位秒。 |
| `round_data_retention_days` | `u32` | `7` | Touches/Judges 轮次文件保留天数，`0` 表示不清理轮次文件。 |
| `database_url` | `String?` | 未设置 | PostgreSQL 连接串；配置后启用统一结构化持久化。未设置时保留旧 JSON/文件回退。 |
| `persistence_retention_days` | `u32` | `30` | PostgreSQL 统一持久化历史数据保留天数，`0` 表示不自动清理。 |
| `touch_judge_retention_days` | `u32?` | 未设置 | Touches/Judges 高频遥测独立保留天数；未设置时遵循 `persistence_retention_days`，`0` 表示不自动清理遥测。 |
| `runtime_v2` | `object` | 见下文 | Runtime v2 内部策略。用于配置 PersistenceWorker、TelemetryBatcher 和启动 cutover 模式，避免继续膨胀管理命令。 |
| `server_name` | `String?` | 未设置 | 服务器展示名称，可用于欢迎语等场景。 |
| `admin_token` | `String?` | 未设置 | 管理令牌预留/供管理接口或自定义扩展使用。基础公开 API 不需要配置。 |
| `admin_phira_ids` | `Vec<i32>` | `[]` | 游戏内管理员 Phira ID。管理员可在创建房间弹窗输入 `_命令` 执行 CLI 命令。 |
| `wasm_runtime` | `object` | 见下表 | WASM 插件运行时资源限制。 |

> 注意：`chat_enabled` 的 Rust 结构体默认值是 `false`，但项目示例配置显式设置为 `true`。如果希望聊天可用，请在 `server_config.yml` 里明确写 `chat_enabled: true`。



## 统一 PostgreSQL 持久化

配置 `database_url` 后，服务端会自动创建统一持久化表。所有结构化数据统一写入 PostgreSQL。

当前会保存的主要信息：

- 用户：Phira ID、用户名、语言、首次/最近出现时间、上线/下线时间。
- 房间：房间完整快照、UUID、房主、系统房主、用户/monitor 列表、锁定/循环/隐藏/持久空房、谱面、状态、房间级 Phira API endpoint。
- 事件：用户连接/断开、加入房间、房间修改、房间快照、轮次开始/结束、结算等。
- 游玩：游玩时间、用户房间历史、轮次元数据、每玩家 Touches/Judges、结算结果。
- 设置：管理员 Phira ID 等运行时可变配置。

所有会变化的数据都会记录修改时间，并尽量使用全局 `sequence` 保留事件/快照/结算的写入顺序，方便外部面板或插件增量读取。

Touches/Judges 在 PG 中采用“双层结构”：

- `mp_round_touch_batches` / `mp_round_judge_batches`：追加式明细批次表，是高频遥测的主要持久化结构；每批都有全局 `sequence`、`created_at`、`count`、`first_game_time`、`last_game_time` 和 JSONB 数据，适合外部面板增量同步。
- `mp_round_player_data`：按 `round_uuid + player_id` 聚合的持久化快照，用于 `round.data` 一次性读回完整 Touches/Judges。


保留时间由 `persistence_retention_days` 控制。Touches/Judges 属于高频遥测，可用 `touch_judge_retention_days` 单独设置；未设置时遵循全局保留时间。

```yaml
database_url: "postgres://user:password@localhost:5432/phira_mp_plus"
persistence_retention_days: 30      # 0 = 不自动清理 PG 历史数据
# touch_judge_retention_days: 7     # 未设置 = 使用 persistence_retention_days；0 = 不清理遥测
```

### Runtime v2 内部策略

Runtime v2 的内部策略优先放在配置文件中，避免继续新增过多管理命令。测试阶段可直接修改这些值并重启服务。

```yaml
runtime_v2:
  persistence_queue_capacity: 4096
  telemetry_cutover_mode: direct_only   # direct_only / worker_preferred
  telemetry_batcher:
    enabled: true
    dry_run: false
    queue_capacity: 8192
    max_items_per_batch: 256
    flush_interval_ms: 1000
  phira_http:
    timeout_ms: 5000
    max_retries: 3
    base_backoff_ms: 200
    max_backoff_ms: 3000
    circuit_breaker:
      enabled: true
      failure_threshold: 8
      open_duration_ms: 20000
```

`direct_only` 只通过 RoundStore/db.rs 直写（安全默认）。`worker_preferred` 直接写入 + 异步 Runtime v2 worker 镜像（生产推荐）。

`phira_http` 控制统一 Phira RetryClient。默认策略会在连续失败达到阈值后短暂打开熔断器，避免 Phira 官方服务 502/超时期间继续把认证、选谱、成绩查询压在业务热路径上。Simulation 默认不访问 Phira；real/hybrid benchmark 必须显式走这套 client。

插件/WIT/host API 可读取：

```text
persist.events          # 参数：since_sequence, limit, kind?, room_id?, user_id?
persist.rooms           # 参数：since_sequence, limit
persist.playtime        # 参数：user_id
persist.top_playtime    # 参数：limit
persist.touches         # 参数：since_sequence?, limit?, round_uuid?, player_id?
persist.judges          # 参数：since_sequence?, limit?, round_uuid?, player_id?
```

## 房间独立 Phira API endpoint

`server_config.yml` 中的 `phira_api_endpoint` 是全局默认值。管理员可以为某个正在运行的房间单独配置 endpoint：

```bash
room set <房间ID> phira_api_endpoint https://phira.example.com
room set <房间ID> endpoint https://phira.example.com
room set <房间ID> phira_api_endpoint default
```

规则：

- 房间覆盖值只保存在运行中的房间状态中，设置后立即生效。
- 值必须是 `http://` 或 `https://` URL；末尾 `/` 会自动去掉。
- `default` / `global` / `none` / `null` / `clear` / `默认` / `全局` / `清除` 会清除覆盖，恢复全局配置。
- 能确定房间上下文的服务端 Phira API 请求优先使用房间 endpoint，例如房间命令查谱、服务端记录校验、终端/欢迎语/Web API 展示中的谱面名和用户名刷新。
- MP 服务端不会尝试改写客户端本机 Phira API 请求行为。
- 登录认证 `/me` 仍使用全局 `phira_api_endpoint`。

WASM/host API 也支持：`room.create_empty`、`room.set_persistent_empty`、`room.set_host`、`room.clear_host`、`room.set_phira_api_endpoint`、`room.get_phira_api_endpoint`、`room.clear_phira_api_endpoint`。

无人持久房间配置不写入全局配置文件，而是运行时房间状态：

```bash
room create-empty <房间ID> [phira_api_endpoint]
room set <房间ID> persistent true
room set <房间ID> persistent false
room host <房间ID> ?              # 显式设为系统房主，不被后续加入者自动接管
room set <房间ID> host ?
```

`persistent=true` 时最后一名玩家离开后房间仍保留；空房间没有房主，首个普通玩家加入时会静默成为房主，不会广播 `NewHost` 造成 `? 成为房主` 提示。

## 压测 token 配置

Real Benchmark 是显式真实网络测试，默认不推荐。默认压测使用 Simulation（隔离 shadow world，不需要 token）。
如需使用 Real Benchmark，详见 [benchmark-real.md](benchmark-real.md)。


## 游戏内管理员与 `_` 命令入口

管理员 Phira ID 可写在配置文件：

```yaml
admin_phira_ids:
  - 123456
  - 234567
```

也可在 TUI/CLI 中维护：

```text
admin-id list
admin-id add <Phira用户ID>
admin-id remove <Phira用户ID>
admin-id set <ID1> <ID2> ...
```

WIT/host API 支持：

```text
admin.ids
admin.is_admin
admin.add_id
admin.remove_id
admin.set_ids
```

管理员在客户端”创建房间”弹窗输入 `_<CLI命令>` 时，服务端不会创建房间，而是执行对应 CLI 命令，并将输出通过聊天消息发回该客户端。非管理员输入 `_...` 会按普通房间名处理。

## 隐藏房间配置与行为

隐藏房间不是全局配置项，而是房间状态：

- 房间名以 `-` 开头时默认隐藏。
- 可用 `room hide <房间ID>` / `room unhide <房间ID>` 手动切换。
- 也可用 `room set <房间ID> hidden true|false` 修改。
- WASM/host API 可用 `room.set_hidden`、`room.is_hidden` 管理。
- 隐藏房间不会出现在 `GET /api/rooms`、`GET /api/rooms/<name>`、`[active_rooms]` 欢迎语占位符和房间 SSE 初始公开快照中。
- 隐藏只影响公开展示，不等于权限隔离；管理员命令和有权限插件仍可定向管理该房间。

## TUI / 终端相关配置

TUI 不使用 YAML 业务配置控制终端能力，而是根据运行环境自动判断：

- `TERM`、`STY`、`TMUX` 用于识别 GNU Screen、tmux、Linux console 等环境。
- GNU Screen、Linux console、`ansi`、`cons25` 等会进入保守模式，尽量避免备用屏幕、鼠标捕获和复杂控制序列。
- `NO_COLOR=1` 会禁用颜色。
- 非 TTY、systemd、重定向环境使用逐行控制台。
- `--no-cli` 或 `cli_enabled: false` 会完全关闭交互式管理控制台。

## 日志配置

日志文件基础名称来自启动参数 `--log-file`，默认 `phira-mp-plus`。日志等级使用 `RUST_LOG`：

```bash
RUST_LOG=info ./phira-mp-plus-server
RUST_LOG=debug ./phira-mp-plus-server
```

`RUST_LOG` 只控制日志过滤级别，不会覆盖 `server_config.yml` 的业务字段。

## WASM 运行时限制

| 配置项 | 类型 | 默认值 | 说明 |
|---|---:|---:|---|
| `wasm_runtime.max_memory_mb` | `usize` | `64` | 单个插件线性内存上限，单位 MB。 |
| `wasm_runtime.fuel_per_call` | `u64` | `10000000` | 每次 guest 调用补充的 fuel；`0` 表示关闭 fuel 计量。 |
| `wasm_runtime.max_stack_bytes` | `usize` | `2097152` | WASM 调用栈上限。 |
| `wasm_runtime.http_timeout_secs` | `u64` | `10` | 插件 `http.get/post` 超时时间。 |
| `wasm_runtime.max_http_response_bytes` | `usize` | `2097152` | 插件 HTTP 响应最大读取字节数。 |
| `wasm_runtime.max_file_bytes` | `usize` | `4194304` | 插件文件读写最大字节数。 |
| `wasm_runtime.allow_private_network` | `bool` | `false` | 是否允许插件访问私有网段地址；默认关闭以降低 SSRF 风险。 |
| `wasm_runtime.max_event_concurrency` | `usize` | `8` | 插件事件处理最大并发数。 |

## 数据文件路径

| 路径 | 说明 |
|---|---|
| `data/extensions.json` | 扩展数据持久化文件，受 `extensions_file` 影响。 |
| `data/welcome-config.json` | 欢迎语模板与占位符相关配置。 |
| `data/rounds/` | 轮次 Touches/Judges 数据。 |
| `data/plugins/<plugin>/` | 插件私有持久化文件目录。 |
| `log/` | 运行日志目录。 |
