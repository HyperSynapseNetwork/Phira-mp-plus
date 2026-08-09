# Phira-mp+ 部署与运维

本文档涵盖 PMP 的环境配置、`server_config.yml` 配置说明、备份/恢复、升级/回滚、容量规划与排障。配置的 JSON Schema 见 [`operations/config-schema.json`](operations/config-schema.json)。

---

## 一、环境配置

PMP 需要 **PostgreSQL**。`database_url` **必填**（留空会启动失败），也可通过环境变量 `PM_DATABASE_URL` 指定。

### 安装 PostgreSQL（Ubuntu/Debian）

```bash
sudo apt update && sudo apt install -y postgresql
sudo systemctl start postgresql
```

### 配置数据库

```bash
# database_url 必填，不能留空
sudo -u postgres psql -c "ALTER USER postgres PASSWORD 'your_password';"
sudo -u postgres createdb phira_mp_plus
```

> `database_url` **必填**（留空会启动失败）：PMP 需要 PostgreSQL 连接。
> 数据库需先创建（`createdb`），PMP 启动后自动 sqlx 迁移建表。
> 以非 postgres 用户运行时，`localhost` 走密码认证，需先设置 postgres 密码（`ALTER USER`）。

### 启动（环境变量方式）

```bash
PM_DATABASE_URL="postgres://postgres:your_password@localhost:5432/phira_mp_plus" ./phira-mp-plus-server-linux-musl
# （如需自定义其它配置，可用 --config 指定 server_config.yml）
```

配置加载优先级：**YAML < 环境变量（`PM_DATABASE_URL`）< CLI 参数**。

---

## 二、配置说明

本文档说明 `server_config.yml`、运行时数据文件和常见环境变量。示例配置见项目根目录的 [`server_config.yml`](../server_config.yml)。

> **编译特性**：当前默认特性为 `postgres` 和 `wit-bindgen`，常规 `cargo build --release` 已包含 PostgreSQL 与完整 WIT 插件系统。需要裁剪功能时再使用 `--no-default-features`。

> **注意**：`http_port` 是内部 HTTP/SSE/WebSocket 端口，不应直接暴露到公网。

### 配置加载规则

- 默认读取项目当前工作目录下的 `server_config.yml`。
- 可用 `--config <FILE>` 指定其他 YAML 配置文件。
- 配置文件不存在时使用内置默认值；配置文件存在但 YAML 解析失败、包含未知顶层字段或校验不通过时直接拒绝启动，避免拼写错误被静默忽略并继续使用默认安全策略。
- YAML 可以只写需要覆盖的字段，其余字段使用结构体默认值。
- 只有显式提供的命令行参数才覆盖 YAML：`--port`、`--http-port`、`--proxy-port`、`--monitor`、`--plugins-dir`、`--ext-file`、`--no-cli`。未提供的 CLI 参数不会再用其默认值覆盖 YAML。
- `config reload` 仍遵循同一优先级：显式 `--monitor` 不会被 YAML 重载覆盖；运行时或数据库维护的管理员/压测凭据在 YAML 与持久化文件均未声明时保持不变。
- `phira_api_endpoint` 在启动和重载时会去除首尾空白及末尾 `/`，并校验必须为 HTTP(S) URL。
- `RUST_LOG`、`NO_COLOR`、`TERM`、`STY`、`TMUX` 等环境变量只影响日志或终端显示，不会覆盖业务配置项。

### 最小可用配置

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

### 完整配置示例

