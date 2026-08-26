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

配置加载规则：默认读取 `server_config.yml`，也可通过 `--config <FILE>` 指定；只有显式提供的命令行参数才覆盖 YAML。配置文件存在但解析或校验失败时服务端拒绝启动。`RUST_LOG`、`NO_COLOR` 等环境变量只影响日志或终端显示。完整配置说明见 [deployment.md](deployment.md)。

## 交互式管理控制台

服务器在普通交互式终端和 tmux 中启动 ratatui 管理控制台。GNU Screen、Linux console、`ansi`/`cons25` 等兼容性较差的终端会进入保守 TUI：不启用备用屏幕、鼠标捕获或 Bracketed Paste，并修正 Ctrl+H Backspace；如果 TUI 初始化失败，会自动回落到逐行兼容控制台。重定向、systemd 和其他非 TTY 环境始终使用逐行控制台。设置 `NO_COLOR` 可关闭颜色。

TUI 快捷键：`Tab` 补全、`Ctrl+A/E` 跳到行首/行尾、`Ctrl+B/F` 左右移动、`Alt+←/→` 按词移动、`Ctrl+W` 删除前一个词、`Alt+Delete` 删除后一个词、`Ctrl+K` 删除到行尾、`Ctrl+L` 清屏、`PgUp/PgDn` 或 `Shift+↑/↓` 滚动日志。

---

## 命令约定

- `<必填参数>` — 尖括号表示必须提供
- `[可选参数]` — 方括号表示可选
- `[默认值]` — 方括号内等号表示默认值

---

## 通用

### `help [command|all|advanced|dev]`

查看命令帮助。`help` 无参数时显示全部命令清单。

| 参数 | 类型 | 说明 |
|------|------|------|
| `command` | `str` (可选) | 要查看详情的命令名，如 `help room close` |
| `all` | 字面量 | 完整视图（含命令级别统计） |
| `advanced` | 字面量 | 仅显示高级命令 |
| `dev` | 字面量 | 仅显示开发诊断命令 |

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

### `version`

显示服务器版本号。

**输出:** `◆ PMP v0.5.2087`（`CARGO_PKG_VERSION`）

---

### `check-config`

验证当前加载的配置并显示脱敏摘要（含活跃会话/房间数，吸收原 `status`/`doctor`）。

**输出:** 服务端版本、端口、数据库、插件目录、容量、保留期、活跃会话/房间数等

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

### `room set <room_id> <field> <value>`

修改房间设置。

| 参数 | 类型 | 说明 |
|------|------|------|
| `room_id` | `str` | 房间名 |
| `field` | `str` | 支持字段：`lock` `cycle` `hidden` `persistent` `degraded` `host` `chart-id` `phira_api_endpoint` `tournament` `live` |
| `value` | 因字段而异 | `lock`/`cycle`/`hidden`/`persistent`/`degraded`/`tournament`/`live` 接受 `true`/`false`；`host` 接受用户 ID、`?`/`system`（系统房主） |

**`live true/false`：** 房间 live 状态（建房自动置 true，此处可手动开关，供 Panel/PPB 控制）。

**`tournament true`：** 赛事模式房间（房间级配置，非全局）。开启后禁用 PMP 默认交互行为——准备倒计时自动开赛、每轮结算广播、房主自动转移、cycle 自动轮换、Playing 期 late-join 确认、聊天，全部交由 PPB 编排（PPB 经 OpenUDS `room.set_tournament` 设置）。

**输出:** 执行结果消息

**字段说明:**
- `persistent true`：把房间转为**持久空房间**（房间空置后保留，服务器重启自动恢复）；`persistent false` 取消
- `degraded true`：清除房间持久化降级状态
- `host ?` / `host system`：设为系统房主（host -1）

---

### `room ready <room_id> [user_id]`

让房间进入准备状态，或强制指定玩家准备。

| 参数 | 类型 | 说明 |
|------|------|------|
| `room_id` | `str` | 房间名 |
| `user_id` | `int` (可选) | 不指定则房间进入准备状态；指定则强制该玩家准备 |

**输出:** 准备状态结果

---

### `room start <room_id>`

服务端强制发起房间游戏；客户端完成谱面加载并准备后正式开始。

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

### `room whitelist add <room_id> <user_id>`

将用户加入房间白名单。白名单**非空**时，仅白名单内用户（+ 房主 + 管理员）可加入该房间；为空则开放。

| 参数 | 类型 | 说明 |
|------|------|------|
| `room_id` | `str` | 房间名 |
| `user_id` | `int` | Phira 用户 ID |

