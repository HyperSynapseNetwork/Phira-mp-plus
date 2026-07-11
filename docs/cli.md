# Phira-mp+ CLI 命令文档

## 启动参数

```
phira-mp-plus-server [OPTIONS]

  -p, --port <PORT>          覆盖 TCP 监听端口（内置默认 12346）
  -d, --plugins-dir <DIR>    覆盖 WASM 插件目录（内置默认 "plugins"）
  -e, --ext-file <FILE>      覆盖扩展数据文件（内置默认 "data/extensions.json"）
      --no-cli               禁用交互式 CLI 管理控制台
  -l, --log-file <NAME>      日志文件基础名称 [默认: "phira-mp-plus"]
  -m, --monitor <IDS>...     覆盖允许旁观的用户 ID
      --http-port <PORT>     覆盖 HTTP/SSE 端口（内置默认 12347）
      --proxy-port <PORT>    覆盖可信转发头兼容端口（不是 PROXY v1/v2；内置默认 0）
  -c, --config <FILE>        YAML 配置文件路径 [默认: "server_config.yml"]
  -h, --help                 显示帮助信息
  -V, --version              显示版本号
```

配置加载规则：默认读取 `server_config.yml`，也可通过 `--config <FILE>` 指定；只有显式提供的命令行参数才覆盖 YAML。配置文件存在但解析或校验失败时服务端拒绝启动。`RUST_LOG`、`NO_COLOR` 等环境变量只影响日志或终端显示。完整配置说明见 [configuration.md](configuration.md)。

## 交互式管理控制台

服务器在普通交互式终端和 tmux 中启动 ratatui 管理控制台。GNU Screen、Linux console、`ansi`/`cons25` 等兼容性较差的终端会进入保守 TUI：不启用备用屏幕、鼠标捕获或 Bracketed Paste，并修正 Ctrl+H Backspace；如果 TUI 初始化失败，会自动回落到逐行兼容控制台。重定向、systemd 和其他非 TTY 环境始终使用逐行控制台。设置 `NO_COLOR` 可关闭颜色。

TUI 快捷键：`Tab` 补全、`Ctrl+A/E` 跳到行首/行尾、`Ctrl+B/F` 左右移动、`Alt+←/→` 按词移动、`Ctrl+W` 删除前一个词、`Alt+Delete` 删除后一个词、`Ctrl+K` 删除到行尾、`Ctrl+L` 清屏、`PgUp/PgDn` 或 `Shift+↑/↓` 滚动日志。

---

## 命令约定

- `<必填参数>` — 尖括号表示必须提供
- `[可选参数]` — 方括号表示可选
- `[默认值]` — 方括号内等号表示默认值
- 级别说明：**Primary** 基础管理（`help` 默认显示）| **Advanced** 高级操作 | **Developer** 开发诊断

---

## 通用

### `help [command|all|advanced|dev]`

查看命令帮助。

| 参数 | 类型 | 说明 |
|------|------|------|
| `command` | `str` (可选) | 要查看详情的命令名，如 `help room close` |
| `all` | 字面量 | 显示所有命令 |
| `advanced` | 字面量 | 显示高级命令 |
| `dev` | 字面量 | 显示开发命令 |

**输出:** 命令清单或指定命令的详细说明（用法、参数、别名、示例）

**示例:**
```
help
help room close
help all
help groups
```

### `exit`

关闭服务器。

**输出:** 终止进程（无输出）

---

### `status`

查看服务器运行状态。

**输出:**
```
  ◆ Phira-mp+ v0.2.0  │ 端口 12346  │ 房间 5
```

| 字段 | 说明 |
|------|------|
| 版本号 | cargo 包版本 |
| 端口 | 配置的监听端口 |
| 房间数 | 当前活跃房间数 |

---

## 房间

### `rooms`

查看活跃房间列表。

**输出:** 每行一个房间，格式：
```
  <room_id>  │ 用户 N │ <InternalRoomState> │ 谱面 <chart_id>
```

