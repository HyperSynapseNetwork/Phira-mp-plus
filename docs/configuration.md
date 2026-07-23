# Phira-mp+ 配置说明

本文档说明 `server_config.yml`、运行时数据文件和常见环境变量。示例配置见项目根目录的 [`server_config.yml`](../server_config.yml)。

> **编译特性**：当前默认特性为 `postgres` 和 `wit-bindgen`，常规 `cargo build --release` 已包含 PostgreSQL 与完整 WIT 插件系统。需要裁剪功能时再使用 `--no-default-features`。

> **注意**：`http_port` 是内部 HTTP/SSE/WebSocket 端口，不应直接暴露到公网。

## 配置加载规则

- 默认读取项目当前工作目录下的 `server_config.yml`。
- 可用 `--config <FILE>` 指定其他 YAML 配置文件。
- 配置文件不存在时使用内置默认值；配置文件存在但 YAML 解析失败、包含未知顶层字段或校验不通过时直接拒绝启动，避免拼写错误被静默忽略并继续使用默认安全策略。
- YAML 可以只写需要覆盖的字段，其余字段使用结构体默认值。
- 只有显式提供的命令行参数才覆盖 YAML：`--port`、`--http-port`、`--proxy-port`、`--monitor`、`--plugins-dir`、`--ext-file`、`--no-cli`。未提供的 CLI 参数不会再用其默认值覆盖 YAML。
- `config reload` 仍遵循同一优先级：显式 `--monitor` 不会被 YAML 重载覆盖；运行时或数据库维护的管理员/压测凭据在 YAML 与持久化文件均未声明时保持不变。
- `phira_api_endpoint` 在启动和重载时会去除首尾空白及末尾 `/`，并校验必须为 HTTP(S) URL；`idle.check_interval_secs` 必须大于 0，防止空闲检测形成忙循环。
- `RUST_LOG`、`NO_COLOR`、`TERM`、`STY`、`TMUX` 等环境变量只影响日志或终端显示，不会覆盖业务配置项。

## 最小可用配置

```yaml
port: 12346
http_port: 12347
max_sessions: 4096
max_pending_auth: 256
graceful_shutdown_timeout_secs: 15
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
# proxy_protocol_port: 12344  # 可信 X-Forwarded-For 兼容监听；不是 PROXY v1/v2
max_sessions: 4096
max_pending_auth: 256
graceful_shutdown_timeout_secs: 15

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
# max_users_per_room: 100
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
  event_queue_capacity: 2048
  call_timeout_ms: 2000
```

## 配置项说明