> 若该用户同时在房间黑名单中，提示黑名单优先（黑名单仍然生效）。

### `room whitelist remove <room_id> <user_id>`

将用户移出房间白名单。

### `room whitelist list <room_id>`

查看房间白名单（为空显示"开放加入"）。

### `room whitelist clear <room_id>`

清空房间白名单，恢复开放加入。

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

### `ban ip <ip> [reason]`

封禁指定 IP 地址。

| 参数 | 类型 | 说明 |
|------|------|------|
| `ip` | `str` | IP 地址 |
| `reason` | `str` (可选) | 封禁原因 |

**输出:** 封禁确认消息

---

### `unban ip <ip>`

取消指定 IP 封禁。

| 参数 | 类型 | 说明 |
|------|------|------|
| `ip` | `str` | IP 地址 |

**输出:** 解封确认消息

---

### `banlist ip`

查看被封禁的 IP 列表。

**输出:** 被封禁的 IP 及原因列表

---

### `ip-history <user_id>`

查看用户使用过的 IP 历史。

| 参数 | 类型 | 说明 |
|------|------|------|
| `user_id` | `int` | 用户 ID |

**输出:** 用户使用过的 IP、次数与最近使用日期

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

### `plugin remove <name> [-y]`

卸载插件（删除插件文件、清除扩展与私有数据，不可撤销）。

| 参数 | 类型 | 说明 |
|------|------|------|
| `name` | `str` | 插件名 |
| `-y` | flag | 跳过确认直接删除（供 OpenUDS / 非交互调用） |

**输出:** `Removed plugin <name>` 或错误信息

> 交互环境（TTY）删除需输入 `y` 确认；非 TTY 环境自动取消，需用 `-y`（OpenUDS 经 `cli.execute "plugin remove X -y"` 可完成删除）。

---

## 基准测试

### `benchmark run <fixed|ramp>`

运行基准测试（**进程内纯内部调用版**）。直接调用服务器内部 API 生成负载，
不跑 phira 线协议、不拉起独立子进程、**不依赖独立数据库**（复用当前实例，
虚拟会话用负数 id、房间用 `bench-` 前缀，结束后全清理，不影响真实玩家）。

运行期间 CLI 输入被锁定，输入框上方显示状态矩形（会话数/游玩房间/CPU/RAM/
速率/进度条），按 `x` 键结束；结束（手动或正常完成）显示报告。

**两种模式：**

- **`fixed`** —— 维持最大同时在线游玩房间数，持续到时长或取消。
- **`ramp`** —— 自动加压直到 CPU / RAM 触顶后维持，持续到时长或取消。

> 会话数由房间自动推导：每个房间 2 个独立虚拟成员，`sessions = playing_rooms × 2`。

| 参数 | 类型 | 说明 |
|------|------|------|
| `--playing-rooms <M>` | `int` | fixed：最大同时在线游玩房间数 |
| `--cpu <P>` | `float` | ramp：CPU 上限（百分比 0-100） |
| `--ram <S>` | `str` | ramp：RAM 上限（如 4096m / 4g / 字节数） |
| `--duration <D>` | `str` (可选) | 时长：30 / 10m / 2h（缺省 60s） |
| `--forever` | flag | 永久运行（直到 x 键结束） |
| `--output <fmt>` | `str` (可选) | 输出格式：text（默认）、json、markdown |

**示例:**
```
benchmark run fixed --playing-rooms 50 --duration 10m
benchmark run fixed --playing-rooms 100 --forever
benchmark run ramp --cpu 80 --ram 4g --duration 1h
```

**输出:** Benchmark 报告（时长、峰值/平均会话数与游玩房间数、CPU%、RSS、
命令速率、错误数、模式参数、ramp 触顶到达点）。报告同时写入当前实例的
`mp_runtime_benchmark_reports` 表。

---

## 运行时诊断

### `runtime`

一次打印全部运行时诊断分区：command registry / Phira HTTP / events / schema / persistence / latency。

**输出:** 各分区的统计与诊断信息（一次出完）。

---

### `config reload`

重新读取启动时 `--config` 指定的 YAML 文件。热更新 `chat_enabled` 和 `monitors`；YAML 中显式声明的管理员/压测凭据也会同步更新。显式 `--monitor` 始终保持高于 YAML 的优先级；YAML 与持久化文件都未声明的动态管理员/凭据状态不会被重载误清空。端口、目录、数据库、限流和持久化内部策略仍需重启。

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



### `welcome-config`

查看欢迎语配置与占位符说明。

**输出:** 欢迎语消息列表、可用占位符及当前配置

