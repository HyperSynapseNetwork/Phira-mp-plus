# Phira-mp+ 配置说明

本文档说明 `server_config.yml`、运行时数据文件和常见环境变量。示例配置见项目根目录的 [`server_config.yml`](../server_config.yml)。

## 配置加载规则

- 默认读取项目当前工作目录下的 `server_config.yml`。
- 可用 `--config <FILE>` 指定其他 YAML 配置文件。
- 配置文件不存在时使用内置默认值；配置文件解析失败时会记录 warning 并回退默认值。
- 命令行参数会覆盖 YAML 中对应字段：`--port`、`--http-port`、`--monitor`、`--plugins-dir`、`--ext-file`、`--no-cli`。
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

# ---- 压测账号 ----
# 推荐多账号写法。每个 token 会作为一个真实 TCP 客户端账号使用。
benchmark_phira_tokens:
  - "token-for-account-1"
  - "token-for-account-2"

# 兼容单账号写法。已配置 benchmark_phira_tokens 时通常不需要它。
# benchmark_phira_token: "token-for-one-account"

# ---- 插件 / 数据 ----
plugins_dir: plugins
extensions_file: data/extensions.json
# database_url: "postgres://user:password@localhost:5432/phira_mp_plus"

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
| `round_data_retention_days` | `u32` | `7` | Touches/Judges 轮次数据保留天数，`0` 表示不保留。 |
| `database_url` | `String?` | 未设置 | PostgreSQL 连接串；未设置时回退 JSON/文件存储。 |
| `server_name` | `String?` | 未设置 | 服务器展示名称，可用于欢迎语等场景。 |
| `admin_token` | `String?` | 未设置 | 管理令牌预留/供管理接口或自定义扩展使用。基础公开 API 不需要配置。 |
| `benchmark_phira_tokens` | `Vec<String>` | `[]` | 真实网络压测使用的 Phira 账号 token 列表。 |
| `benchmark_phira_token` | `String?` | 未设置 | 单账号兼容写法，会并入压测 token 列表。 |
| `wasm_runtime` | `object` | 见下表 | WASM 插件运行时资源限制。 |

> 注意：`chat_enabled` 的 Rust 结构体默认值是 `false`，但项目示例配置显式设置为 `true`。如果希望聊天可用，请在 `server_config.yml` 里明确写 `chat_enabled: true`。


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
- 能确定房间上下文的 Phira API 请求优先使用房间 endpoint，例如客户端选谱时服务端查 `/chart/<id>`、提交成绩时服务端校验 `/record/<id>`、服务端命令 `room set <id> chart-id <谱面ID>`，以及终端/Web API/欢迎语展示谱面名和用户名。
- 登录认证 `/me` 发生在用户加入任何房间之前，没有房间上下文，因此仍使用全局 `phira_api_endpoint`。

WASM/host API 也支持：`room.set_phira_api_endpoint`、`room.get_phira_api_endpoint`、`room.clear_phira_api_endpoint`。房间 endpoint 只影响服务端侧查询和展示；认证 `/me` 保持全局 endpoint，服务端不会改写客户端本机 Phira API 请求行为。

## 压测 token 配置

`benchmark` 是真实 TCP 网络压测，不再直接改内存状态。它会使用配置的 Phira token 连接本机 TCP 服务端、认证并创建 `bench-*` 房间。

### 方式一：写入 `server_config.yml`

```yaml
benchmark_phira_tokens:
  - "token-for-account-1"
  - "token-for-account-2"
```

单账号也可以写：

```yaml
benchmark_phira_token: "token-for-one-account"
```

### 方式二：用 CLI 绑定

```text
benchmark-bind token1,token2,token3
```

该命令会写入 `data/benchmark-auth.json`：

```json
{
  "token": null,
  "tokens": ["token1", "token2", "token3"]
}
```

### 读取优先级

1. `server_config.yml` 的 `benchmark_phira_tokens` 和 `benchmark_phira_token`。
2. 如果配置文件里没有任何压测 token，才读取 `data/benchmark-auth.json`。
3. 两处都没有 token 时，执行 `benchmark` 会提示先运行 `benchmark-bind` 或修改配置文件。

### 多账号与房间数

每个压测房间至少需要一个可认证的 Phira token。请求 `benchmark 30 100` 但只配置 3 个 token 时，服务端会按 3 个账号创建 3 个压测房间，并提示账号不足。要压更多房间，请配置更多不同账号 token。


## 无人持久房间

管理员可以创建没有初始房主和玩家的空房间，并让它在无人在线时继续保留：

```bash
room create <房间ID> [phira_api_endpoint|default] [hidden]
room keep <房间ID> true
room set <房间ID> persistent true
```

这类房间的房主初始为空；第一个普通玩家加入时会自动成为房主。最后一名普通玩家离开时，如果 `persistent=true`，房间不会被清理，房主会重新变为空，等待下一位普通玩家加入。WASM/host API 对应 `room.create_empty` 与 `room.set_persistent`。

## 隐藏房间配置与行为

隐藏房间不是全局配置项，而是房间状态：

- 房间名以 `-` 开头时默认隐藏；兼容旧写法 `+-`。
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
| `data/benchmark-auth.json` | `benchmark-bind` 写入的压测 token 文件。配置文件已有 token 时不会读取它。 |
| `data/welcome-config.json` | 欢迎语模板与占位符相关配置。 |
| `data/rounds/` | 轮次 Touches/Judges 数据。 |
| `data/plugins/<plugin>/` | 插件私有持久化文件目录。 |
| `log/` | 运行日志目录。 |