| 配置项 | 类型 | 默认值 | 说明 |
|---|---:|---:|---|
| `port` | `u16` | `12346` | TCP 游戏协议监听端口。Phira 客户端连接这个端口。 |
| `http_port` | `u16` | `12347` | PMP HTTP/SSE/WebSocket 端口 |
| `proxy_protocol_port` | `u16` | `0` | 可信转发头兼容监听端口。读取 `X-Forwarded-For`，不是 HAProxy PROXY protocol v1/v2；只能放在可信代理之后。 |
| `monitors` | `Vec<i32>` | `[2]` | 允许用 room monitor 协议旁观的 Phira 用户 ID。 |
| `phira_api_endpoint` | `String` | `https://phira.5wyxi.com` | 全局 Phira API 地址。认证默认访问它；房间未配置覆盖时，查谱面、查成绩也访问它。 |
| `plugins_dir` | `String` | `plugins` | WASM 插件目录。服务端启动时会自动创建。 |
| `extensions_file` | `String?` | `data/extensions.json` | 扩展数据持久化 JSON 路径。 |
| `cli_enabled` | `bool` | `true` | 是否启用交互式 TUI/CLI 管理控制台。`--no-cli` 会覆盖为 false。 |
| `chat_enabled` | `bool` | `true` | 是否允许聊天；可通过 `config reload` 热更新。 |
| `max_rooms` | `usize?` | 不限制 | 最大房间数。达到上限后会拒绝继续创建房间。 |
| `max_users_per_room` | `usize?` | `100` | 每个房间最大玩家数。 |
| `max_sessions` | `usize` | `4096` | 在线/已注册会话硬上限；容量名额从认证前预留到 Session 生命周期结束。 |
| `max_pending_auth` | `usize` | `256` | 并发认证握手上限；必须大于 0 且不超过 `max_sessions`。 |
| `graceful_shutdown_timeout_secs` | `u64` | `15` | 会话通知、插件事件、持久化 flush 和后台任务退出共享的总时限。 |
| `connection_rate_limit` | `u32` | `30` | 每个统计窗口内允许的连接次数。 |
| `connection_rate_window` | `u32` | `10` | 连接限速窗口，单位秒。 |
| `round_data_retention_days` | `u32` | `7` | Touches/Judges 轮次文件保留天数，`0` 表示不清理轮次文件。 |
| `database_url` | `String?` | `""` | PostgreSQL 连接串，格式 `postgres://user:password@host:port/dbname`。留空时默认尝试 `postgres://postgres:postgres@localhost:5432/phira_mp_plus`。数据库不存在时会自动创建。 |
| `persistence_retention_days` | `u32` | `30` | PostgreSQL 统一持久化历史数据保留天数，`0` 表示不自动清理。 |
| `touch_judge_retention_days` | `u32?` | 未设置 | Touches/Judges 高频遥测独立保留天数；未设置时遵循 `persistence_retention_days`，`0` 表示不自动清理遥测。 |
| `runtime_v2` | `object` | 见下文 | 持久化内部策略。用于配置 PersistenceWorker、TelemetryBatcher 和启动 cutover 模式，避免继续膨胀管理命令。 |
| `idle` | `object` | 见下文 | 空载调度提示。不得暂停或丢弃权威持久化与可靠插件事件；只允许降低非关键后台活动。 |
| `server_name` | `String?` | 未设置 | 服务器展示名称，可用于欢迎语等场景。 |
| `admin_token` | `String?` | 未设置 | 预留字段 |
| `admin_phira_ids` | `Vec<i32>` | `[]` | 游戏内管理员 Phira ID。管理员可在创建房间弹窗输入 `_命令` 执行 CLI 命令。 |
| `wasm_runtime` | `object` | 见下表 | WASM 插件运行时资源限制。 |

端口校验规则：`port`、`http_port` 和启用后的 `proxy_protocol_port` 不能冲突；设置 `proxy_protocol_port > 0` 时必须同时启用 `http_port`。`proxy_protocol_port` 只解析可信代理写入的 `X-Forwarded-For`，不实现 PROXY v1/v2。`max_rooms` 与 `max_users_per_room` 若设置，必须大于 0；`max_sessions`、`max_pending_auth` 和关闭时限也必须为正。`max_rooms` 同时约束客户端建房与管理端/WIT 创建空房。

### WASM 运行时限制

| 配置项 | 默认值 | 语义 |
|---|---:|---|
| `max_memory_mb` | `64` | 单个插件 Store 的线性内存增长上限。 |
| `fuel_per_call` | `10000000` | 每次 guest 调用重置的 fuel；PMP 拒绝 `0`，避免无计量执行。 |
| `max_stack_bytes` | `2097152` | Wasmtime guest 栈上限，最小 65536。 |
| `http_timeout_secs` | `10` | 插件出站 HTTP 超时。 |
| `max_http_response_bytes` | `2097152` | 出站 HTTP 流式读取上限。 |
| `max_file_bytes` | `4194304` | 插件文件读取/写入大小上限。 |
| `allow_private_network` | `false` | 是否允许插件访问私网地址；默认拒绝。 |
| `max_event_concurrency` | `8` | 单个有序事件内并行执行的插件数量上限；事件之间仍按队列顺序处理。 |
| `event_queue_capacity` | `2048` | 可靠低/中频插件事件队列容量。 |
| `call_timeout_ms` | `2000` | init/event/API 的墙钟期限。fuel 约束 guest CPU，但进程内宿主阻塞调用不能获得 OS 级强杀保证。 |

插件 capability 从同目录 sidecar `<plugin>.capabilities.json` 读取。缺失时只授予非特权默认能力；未知 capability 会拒绝加载。


## 运行时重载

```text
config reload
```

命令会重新读取启动时 `--config` 指定的同一文件，而不是固定读取当前目录下的 `server_config.yml`。聊天开关和 monitor 列表可立即生效；YAML 或对应持久化文件明确提供的管理员/压测凭据也会同步。显式 `--monitor` 仍保持最高优先级，未由 YAML 或持久化源声明的数据库/运行时动态列表不会被误清空。