```yaml
# ---- 网络 ----
port: 12346
http_port: 12347
# trusted_forwarded_http_port: 12344  # 可信 X-Forwarded-For 兼容监听；不是 PROXY v1/v2
max_sessions: 4096
max_pending_auth: 256
graceful_shutdown_timeout_secs: 15

# ---- 认证 / Phira API ----
monitors:
  - 2
phira_api_endpoint: "https://phira.5wyxi.com"

# ---- 错误监控 ----
# sentry_dsn: "https://examplePublicKey@o0.ingest.sentry.io/0"

# ---- 压测 ----
# 压测使用 Real 模式（需要 Phira token）。

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

### 配置项说明

| 配置项 | 类型 | 默认值 | 说明 |
|---|---:|---:|---|
| `port` | `u16` | `12346` | TCP 游戏协议监听端口。Phira 客户端连接这个端口。 |
| `http_port` | `u16` | `12347` | PMP HTTP/SSE/WebSocket 端口 |
| `http_bind_address` | `String` | `0.0.0.0` | HTTP/SSE/WebSocket 监听地址。默认所有接口；通过反向代理访问时建议改为 `127.0.0.1`。 |
| `monitors` | `Vec<i32>` | `[2]` | Phira 用户 ID 白名单，允许用 room monitor 协议旁观的用户。 |
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
| `log_retention_days` | `u32` | `7` | `log/` 日志保留天数（每小时轮转，mtime 判定；0 = 不清理）。PMP 日志量大，默认 7 天。 |
| `connection_rate_limit` | `u32` | `30` | 每个统计窗口内允许的连接次数。 |
| `connection_rate_window` | `u32` | `10` | 连接限速窗口，单位秒。 |
| `round_data_retention_days` | `u32` | `7` | Touches/Judges 轮次文件保留天数，`0` 表示不清理轮次文件。 |
| `database_url` | `String` | `""` | PostgreSQL 连接串，格式 `postgres://user:password@host:port/dbname`。留空时使用本地默认连接（Unix socket peer auth）。**生产环境必须填写**。数据库不存在时自动创建。 |
| `persistence_retention_days` | `u32` | `30` | PostgreSQL 统一持久化历史数据保留天数，`0` 表示不自动清理。 |
| `touch_judge_retention_days` | `u32?` | 未设置 | Touches/Judges 高频遥测独立保留天数；未设置时遵循 `persistence_retention_days`，`0` 表示不自动清理遥测。 |
| `runtime` | `object` | 见下文 | 持久化内部策略。用于配置 PersistenceWorker。 |
| `idle` | `object` | 见下文 | 空载调度提示。不得暂停或丢弃权威持久化与可靠插件事件；只允许降低非关键后台活动。 |
| `sentry_dsn` | `String?` | 未设置 | Sentry 错误监控 DSN。设置有效的 Sentry DSN 可启用自动错误和 Panic 捕获，留空或省略则不启用。 |
| `server_name` | `String?` | 未设置 | 服务器展示名称，可用于欢迎语等场景。 |
| `admin_phira_ids` | `Vec<i32>` | `[]` | 游戏内管理员 Phira ID。管理员可在创建房间弹窗输入 `_命令` 执行 CLI 命令。 |
| `filtered_player_ids` | `Vec<i32>` | `[]` | 全局过滤玩家（如测试站 Bot）：不计游玩时长/排行榜/房间历史/领域事件，只计入在线数与访问人次；自动更新空闲判定剔除；已有数据启动时自动清除。 |
| `wasm_runtime` | `object` | 见下表 | WASM 插件运行时资源限制。 |
| `auto_update` | `object` | 见下表 | 自动更新配置（默认关闭）。 |

端口校验规则：`port`、`http_port` 和启用后的 `trusted_forwarded_http_port` 不能冲突；设置 `trusted_forwarded_http_port > 0` 时必须同时启用 `http_port`。`trusted_forwarded_http_port` 只解析可信代理写入的 `X-Forwarded-For`，不实现 PROXY v1/v2。`max_rooms` 与 `max_users_per_room` 若设置，必须大于 0；`max_sessions`、`max_pending_auth` 和关闭时限也必须为正。`max_rooms` 同时约束客户端建房与管理端/WIT 创建空房。

### 自动更新（`auto_update`）

自动更新默认关闭，需显式开启。开启后服务器启动时与每隔 `check_interval_secs`
检查 GitHub 最新 Release；检测到新版本且无在线玩家超过 `min_idle_minutes` 时，
自动下载匹配当前平台的资产、替换自身可执行文件并尝试重启。