欢迎语模板：`server_config.yml` 的 `welcome.messages[lang]`（单文件配置）> 内置国际化默认（en-US/zh-CN/zh-TW **三语键集一致**，随版本更新）。**缺省不配置 → 内置国际化，按用户语言渲染**。`welcome-config` 命令可查看内置默认与占位符。

---

### `roomcreation on|off`

开关玩家建房功能。

| 参数 | 类型 | 说明 |
|------|------|------|
| `on`/`enable`/`1` | — | 开启玩家建房 |
| `off`/`disable`/`0` | — | 关闭玩家建房 |

无参数时显示当前开关状态。**输出:** `◆ 玩家建房：已开启/已关闭`

---

### `connections on|off`

开关**新用户连接**（运维工具：维护时防止新玩家进入，等存量玩家下线）。

| 参数 | 类型 | 说明 |
|------|------|------|
| `on`/`enable`/`1` | — | 开启新用户连接 |
| `off`/`disable`/`0` | — | 关闭新用户连接（**已连接用户重连不受影响**） |

无参数时显示当前状态。关闭时新用户 Authenticate 被拒（`auth-not-accepting`），已在 `server.users` 中的用户重连放行。该标志运行时切换，`config reload` 不重置。

---

### 聊天执行命令（管理员）

管理员在**聊天消息**中发送 `/命令` 可直接执行**原生 CLI 语法**（空格分隔，无字符限制，无需 `_` 房间名转换语法）。命令输出以 `[CLI]` 前缀回显给发送者，不广播给房间。

```
/rooms
/connections off
/server stats
```

---

### `wal inspect`

查看 WAL 状态统计。

**输出:** WAL 文件路径、文件大小

---

### `dead-letter list [limit]`

列出 dead-letter 记录摘要。

| 参数 | 类型 | 说明 |
|------|------|------|
| `limit` | `int` (可选) | 显示最近 N 条（默认 10） |

**输出:** dead-letter 总记录数与最近条目摘要

---

### `dead-letter replay`

重放 dead-letter 事件到持久化队列。

**输出:** 已提交重放的事件数

---

## 自动更新

自动更新默认关闭。启用后服务器启动时与每隔 `check_interval_secs` 检查
GitHub Release；检测到新版本时自动"预约"（写入 `pending_update`，幂等，
不重复预约），下线满 `min_idle_minutes` 分钟后由后台执行器自动下载替换
并尝试重启。手动 `update schedule` 与自动更新统一走该预约流程。
检查失败静默降级（只记 warn），不影响运行。

### `update [check|apply|force|schedule|cancel|auto]`

自动更新命令入口。无子命令时显示全部可用子命令。

**子命令:** `update check`（检查版本）、`update apply`（立即更新，不预约）、`update force`（强制立刻更新）、`update schedule`（预约更新）、`update cancel`（取消预约）、`update auto [on|off]`（开关自动更新）

### `update check`

检查 GitHub 最新 Release 并与当前版本对比。

**输出:** 当前版本、最新版本、是否有更新、发布页链接

**示例:**
```
update check
```

### `update apply`

**立即**启动更新流程（不预约）：检查在线玩家与空闲时长，满足条件则下载并
替换可执行文件、尝试重启；有玩家在线或最近下线未满 `min_idle_minutes`
时只返回原因、不预约。需要自动执行时可改用 `update schedule` 预约。

**输出:** 更新结果或拒绝原因

### `update force`

强制立即更新，跳过在线玩家与空闲时长检查。

**输出:** 更新结果或失败原因

### `update schedule`

预约更新：检查 GitHub 最新 Release，有新版本则记录为预约目标
（`pending_update`），下线满 `min_idle_minutes` 分钟后由后台执行器自动
执行（与自动更新同一流程）。无新版本时返回"已是最新"。

预约幂等：若已存在相同或更新版本的预约，不覆盖、不重复预约，返回当前
预约版本；仅当新版本高于已预约版本时才更新预约。

**输出:** 预约结果或拒绝原因

**示例:**
```
update schedule
```

### `update cancel`

取消预约更新（清除 `pending_update`）。无预约时提示"当前无预约更新"。

**输出:** 取消结果

**示例:**
```
update cancel
```

### `update auto [on|off]`

开关自动更新（修改运行时 `live_config.auto_update.enabled`，无需重启）。
无参数时显示当前状态。

| 参数 | 类型 | 说明 |
|------|------|------|
| `on`/`off` | 字面量 (可选) | 开启 / 关闭自动更新；省略则显示状态 |

**示例:**
```
update auto
update auto on
update auto off
```