端口、目录、数据库、连接限流以及持久化内部策略需要重启服务端。配置或相关持久化列表读取、解析、未知字段或校验失败时保留当前运行配置并返回明确错误，不会用空列表覆盖现有状态。

## 有序关闭语义

收到 Ctrl+C/SIGTERM 后，PMP 在 `graceful_shutdown_timeout_secs` 总时限内依次停止接入、摘除并关闭会话、刷新插件事件队列、执行插件 cleanup、保存扩展数据、Flush/Shutdown 持久化 Worker，并取消和等待受监督后台任务。所有步骤共享同一 deadline，避免每个子系统分别消耗完整超时。

该机制保证“进程仍正常运行且依赖可响应”条件下已接收队列项按顺序 drain，并把 Flush/Shutdown 的真实失败返回给调用方。数据库重试耗尽的事件会尝试写入 dead-letter；但它仍不是崩溃一致性协议。没有 enqueue-before WAL 时，`kill -9`、进程崩溃或主机掉电仍可能丢失尚在内存队列中的数据。

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

### 持久化内部策略（配置键 `runtime_v2`）

持久化内部策略优先放在配置文件中，避免继续新增过多管理命令。测试阶段可直接修改这些值并重启服务。

```yaml
runtime_v2:
  persistence_queue_capacity: 4096
  persistence_dead_letter_path: data/persistence-dead-letter.jsonl  # null = 禁用
  telemetry_cutover_mode: direct_only   # direct_only / worker_preferred / worker_authoritative
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

三种遥测模式的权威语义不同：

| 模式 | 权威写入 | Worker 作用 | 失败行为 |
|------|----------|-------------|----------|
| `direct_only` | RoundStore/数据库直写 | 不接收生产 Touch/Judge | 直写返回真实成功或失败；无有效数据库时使用文件回退 |
| `worker_preferred` | 优先直写 | 直写成功时为迁移镜像；直写失败且 Worker 接收时成为该批次的权威补偿路径 | Worker 镜像失败不改变已成功直写的结果；两条路径均未接受时计入 `unaccepted` 并报告 degraded |
| `worker_authoritative` | PersistenceWorker/TelemetryBatcher | 正常运行时唯一写入者 | 仅在事件尚未入队且 Worker 明确拒绝时允许直写回退；已入队后不双写 |

`worker_authoritative` 不是自动升级模式。它要求 `database_url` 非空、PostgreSQL 实际初始化成功、`telemetry_batcher.enabled=true` 且 `dry_run=false`；任一条件不满足时拒绝启动或拒绝运行时切换，不会静默降级。

`worker_preferred` 中，Worker payload 只有在直写已经确认成功时才标记为 mirror；若直写失败而 Worker 队列接收成功，同一事件 ID 由 Worker 更新权威轮次表并写入持久化明细。CLI 的 `direct_failed` 与 `path_accepted`/`unaccepted` 指标描述的是“至少一个持久化入口接收了批次”，Worker 入队并不等于数据库已经 commit，最终结果必须结合 batcher DB ACK、重试和 dead-letter 指标判断。

普通事件与遥测的数据库写入使用有限重试和稳定幂等键。重试耗尽后，能序列化的失败事件写入 `persistence_dead_letter_path` 指定的 JSONL，并执行 `flush + sync_data`。设置为 `null` 可禁用 dead-letter；此时数据库最终失败会使 Supervisor 进入 degraded。dead-letter 只保全已经完成数据库尝试的失败事件，不是 enqueue-before WAL，无法保证 `kill -9`、进程崩溃或主机掉电时内存队列零丢失，也不会自动 replay。

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

## 空载模式 (Idle Mode)

空载状态只降低非关键后台活动，不改变权威持久化、可靠插件事件或连接接入的正确性语义。当前实现不会在 idle 时卸载 HTTP、插件或 PersistenceWorker，也不会把 `suspended` 当作丢弃数据的许可。

| 配置项 | 类型 | 默认值 | 说明 |
|---|---:|---:|---|
| `idle.idle_after_secs` | `u64` | `300` | 无活动多少秒后标记为空载。 |
| `idle.check_interval_secs` | `u64` | `15` | 空载检查间隔，必须大于 0。 |
| `idle.heartbeat_timeout_secs` | `u64` | `30` | 会话心跳超时阈值。 |
| `idle.auth_timeout_secs` | `u64` | `15` | 未认证连接超时阈值。 |

## WASM 运行时限制

| 配置项 | 类型 | 默认值 | 说明 |
|---|---:|---:|---|
| `wasm_runtime.max_memory_mb` | `usize` | `64` | 单个插件线性内存上限，单位 MB。 |
| `wasm_runtime.fuel_per_call` | `u64` | `10000000` | 每次 guest 调用重置的 fuel；必须大于 0。 |
| `wasm_runtime.max_stack_bytes` | `usize` | `2097152` | WASM guest 栈上限。 |
| `wasm_runtime.http_timeout_secs` | `u64` | `10` | 插件出站 HTTP 超时。 |
| `wasm_runtime.max_http_response_bytes` | `usize` | `2097152` | 插件 HTTP 响应流式读取上限。 |
| `wasm_runtime.max_file_bytes` | `usize` | `4194304` | 插件文件读写上限。 |
| `wasm_runtime.allow_private_network` | `bool` | `false` | 是否允许插件访问私有网段；默认关闭。 |
| `wasm_runtime.max_event_concurrency` | `usize` | `8` | 单个事件内的插件并发上限；可靠事件本身不并行。 |
| `wasm_runtime.event_queue_capacity` | `usize` | `2048` | 可靠插件事件队列容量。 |
| `wasm_runtime.call_timeout_ms` | `u64` | `2000` | init/event/API 超时观测期限；超时后 quarantine，不能强杀已进入阻塞宿主函数的线程。 |

## jemalloc 内存分配器

Linux 下使用 `tikv-jemallocator` 替代 musl/glibc 默认分配器，并启用以下优化默认值：

| 选项 | 值 | 说明 |
|------|----|------|
| `background_thread` | `true` | 后台线程异步归还内存页，减少应用停顿 |
| `dirty_decay_ms` | `5000` | 脏页 5 秒未使用即归还 OS（默认 10 秒） |
| `muzzy_decay_ms` | `5000` | 模糊页 5 秒未使用即归还 OS（默认 10 秒） |

可通过 `MALLOC_CONF` 环境变量覆盖：

```bash
# 还原为 jemalloc 出厂默认值
MALLOC_CONF=background_thread:false,dirty_decay_ms:10000,muzzy_decay_ms:10000 ./phira-mp-plus-server