| 配置项 | 类型 | 默认值 | 说明 |
|---|---:|---:|---|
| `auto_update.enabled` | `bool` | `false` | 自动更新总开关。可通过 CLI `update auto on\|off` 运行时切换。 |
| `auto_update.check_interval_secs` | `u64` | `3600` | 检查新版本间隔（秒）。 |
| `auto_update.min_idle_minutes` | `u64` | `10` | 无在线玩家达到此分钟数才允许自动更新。 |
| `auto_update.github_repo` | `String` | `HyperSynapseNetwork/Phira-mp-plus` | 更新来源 GitHub 仓库（owner/repo）。 |

```yaml
auto_update:
  enabled: false          # 默认关，需显式开启
  check_interval_secs: 3600
  min_idle_minutes: 10
  github_repo: "HyperSynapseNetwork/Phira-mp-plus"
```

注意事项：

- 资产选择按平台精确匹配：Linux 产物统一为静态 musl（x86_64 用
  `linux-musl`，aarch64 用 `linux-arm64-musl`，纯静态、无 glibc 依赖、兼容
  任意 Linux 版本），Release 不再发布 glibc 构建；全部未命中返回明确错误、
  不下载错误资产。下载内容校验非空。
- 替换可执行文件后以相同参数 spawn 新进程接管，随后当前进程退出以释放监听
  端口（直接运行由新进程接管；systemd `Restart=on-failure` / Docker
  `restart: unless-stopped` 由服务管理器重启，二进制已替换）。新进程启动时若
  端口尚未释放，listener 绑定处有约 3 秒重试窗口保证接管。
- 更新流程在替换二进制后向 `data/update/updated-version` 写入目标版本号；
  下次启动时若与当前版本一致，会输出一次"更新完成：已更新到 vX"提示并清除
  标记（不一致则告警并清除，不重复提示）。
- 检查失败（网络/API 错误）静默降级，只记 warn，不影响启动与运行。
- 手动更新：`update check` 查看新版本；`update schedule` 预约更新（下线满
  `min_idle_minutes` 后自动执行）；`update apply` 立即更新（不满足空闲条件
  只返回原因，不预约）；`update force` 跳过在线玩家检查强制更新；
  `update cancel` 取消预约更新。

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

### 运行时重载

```text
config reload
```

命令会重新读取启动时 `--config` 指定的同一文件，而不是固定读取当前目录下的 `server_config.yml`。聊天开关和 monitor 列表可立即生效；YAML 或对应持久化文件明确提供的管理员/压测凭据也会同步。显式 `--monitor` 仍保持最高优先级，未由 YAML 或持久化源声明的数据库/运行时动态列表不会被误清空。

端口、目录、数据库、连接限流以及持久化内部策略需要重启服务端。配置或相关持久化列表读取、解析、未知字段或校验失败时保留当前运行配置并返回明确错误，不会用空列表覆盖现有状态。

### 有序关闭语义

收到 Ctrl+C/SIGTERM 后，PMP 在 `graceful_shutdown_timeout_secs` 总时限内依次停止接入、摘除并关闭会话、刷新插件事件队列、执行插件 cleanup、保存扩展数据、Flush/Shutdown 持久化 Worker，并取消和等待受监督后台任务。所有步骤共享同一 deadline，避免每个子系统分别消耗完整超时。

该机制保证“进程仍正常运行且依赖可响应”条件下已接收队列项按顺序 drain，并把 Flush/Shutdown 的真实失败返回给调用方。数据库重试耗尽的事件会尝试写入 dead-letter；但它仍不是崩溃一致性协议。没有 enqueue-before WAL 时，`kill -9`、进程崩溃或主机掉电仍可能丢失尚在内存队列中的数据。

### 统一 PostgreSQL 持久化

PMP 需要 PostgreSQL，`database_url` 留空时自动尝试连接本地 PostgreSQL（Unix socket peer auth，无需密码）。数据库不存在时自动创建。所有结构化数据统一写入 PostgreSQL。

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

### 持久化内部策略（配置键 `runtime`）

持久化内部策略优先放在配置文件中，避免继续新增过多管理命令。测试阶段可直接修改这些值并重启服务。