| 字段 | 说明 |
|------|------|
| room_id | 房间名称 |
| 用户数 | 房间内玩家数 |
| 状态 | 房间状态 (Wait/Playing/SelectChart) |
| 谱面 | 当前谱面 ID |

---

### `room info <room_id>`

查看房间详情。

| 参数 | 类型 | 说明 |
|------|------|------|
| `room_id` | `str` | 房间名 |

**输出:** 房间全部信息：状态、用户列表、房主、封禁列表、配置等。

---

### `room create-empty <room_id> [phira_api_endpoint]`

创建无人持久空房间。

| 参数 | 类型 | 说明 |
|------|------|------|
| `room_id` | `str` | 房间名 |
| `phira_api_endpoint` | `str` (可选) | 可选 Phira API endpoint 覆盖 |

**输出:** `创建成功` 或 `房间 <room_id> 已存在`

---

### `room close <room_id>`

解散房间。

| 参数 | 类型 | 说明 |
|------|------|------|
| `room_id` | `str` | 房间名 |

**输出:** 成功消息或错误

---

### `room host <room_id> <user_id|?>`

设置房主。

| 参数 | 类型 | 说明 |
|------|------|------|
| `room_id` | `str` | 房间名 |
| `user_id` | `int` / `?` | 用户 ID，`?` 表示系统房主 |

**输出:** `房主已转移给 <user_name>` 或 `房主已设为系统 ?`

---

### `room set <room_id> <field> <value>`

修改房间设置。

| 参数 | 类型 | 说明 |
|------|------|------|
| `room_id` | `str` | 房间名 |
| `field` | `str` | 支持字段：`lock` `cycle` `hidden` `persistent` `host` `chart-id` `phira_api_endpoint` |
| `value` | 因字段而异 | `lock`/`cycle`/`hidden` 接受 `true`/`false`，`host` 接受用户 ID 或 `?` |

**输出:** 执行结果消息

---

### `room start <room_id>`

服务端强制发起房间游戏；客户端完成谱面加载并准备后正式开始。兼容旧命令 `force-start <room_id>`，也可使用 `room force-start <room_id>`。

| 参数 | 类型 | 说明 |
|------|------|------|
| `room_id` | `str` | 房间名 |

**输出:** 游戏开始状态

---

### `room cancel <room_id>`

取消管理员发起的游戏开始。

| 参数 | 类型 | 说明 |
|------|------|------|
| `room_id` | `str` | 房间名 |

**输出:** 取消结果

---

### `room kick <room_id> <user_id>`

从房间踢出用户。

| 参数 | 类型 | 说明 |
|------|------|------|
| `room_id` | `str` | 房间名 |
| `user_id` | `int` | 目标用户 ID |

**输出:** 踢出结果。目标客户端会立即收到 `LeaveRoom(Ok)` 并退出本地房间状态，不再等待超时重连。

---

### `room hide <room_id> [true|false]`

隐藏房间。隐藏的房间不在 Web API / 欢迎语中显示。

| 参数 | 类型 | 说明 |
|------|------|------|
| `room_id` | `str` | 房间名 |
| 值 | `bool` (可选, 默认 `true`) | 隐藏或取消隐藏 |

**输出:** 结果消息

---

### `room unhide <room_id>`

取消隐藏房间。

| 参数 | 类型 | 说明 |
|------|------|------|
| `room_id` | `str` | 房间名 |

**输出:** 结果消息

---

### `room force-move <room_id> <user_id> [monitor]`

强制迁移用户到指定房间。原房间 ID 会被替换，此操作不可逆。

| 参数 | 类型 | 说明 |
|------|------|------|
| `room_id` | `str` | 目标房间名 |
| `user_id` | `int` | 用户 ID |
| `monitor` | `str` (可选) | 指定为 monitor 时用户以旁观者加入 |

**输出:** 迁移结果

---

### `room history <room_id>`

查看房间游玩历史。

| 参数 | 类型 | 说明 |
|------|------|------|
| `room_id` | `str` | 房间名 |

**输出:** 历史轮次列表，含谱面 ID 和各玩家成绩

---

### `room rounds <room_id>`

查看房间轮次列表。