# 更激进：3 秒回收 + 打印统计
MALLOC_CONF=background_thread:true,dirty_decay_ms:3000,muzzy_decay_ms:3000,stats_print:true ./phira-mp-plus-server
```

## 数据文件路径

| 路径 | 说明 |
|---|---|
| `data/extensions.json` | 扩展数据持久化文件，受 `extensions_file` 影响。 |
| `data/welcome-config.json` | 欢迎语模板与占位符相关配置。 |
| `data/rounds/` | 轮次 Touches/Judges 数据。 |
| `data/plugins/<plugin>/` | 插件私有持久化文件目录。 |
| `log/` | 运行日志目录。 |
| `NOTICE` | 版权归属与第三方依赖许可证声明。 |

### 持久化 WAL（预写日志）

`runtime.persistence_wal_path` 配置 PersistenceWorker 队列准入前的本地预写日志。默认路径为 `data/persistence-worker.wal.jsonl`。

PMP 对每个被接受的事件执行：

```text
序列化准入 → 追加 WAL → flush → sync_data → 入队
```

启动时 Worker 扫描日志并重放所有没有匹配 ACK 的准入项。终态处理后追加 ACK。显式 `runtime flush` 和优雅关闭会通过原子重写仅保留未完成的准入项来压缩文件。

WAL 提供本地节点持久性，不是复制。它无法在宿主机文件系统丢失后幸存。损坏的记录会停止可信重放并将持久化子系统标记为降级；PMP 不会静默丢弃损坏后缀。

对于低频普通事件，ACK 在成功持久化、显式无数据库终态策略或成功 dead-letter 保存后发出。Touch/Judge 目前在 TelemetryBatcher 接受事件后发送 ACK；因此 WAL 尚不能证明遥测批处理的 PostgreSQL 已提交。