```yaml
runtime:
  persistence_queue_capacity: 4096
  persistence_dead_letter_path: data/persistence-dead-letter.jsonl  # null = 禁用
  phira_http:
    timeout_ms: 5000
    max_retries: 3
    base_backoff_ms: 200
    max_backoff_ms: 3000
    circuit_breaker:
      enabled: true
      failure_threshold: 8
      open_duration_ms: 20000
  high_frequency:
    channel_capacity: 4096
    max_batch_size: 256
    flush_interval_ms: 5000
    max_retries: 3
    overflow_capacity: 8192
    overflow_max_age_ms: 30000
    retry_max_age_ms: 30000
```

生产 Touch/Judge 统一通过 `HighFrequencyWriter` 写入 PostgreSQL（绕过 WAL），启用 PostgreSQL COPY 以最大化吞吐量。

普通事件与遥测的数据库写入使用有限重试和稳定幂等键。重试耗尽后，能序列化的失败事件写入 `persistence_dead_letter_path` 指定的 JSONL，并执行 `flush + sync_data`。设置为 `null` 可禁用 dead-letter；此时数据库最终失败会使 Supervisor 进入 degraded。dead-letter 只保全已经完成数据库尝试的失败事件，不是 enqueue-before WAL，无法保证 `kill -9`、进程崩溃或主机掉电时内存队列零丢失，也不会自动 replay。

`phira_http` 控制统一 Phira RetryClient。默认策略会在连续失败达到阈值后短暂打开熔断器，避免 Phira 官方服务 502/超时期间继续把认证、选谱、成绩查询压在业务热路径上。基准测试（`benchmark run`）为进程内内部调用，不走线协议，认证/选谱不经过该 client。

插件/WIT/host API 可读取：

```text
persist.events          # 参数：since_sequence, limit, kind?, room_id?, user_id?
persist.rooms           # 参数：since_sequence, limit
persist.playtime        # 参数：user_id
persist.top_playtime    # 参数：limit
persist.touches         # 参数：since_sequence?, limit?, round_uuid?, player_id?
persist.judges          # 参数：since_sequence?, limit?, round_uuid?, player_id?
```

### 房间独立 Phira API endpoint

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
room set <房间ID> host ?          # 显式设为系统房主，不被后续加入者自动接管
```

`persistent=true` 时最后一名玩家离开后房间仍保留；空房间没有房主，首个普通玩家加入时会静默成为房主，不会广播 `NewHost` 造成 `? 成为房主` 提示。

### 游戏内管理员与 `_` 命令入口

管理员 Phira ID 可写在配置文件：

```yaml
admin_phira_ids:
  - 123456
  - 234567
```

### 全局过滤玩家

不想让某些用户（如测试站 Bot）计入任何数据时，用 `filtered_player_ids`——过滤玩家**不计游玩时长/排行榜/房间历史/领域事件**，只计入在线数与服务访问人次；**自动更新空闲判定也剔除**（常驻 Bot 不会阻塞更新）；其既有数据在服务器启动时自动清除。

```yaml
filtered_player_ids:
  - 999999
  - 888888
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

### 隐藏房间配置与行为

隐藏房间不是全局配置项，而是房间状态：

- 房间名以 `-` 开头时默认隐藏。
- 可用 `room set <房间ID> hidden true|false` 修改。
- WASM/host API 可用 `room.set_hidden`、`room.is_hidden` 管理。
- 隐藏房间不会出现在房间列表（`rooms.list`，插件挂载的 `GET /api/rooms` 等端点基于此列表）、`[active_rooms]` 欢迎语占位符和房间 SSE 初始公开快照中。
- 隐藏只影响公开展示，不等于权限隔离；管理员命令和有权限插件仍可定向管理该房间。

### TUI / 终端相关配置

TUI 不使用 YAML 业务配置控制终端能力，而是根据运行环境自动判断：

- `TERM`、`STY`、`TMUX` 用于识别 GNU Screen、tmux、Linux console 等环境。
- GNU Screen、Linux console、`ansi`、`cons25` 等会进入保守模式，尽量避免备用屏幕、鼠标捕获和复杂控制序列。
- `NO_COLOR=1` 会禁用颜色。
- 非 TTY、systemd、重定向环境使用逐行控制台。
- `--no-cli` 或 `cli_enabled: false` 会完全关闭交互式管理控制台。