| 参数 | 类型 | 说明 |
|------|------|------|
| `room_id` | `str` | 房间名 |

**输出:** 轮次 UUID 列表

---

### `room round <round_uuid>`

查看指定轮次详情。

| 参数 | 类型 | 说明 |
|------|------|------|
| `round_uuid` | `uuid` | 轮次 UUID |

**输出:** 该轮次的完整数据（谱面、所有玩家提交的成绩）

---

### `room uuid <room_id>`

查看房间 UUID。

| 参数 | 类型 | 说明 |
|------|------|------|
| `room_id` | `str` | 房间名 |

**输出:** 房间唯一标识 UUID

---

### `room ban <room_id> <user_id> [reason]`

将用户加入房间黑名单。

| 参数 | 类型 | 说明 |
|------|------|------|
| `room_id` | `str` | 房间名 |
| `user_id` | `int` | 用户 ID |
| `reason` | `str` | 可选封禁原因 |

**输出:** 封禁结果。若目标当前仍在房间，服务端先向其显示封禁原因，再立即发送离房响应并移出房间。

---

### `room unban <room_id> <user_id>`

将用户移出房间黑名单。

| 参数 | 类型 | 说明 |
|------|------|------|
| `room_id` | `str` | 房间名 |
| `user_id` | `int` | 用户 ID |

**输出:** `已解除封禁` 或 `未找到封禁记录`

---

### `room banlist <room_id>`

查看房间黑名单。

| 参数 | 类型 | 说明 |
|------|------|------|
| `room_id` | `str` | 房间名 |

**输出:** 黑名单用户列表（含用户 ID 和原因）

---

## 用户

### `users`

查看在线用户列表。

**输出:** 每行一个用户，格式：
```
  <user_id>  │ <user_name>  │ IP <addr>  │ 在线 N  │ 房间 <room_id> │ [host] [monitor]
```

| 字段 | 说明 |
|------|------|
| user_id | 用户数字 ID |
| user_name | 用户名 |
| IP | 连接 IP 地址 |
| 在线时长 | 已连接时间 |
| 房间 | 当前所在房间 |
| host/monitor | 身份标签 |

---

### `kick <user_id>`

踢出在线用户。

| 参数 | 类型 | 说明 |
|------|------|------|
| `user_id` | `int` | 用户 ID |

**输出:** `Kicked user <user_id> from server`

---

### `ban <user_id> [reason]`

全局封禁用户。

| 参数 | 类型 | 说明 |
|------|------|------|
| `user_id` | `int` | 用户 ID |
| `reason` | `str` (可选) | 封禁原因 |

**输出:** 封禁确认消息

---

### `unban <user_id>`

取消全局封禁。

| 参数 | 类型 | 说明 |
|------|------|------|
| `user_id` | `int` | 用户 ID |

**输出:** 解封确认消息

---

### `banlist`

查看全局封禁列表。

**输出:** 被封禁的用户 ID 及其原因列表

---

## 广播

### `broadcast all <message>`

向所有已连接用户发送消息。

| 参数 | 类型 | 说明 |
|------|------|------|
| `message` | `str` | 消息文本 |

**输出:** `Sent to N users`

---

### `broadcast room <room_id> <message>`

向指定房间内所有用户发送消息。

| 参数 | 类型 | 说明 |
|------|------|------|
| `room_id` | `str` | 房间名 |
| `message` | `str` | 消息文本 |

**输出:** `Sent to room: N users`

---

### `broadcast user <user_id> <message>`

向指定用户发送私信。

| 参数 | 类型 | 说明 |
|------|------|------|
| `user_id` | `int` | 目标用户 ID |
| `message` | `str` | 消息文本 |

**输出:** `Sent direct message`

---

## 管理员 ID

### `admin-id list`

查看管理员 Phira ID 列表。

**输出:** 管理员 ID 列表

---

### `admin-id add <PhiraID>`

添加管理员。

| 参数 | 类型 | 说明 |
|------|------|------|
| `PhiraID` | `int` | Phira 用户 ID |

**输出:** `Added admin <PhiraID>`

---

### `admin-id remove <PhiraID>`

移除管理员。

| 参数 | 类型 | 说明 |
|------|------|------|
| `PhiraID` | `int` | Phira 用户 ID |

**输出:** `Removed admin <PhiraID>`

---

### `admin-id set <PhiraID...>`

替换整个管理员列表。

| 参数 | 类型 | 说明 |
|------|------|------|
| `PhiraID...` | `int...` | 一个或多个 Phira 用户 ID，空格分隔 |

**输出:** `Set admin IDs: [N]`

---

## 插件

### `plugin list`

列出所有已加载插件。

**输出:** 插件列表，每行格式：
```
  <name>  v<version>  │ <author>  │ <enabled|disabled>
```

---

### `plugin enable <name>`

启用插件。

| 参数 | 类型 | 说明 |
|------|------|------|
| `name` | `str` | 插件名 |

**输出:** `Enabled plugin <name>`

---

### `plugin disable <name>`

禁用插件。

| 参数 | 类型 | 说明 |
|------|------|------|
| `name` | `str` | 插件名 |

**输出:** `Disabled plugin <name>`

---

### `plugin reload`

重新加载所有插件。

**输出:** 重载结果

---

### `plugin info <id_or_name>`

查看插件详情。

| 参数 | 类型 | 说明 |
|------|------|------|
| `id_or_name` | `str` | 插件 ID 或名称 |

**输出:** 插件详细信息（名称、版本、作者、描述、能力集、注册路由）

---

### `plugin call <id_or_name> <method> [JSON_ARRAY]`

调用插件导出 API。

| 参数 | 类型 | 说明 |
|------|------|------|
| `id_or_name` | `str` | 插件 ID 或名称 |
| `method` | `str` | API 方法名 |
| `JSON_ARRAY` | `json` (可选) | JSON 数组格式参数 |

**输出:** API 调用返回的 JSON

---

### `plugin remove <name>`

卸载插件。

| 参数 | 类型 | 说明 |
|------|------|------|
| `name` | `str` | 插件名 |

**输出:** `Removed plugin <name>` 或错误信息

---

## 基准测试

### `benchmark [seconds] [rooms]`

运行真实 TCP 网络压测（需 Phira token）。

| 参数 | 类型 | 说明 |
|------|------|------|
| `seconds` | `int` (可选, 默认 30) | 测试时长 |
| `rooms` | `int` (可选, 默认 8) | 房间数 |

**输出:** Benchmark 报告

---

### `benchmark modes`

查看三种压测模式说明。

**输出:** Simulation / Hybrid / Real 三种模式说明

---

### `benchmark run real [seconds] [rooms]`

显式真实 TCP 协议测试。

| 参数 | 类型 | 说明 |
|------|------|------|
| `seconds` | `int` (可选, 默认 30) | 测试时长 |
| `rooms` | `int` (可选, 默认 8) | 房间数 |

**输出:** Benchmark 报告（含 probe_stats、failure_samples、elapsed）

---

### `benchmark run hybrid [duration=N] [authenticate=true] [chart_lookup=<id>] [record_lookup=<id>]`

运行混合 Phira 探测。

| 参数 | 类型 | 说明 |
|------|------|------|
| `duration` | `int` (可选, 默认 10) | 测试秒数 |
| `authenticate` | `bool` (可选, 默认 true) | 是否执行认证探测 |
| `chart_lookup` | `int` (可选) | 谱面查询 ID |
| `record_lookup` | `int` (可选) | 成绩查询 ID |

**输出:** Hybrid 探测报告

---

### `benchmark report [simulation|hybrid|real|limit]`

查看最新 Benchmark 报告。

| 参数 | 类型 | 说明 |
|------|------|------|
| `mode` | `str` (可选) | 模式过滤，如 `report simulation` |
| `limit` | `int` (可选) | 显示最近 N 条 |

**输出:** 报告摘要（模式、标题、失败操作数、耗时）

---

### `benchmark history [simulation|hybrid|real] [limit]`

查看持久化的 Benchmark 历史记录。