### 日志配置

日志文件基础名称来自启动参数 `--log-file`，默认 `phira-mp-plus`。日志等级使用 `RUST_LOG`：

```bash
RUST_LOG=info ./phira-mp-plus-server
RUST_LOG=debug ./phira-mp-plus-server
```

`RUST_LOG` 只控制日志过滤级别，不会覆盖 `server_config.yml` 的业务字段。

### 空载模式 (Idle Mode)

空载状态只降低非关键后台活动，不改变权威持久化、可靠插件事件或连接接入的正确性语义。当前实现不会在 idle 时卸载 HTTP、插件或 PersistenceWorker，也不会把 `suspended` 当作丢弃数据的许可。

| 配置项 | 类型 | 默认值 | 说明 |
|---|---:|---:|---|
| `idle.heartbeat_timeout_secs` | `u64` | `15` | 会话心跳超时阈值。 |
| `idle.auth_timeout_secs` | `u64` | `15` | 未认证连接超时阈值。 |
| `idle.dangle_grace_secs` | `u64` | `10` | 断线重连宽限时间（秒）。玩家断线后在此时长内重连可恢复。 |
| `idle.playing_reconnect_grace_secs` | `u64` | `15` | Playing 状态断线重连宽限（秒）。Playing 中断线不立即踢出房间，保留成员资格等待重连。设为 0 恢复旧行为（立即踢出）。 |

### jemalloc 内存分配器

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

### 数据文件路径

| 路径 | 说明 |
|---|---|
| `data/extensions.json` | 扩展数据持久化文件，受 `extensions_file` 影响。 |
| `welcome`（server_config.yml 段） | 欢迎语每语言配置（单文件；缺省用内置国际化，随版本更新）。 |
| `data/rounds/` | 轮次 Touches/Judges 数据。 |
| `data/plugins/<plugin>/` | 插件私有持久化文件目录。 |
| `log/` | 运行日志目录。 |
| `THIRDPARTY_LICENSES` | 版权归属与第三方依赖许可证声明。 |

### 持久化 WAL（预写日志）

`runtime.persistence_wal_path` 配置 PersistenceWorker 队列准入前的本地预写日志。默认路径为 `data/persistence-worker.wal.jsonl`。

PMP 对每个被接受的事件执行：

```text
序列化准入 → 追加 WAL → flush → sync_data → 入队
```

启动时 Worker 扫描日志并重放所有没有匹配 ACK 的准入项。终态处理后追加 ACK。显式 `runtime flush` 和优雅关闭会通过原子重写仅保留未完成的准入项来压缩文件。

WAL 提供本地节点持久性，不是复制。它无法在宿主机文件系统丢失后幸存。损坏的记录会停止可信重放并将持久化子系统标记为降级；PMP 不会静默丢弃损坏后缀。

对于低频普通事件，ACK 在成功持久化、显式无数据库终态策略或成功 dead-letter 保存后发出。生产 Touch/Judge 绕过 WAL 直接写入 PostgreSQL（通过 HighFrequencyWriter），WAL 不参与高频遥测路径。

---

## 三、运维手册

### 备份与恢复

#### 创建备份

```bash
pmp-admin backup create /path/to/backup/dir
```

备份内容：
- `data/` 目录（扩展数据、插件数据）
- `plugins/` 目录（插件文件 + 能力文件）
- `server_config.yml`

#### 验证备份

```bash
pmp-admin backup verify /path/to/backup/dir
```

#### 恢复

手动将备份文件解压到目标目录，重启 PMP。

> 注意：当前备份不含自动恢复机制。需确保目标目录配置与备份时一致。

### 启动恢复

PMP 启动时自动执行恢复流程：