| 参数 | 类型 | 说明 |
|------|------|------|
| `mode` | `str` (可选) | 模式过滤 |
| `limit` | `int` (可选, 默认 10) | 返回条数上限 |

**输出:** 历史报告列表

---

## 模拟器

### `simulation status`

查看模拟器状态。

**输出:**
```
  Running: <true|false>  │ Users: N  │ Rooms: N  │ Seed: N  │ Scenario: <name>
```
| 字段 | 说明 |
|------|------|
| Running | 是否正在运行 |
| Users/Rooms | 虚拟用户/房间数 |
| Seed | 随机种子 |
| Scenario | 当前场景 |
| Started/Elapsed | 启动时间/已运行时间 |

---

### `simulation run <preset> [key=value...]`

启动隔离本地压测。自动 tick，到达 duration 后自动停止。

| 参数 | 类型 | 说明 |
|------|------|------|
| `preset` | `str` | 预置：`baseline` `small` `medium` `large` `custom` |
| `scenario` | `str` (可选) | 场景：`balanced` `chat_storm` `ready_storm` `round_storm` `touch_judge_burst` `idle` |
| `users` | `int` (可选) | 虚拟用户数 |
| `rooms` | `int` (可选) | 虚拟房间数 |
| `duration` | `int` (可选) | 运行秒数 |
| `tick_ms` | `int` (可选) | tick 间隔毫秒 |
| `auto` | `bool` (可选) | 自动 tick |
| `persist_every` | `int` (可选) | 每 N tick 持久化一次 |
| `touch/judge/chat/ready/rounds` | `bool` (可选) | 活动开关 |

**输出:** Simulation 状态，含运行 ID 和配置

---

### `simulation stop`

停止当前 Simulation 运行。

**输出:** Simulation 状态（已停止）

---

### `simulation cleanup`

清理 Simulation 数据（shadow world）。

**输出:** `simulation shadow world cleaned; real rooms/users were not touched`

---

### `simulation scenarios`

查看可用场景列表。

**输出:** 每个场景的名称、描述和参数说明

---

### `simulation suite <name> [key=value...]`

运行场景序列。

| 参数 | 类型 | 说明 |
|------|------|------|
| `name` | `str` | `smoke` `mixed` `stress` |
| `duration` | `int` (可选) | 每步秒数 |
| `tick_ms` | `int` (可选) | tick 间隔 |
| `persist_every` | `int` (可选) | 持久化频率 |
| `users/rooms` | `int` (可选) | 大小覆盖 |

**输出:** Suite 汇总报告（统一 BenchmarkReport 格式）

---

### `simulation report [latest|list|clear]`

查看模拟报告。

| 参数 | 类型 | 说明 |
|------|------|------|
| 子命令 | `str` (可选) | `latest` 最新、`list` 列表、`clear` 清除 |

**输出:** Simulation 运行报告或报告列表

---

### `simulation tick [count]`

手动推进 Simulation tick。仅 developer。

| 参数 | 类型 | 说明 |
|------|------|------|
| `count` | `int` (可选, 默认 1) | tick 次数 |

**输出:** Simulation 状态

---

### `simulation inspect [limit]`

查看影子世界数据。仅 developer。

| 参数 | 类型 | 说明 |
|------|------|------|
| `limit` | `int` (可选, 默认 5) | 样本条数 |

**输出:** VirtualUser / VirtualRoom / VirtualRound 样本

---

### `simulation seed <value>`

设置确定性随机种子。仅 developer。

| 参数 | 类型 | 说明 |
|------|------|------|
| `value` | `u64` | 种子值 |

**输出:** `simulation seed updated to <value>`

---

### `simulation persist`

发送 Simulation 快照到持久化 Worker。仅 developer。

**输出:** 持久化队列状态

---

### `simulation sample`

查看 deterministic touches/judges 示例数据规模。仅 developer。

**输出:** `Touches: N  │ Judges: N`

---

## 运行时诊断

### `runtime status`

服务器运行时诊断总览。

**输出:**
```
  Runtime v2: <rooms> rooms | <N> commands | MIGRATION_PHASE=<phase> (WIT component ABI)
```