1. **WAL 重放** — Worker 按 WAL sequence 顺序重放未 ACK 事件（先 WAL 后 queue）
2. **扫描未完成轮次** — 查询 `mp_rounds WHERE finished_at IS NULL`，标记为 aborted
3. **Schema 验证** — 验证 `_pmp_schema_version` 可读，失败时 not-ready
4. **WAL 健康检查** — 验证 PersistenceWorker 状态，等待 replay drained 后才 ready
5. **Playtime 会话修复** — 关闭全部残留 `session_start`（最多补偿 1h，防止停机时间计入）
6. **持久空房恢复** — 从 `mp_settings` 读取 `persistent_rooms` 列表并重建
7. **DLQ 重放** — 先 rename active DLQ 文件再读取，避免与 Worker 并发写冲突。完成后删除 replaying 文件

以上任一步骤失败时，服务进入 **not-ready** 状态（不接收客户端连接），必须人工干预。

### Playing 重连宽限

玩家在 Playing 状态断线时不会立即被踢出房间。宽限时间默认 15 秒，可通过配置：

```yaml
idle:
  playing_reconnect_grace_secs: 15  # 0 = 关闭，恢复旧行为
```

宽限期内：
- 保留房间成员资格
- 新 Session 可替换旧 Session
- timeout 后通过 Actor 执行 `remove_user` + 持久化 offline

### 持久化 admission 顺序

关键事件（UserAuthenticated、RoundCompleted、RoomSnapshot 等）的持久化顺序：

```text
WAL append/fsync → queue reservation → background worker → PostgreSQL commit → WAL ACK
```

Queue 满时使用 100ms 有界等待，超过后返回 `WalOnly` 而不是在 WAL 前丢事件。
WalOnly 事件由 WAL recovery scanner 每 5 秒重新入队，保持 WAL sequence 顺序（不插入队尾）。

#### Admission 返回语义

| 返回 | 含义 |
|------|------|
| `Queued` | WAL 已持久化，Worker 已收到通知 |
| `WalOnly` | WAL 已持久化，queue 满，scanner 会重试 |
| `RejectedBeforeWal` | 事件未进入持久化系统（极少发生，仅 WAL 文件系统错误） |

#### WAL sequence gating

Worker 维护 `next_expected_sequence` 和 `BTreeMap` 缓冲区。
来自 channel 的消息如果 sequence 不连续，会先存入缓冲区，等缺失的消息到达后再按序处理。
来自 replay 和 scanner 的消息自带 sequence，确保 WalOnly 事件不会插队到 Queued 事件之前。

非关键事件（调试 telemetry 等）允许 best-effort 丢弃。

PMP 配置支持 YAML 文件、环境变量、CLI 参数三层覆盖（优先 CLI > 环境变量 > YAML）。

#### 配置加载顺序

1. `--config <FILE>` 指定（或默认 `server_config.yml`）
2. 环境变量覆盖（如 `PMP_PORT=12346`）
3. CLI 参数覆盖（如 `--port 12346`）

#### 关键配置项

| 项 | 默认值 | 说明 |
|----|--------|------|
| `port` | `12346` | TCP 游戏端口 |
| `http_port` | `12347` | HTTP/SSE/WS 端口 |
| `max_sessions` | `4096` | 最大在线会话数 |
| `database_url` | - | PostgreSQL 连接串 |
| `persistence_retention_days` | `30` | 事件保留天数 |

完整配置说明见本文档「二、配置说明」。

### 升级与回滚

#### 升级步骤

```bash
# 1. 备份当前状态
pmp-admin backup create /tmp/pre-upgrade-backup

# 2. 替换二进制
cp phira-mp-plus-server /usr/local/bin/
systemctl restart pmp

# 3. 验证
systemctl status pmp
journalctl -u pmp -n 50
```

#### 回滚步骤

```bash
# 1. 恢复旧二进制
cp phira-mp-plus-server.bak /usr/local/bin/
systemctl restart pmp

# 2. 如需恢复数据
手动将备份解压回目标目录后重启 PMP（pmp-admin 仅提供 `backup create` / `backup verify`）
```

#### 迁移注意事项

- 数据库 migration 是版本化的，新版本会自动运行未应用的 migration
- 回滚时若已运行不可逆 migration，需手动处理
- WAL 格式向后兼容（当前版本 v1）