---

### `runtime cutover [direct_only|worker_preferred|worker_authoritative]`

查看或切换 Touch/Judge 持久化 Cutover 模式。

| 参数 | 类型 | 说明 |
|------|------|------|
| `mode` | `str` (可选) | `direct_only` 仅直写；`worker_preferred` 优先直写，直写成功后 Worker 为镜像，直写失败时 Worker 可接管该批次；`worker_authoritative` 由 Worker 正常运行时单写，启用前强制检查数据库与 batcher |

**输出:** 当前 cutover 模式和说明

---

### `runtime persistence`

查看持久化 Worker 和遥测批处理器统计。

**输出:** Worker 队列与 pending、数据库确认/失败、dead-letter 路径与成功/失败计数、遥测 cutover、`direct_failed`、`worker_canonical_fallback`、`unaccepted`、batcher 批次与延迟、per-kind 统计和最近 trace 条目。`worker_enqueued/path_accepted` 仅表示管线接收，不表示 PostgreSQL 已 commit。

---

### `runtime phira`

查看 Phira HTTP RetryClient 统计和策略。仅 developer。

**输出:** 请求/成功/重试/失败计数、circuit breaker 状态、endpoint 级统计

---

### `runtime commands`

查看命令注册表统计。仅 developer。

**输出:** `Registry: N primary, N advanced, N dev`

---

### `runtime events`

查看事件总线统计与最近事件。仅 developer。

**输出:** EventBus 统计（总事件数、trace 窗口大小、订阅者数）

---

### `runtime schema`

查看持久化 Schema 信息。仅 developer。

**输出:** Schema 版本号、telemetry 表说明

---

### `runtime rooms`

查看房间命令通道与 Actor 迁移状态。仅 developer。

**输出:** 房间 mailbox 命中/缺失/关闭/拥塞/不确定结果统计和已注册房间通道；fallback 字段仅为旧诊断结构兼容，不代表仍存在 inline 执行路径

---

### `runtime actors`

查看 Actor 模型迁移蓝图。仅 developer。

**输出:** 每个 Actor 边界的名称、职责、状态和下一步

---

### `config reload`

重新读取启动时 `--config` 指定的 YAML 文件。热更新 `chat_enabled` 和 `monitors`；YAML 中显式声明的管理员/压测凭据也会同步更新。显式 `--monitor` 始终保持高于 YAML 的优先级；YAML 与持久化文件都未声明的动态管理员/凭据状态不会被重载误清空。端口、目录、数据库、限流和 Runtime v2 策略仍需重启。

**输出:** 实际读取路径、已热更新字段及错误信息。配置失败或运行时锁繁忙时保留现有运行配置。

---

## 扩展字段

### `extension list`

查看已注册扩展字段。

**输出:** 已注册的用户扩展字段列表（名称、默认值、注册者、描述）

---

### `extension get <target> <key>`

获取扩展数据。

| 参数 | 类型 | 说明 |
|------|------|------|
| `target` | `str` | `user:<id>` 或 `room:<id>` 格式 |
| `key` | `str` | 字段键名 |

**输出:** 扩展数据的 JSON 值，或 `Field not found`

扩展字段命令只提供查看能力；写入由服务端内部逻辑、WIT/host API 或插件完成。

---

## 实用工具

### `player-count`

查看游玩过的玩家总数。

**输出:** `◆ 玩家总数: N`

---

### `playtime <user_id>`

查询指定用户的游玩时间。

| 参数 | 类型 | 说明 |
|------|------|------|
| `user_id` | `int` | 用户 Phira ID |

**输出:** 该用户的游玩时长

---

### `round-last <room_id>`

查看房间最近一轮结算结果。

| 参数 | 类型 | 说明 |
|------|------|------|
| `room_id` | `str` | 房间名 |

**输出:** 最近一轮的排行数据（玩家成绩、准确率等）

---

### `welcome-config`

查看欢迎语配置与占位符说明。

**输出:** 欢迎语消息列表、可用占位符及当前配置

---