### 容量规划

#### 参考指标

| 场景 | 会话数 | 内存 | CPU |
|------|--------|------|-----|
| 小型部署 | ≤ 100 | 256 MB | 1 核 |
| 中型部署 | 500 | 1 GB | 2 核 |
| 大型部署 | 2000+ | 4 GB | 4 核 |

#### 关键资源

- **数据库连接池**：默认 20 连接，高并发下需增加
- **插件内存**：每个插件上限 64 MB，10 个插件用满可能 640 MB
- **文件数**：`data/` + `plugins/` + WAL 文件，通常 < 1000

### 排障指南

#### 服务器无法启动

```bash
# 检查配置（服务器交互式控制台内执行 check-config）
# 无服务器时直接检查 server_config.yml

# 检查端口占用
ss -tlnp | grep 12346

# 查看日志
journalctl -u pmp -n 100 --no-pager
```

#### 玩家无法连接

1. `systemctl status pmp` 确认服务器运行
2. 在服务器交互式控制台执行 `rooms` 查看房间列表
3. 检查防火墙端口
4. 检查认证服务可用性

#### 持久化问题

- 数据库连接失败：检查 `database_url` 和 PostgreSQL 状态
- WAL 损坏：日志会输出 WAL 错误，按提示删除 `.wal.instance`（谨慎操作）
- Dead-letter 写入失败：检查 `data/persistence-dead-letter.jsonl` 权限

#### 插件问题

```bash
plugin list          # 查看插件状态
plugin info <name>   # 查看详情和错误
plugin disable <name> # 临时禁用
plugin reload <name>  # 热重载
```

### 事故处理

#### 1. 数据库连接丢失

**症状**：PersistenceWorker 日志持续报数据库错误

**处理**：
1. 检查 PostgreSQL 状态：`systemctl status postgresql`
2. 数据库恢复后 PMP 自动重试并恢复
3. 如自动恢复失败：`systemctl restart pmp`

#### 2. WAL 损坏

**症状**：启动时 `WAL replay failed — persistence worker cannot start`

**处理**：
1. 确认所有 admission 已处理（查看日志）
2. 如有 `persistence-dead-letter.jsonl`，确认死信已处理
3. 手动移除 `.wal.instance` 标记文件
4. 重启 PMP（WAL 记录无法恢复的执行重放）

#### 3. 磁盘空间不足

**症状**：WAL admission 被拒绝，日志 `low disk space`

**处理**：
1. `df -h` 确认磁盘使用
2. 清理过期数据：调整 `persistence_retention_days`
3. 手动清理：`journalctl --vacuum-time=7d`
4. 扩展磁盘或挂载更大的数据目录

#### 4. 插件引发性能问题

**症状**：CPU 高、事件队列积压

**处理**：
1. `plugin list` 确认哪些插件活动
2. 逐个 `plugin disable` 定位问题插件
3. 检查插件日志和 `wasm_runtime` 配置
4. 降低 `fuel_per_call` 或 `max_event_concurrency`

### HighFrequency Flush/Shutdown

`HighFrequencyWriter` 用于 Touch/Judge 高频遥测（绕过 WAL，直接 PostgreSQL COPY）。

#### Sequence 跟踪

- `admission_sequence` — 下一个待分配序号（从 1 开始）
- `last_accepted_sequence` — 最后成功进入 main/overflow 队列的序号（fetch_max 并发安全）
- `committed_sequence` — 已提交的最高序号
- `continuous_committed_watermark` — 从 1 开始连续已提交的最高序号（基于 interval set 合并）

#### Flush target

Flush 使用 `last_accepted_sequence` 作为 target，避免等待不存在的序号。
Dropped 的序号进入 `dropped_range`，Flush 检测到 drop gap 时返回 `DataLoss`。

#### Shutdown

Shutdown 以 `usize::MAX` 为 limit 循环 drain overflow，确保全部 accepted item 被处理。

#### Retry

重试循环使用 `retry_max_age_ms` 作为硬截止时间（默认 30s），超时后放弃，不是固定 `max_retries` 次。
