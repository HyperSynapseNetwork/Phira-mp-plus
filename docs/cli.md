# Phira-mp+ CLI 命令文档

## 启动参数

```
phira-mp-plus-server [OPTIONS]

  -p, --port <PORT>          服务器监听端口 [默认: 12346]
  -d, --plugins-dir <DIR>    WASM 插件目录路径 [默认: "plugins"]
  -e, --ext-file <FILE>      扩展数据持久化 JSON 文件路径 [默认: "data/extensions.json"]
      --no-cli               禁用交互式 CLI 管理控制台
  -l, --log-file <NAME>      日志文件基础名称 [默认: "phira-mp-plus"]
  -m, --monitor <IDS>...     允许旁观的用户 ID（可多次指定，如 `-m 1 -m 2`）
      --http-port <PORT>     HTTP/SSE 服务端口 [默认: 12347]
      --proxy-port <PORT>   PROXY protocol 端口 [默认: 0=禁用, 典型值 12344]
  -c, --config <FILE>        YAML 配置文件路径 [默认: "server_config.yml"]
  -h, --help                 显示帮助信息
  -V, --version              显示版本号
```

配置加载规则：默认读取 `server_config.yml`，也可通过 `--config <FILE>` 指定；命令行参数会覆盖 YAML 中对应字段。`RUST_LOG`、`NO_COLOR` 等环境变量只影响日志或终端显示。完整配置说明见 [configuration.md](configuration.md)。

## 交互式管理控制台

服务器在普通交互式终端和 tmux 中启动 ratatui 管理控制台。GNU Screen、Linux console、`ansi`/`cons25` 等兼容性较差的终端会进入保守 TUI：不启用备用屏幕、鼠标捕获或 Bracketed Paste，并修正 Ctrl+H Backspace；如果 TUI 初始化失败，会自动回落到逐行兼容控制台。重定向、systemd 和其他非 TTY 环境始终使用逐行控制台。设置 `NO_COLOR` 可关闭颜色。

TUI 快捷键：`Tab` 补全、`Ctrl+A/E` 跳到行首/行尾、`Ctrl+B/F` 左右移动、`Alt+←/→` 按词移动、`Ctrl+W` 删除前一个词、`Alt+Delete` 删除后一个词、`Ctrl+K` 删除到行尾、`Ctrl+L` 清屏、`PgUp/PgDn` 或 `Shift+↑/↓` 滚动日志。

---

## 命令列表

### 通用

| 命令 | 参数 | 级别 | 说明 |
|------|------|------|------|
| `help [command\|all\|advanced\|dev]` | 可选：命令名 | Primary | 显示帮助信息 |
| `exit` | 无 | Primary | 关闭服务器 |
| `status` | 无 | Primary | 显示服务器状态（版本、端口、在线数、插件数） |
| `config reload` | 无 | Primary | 热重载 `server_config.yml` |

### 房间

| 命令 | 参数 | 级别 | 说明 |
|------|------|------|------|
| `rooms` | 无 | Primary | 查看活跃房间列表 |
| `room info <room_id>` | 房间名 | Primary | 查看房间详情 |
| `room host <room_id> <user_id\|?>` | 房间名, 用户ID/`?` | Primary | 设置房主（`?` = 系统房主） |
| `room close <room_id>` | 房间名 | Primary | 解散房间 |
| `room set <room_id> <field> <value>` | 房间名, 字段, 值 | Primary | 修改房间设置（lock/cycle/hidden/persistent/host/chart-id/phira_api_endpoint） |
| `room create-empty <room_id> [endpoint]` | 房间名, 可选 API endpoint | Advanced | 创建持久空房间 |
| `room start <room_id>` | 房间名 | Advanced | 管理员开始游戏 |
| `room cancel <room_id>` | 房间名 | Advanced | 取消开始游戏 |
| `room kick <room_id> <user_id>` | 房间名, 用户ID | Advanced | 踢出房间 |
| `room hide <room_id> [bool]` | 房间名, 可选布尔值 | Advanced | 隐藏房间（不在 Web API / 欢迎语中显示） |
| `room unhide <room_id>` | 房间名 | Advanced | 取消隐藏 |
| `room force-move <room_id> <user_id> [monitor]` | 房间名, 用户ID, 可选monitor | Advanced | 强制迁移用户到指定房间 |
| `room history <room_id>` | 房间名 | Advanced | 查看房间游玩历史 |
| `room rounds <room_id>` | 房间名 | Advanced | 查看房间轮次列表 |
| `room round <round_uuid>` | 轮次 UUID | Advanced | 查看轮次详情 |
| `room uuid <room_id>` | 房间名 | Advanced | 查看房间 UUID |
| `room ban <room_id> <user_id> [reason]` | 房间名, 用户ID, 可选原因 | Advanced | 房间封禁用户 |
| `room unban <room_id> <user_id>` | 房间名, 用户ID | Advanced | 取消房间封禁 |
| `room banlist <room_id>` | 房间名 | Advanced | 查看房间封禁列表 |

### 用户

| 命令 | 参数 | 级别 | 说明 |
|------|------|------|------|
| `users` | 无 | Primary | 查看在线用户列表 |
| `kick <user_id>` | 用户ID | Primary | 踢出用户 |
| `ban <user_id> [reason]` | 用户ID, 可选原因 | Primary | 全局封禁用户 |
| `unban <user_id>` | 用户ID | Primary | 取消封禁 |
| `banlist` | 无 | Primary | 查看封禁列表 |

### 广播

| 命令 | 参数 | 级别 | 说明 |
|------|------|------|------|
| `broadcast all <message>` | 消息文本 | Primary | 广播给所有用户 |
| `broadcast room <room_id> <message>` | 房间名, 消息 | Primary | 广播到指定房间 |
| `broadcast user <user_id> <message>` | 用户ID, 消息 | Primary | 私信指定用户 |

### 管理员 ID

| 命令 | 参数 | 级别 | 说明 |
|------|------|------|------|
| `admin-id list` | 无 | Advanced | 查看管理员 ID 列表 |
| `admin-id add <PhiraID>` | Phira ID | Advanced | 添加管理员 |
| `admin-id remove <PhiraID>` | Phira ID | Advanced | 移除管理员 |
| `admin-id set <PhiraID...>` | 一个或多个 Phira ID | Advanced | 替换整个管理员列表 |

### 插件

| 命令 | 参数 | 级别 | 说明 |
|------|------|------|------|
| `plugin list` | 无 | Primary | 列出所有已加载插件 |
| `plugin enable <name>` | 插件名 | Primary | 启用插件 |
| `plugin disable <name>` | 插件名 | Primary | 禁用插件 |
| `plugin reload` | 无 | Advanced | 重新加载所有插件 |
| `plugin info <id_or_name>` | 插件 ID 或名 | Advanced | 查看插件详情 |
| `plugin call <name> <method> [args]` | 插件名, 方法, 可选 JSON 参数 | Advanced | 调用插件 API |

### 基准测试

| 命令 | 参数 | 级别 | 说明 |
|------|------|------|------|
| `benchmark [seconds] [rooms]` | 可选：秒, 房间数 | Advanced | 运行真实 TCP 网络压测（需 Phira token） |
| `benchmark modes` | 无 | Advanced | 查看三种压测模式说明 |
| `benchmark run real [seconds] [rooms]` | 可选：秒, 房间数 | Advanced | 显式真实 TCP 协议测试 |
| `benchmark run hybrid [key=value...]` | 可选参数 | Advanced | 运行混合 Phira 探测（chart/record 查询） |
| `benchmark report [mode\|limit]` | 可选：模式或数量 | Advanced | 查看最新 Benchmark 报告 |
| `benchmark history [mode] [limit]` | 可选：模式, 数量 | Advanced | 查看持久化的 Benchmark 历史 |
| `benchmark token bind <tokens...>` | Phira token | Developer | 绑定压测用 token |
| `benchmark cleanup` | 无 | Developer | 清理所有 `bench-*` 房间 |

默认推荐使用 `simulation` 进行压力测试（隔离本地，不访问 Phira，不需要 token）。Real Benchmark 详见 [benchmark-real.md](benchmark-real.md)。

### 模拟器

| 命令 | 参数 | 级别 | 说明 |
|------|------|------|------|
| `simulation status` | 无 | Primary | 查看模拟器状态 |
| `simulation run <preset> [key=value...]` | preset + 可选覆盖 | Primary | 启动隔离本地压测（支持 auto_tick） |
| `simulation stop` | 无 | Primary | 停止模拟 |
| `simulation cleanup` | 无 | Primary | 清理模拟数据 |
| `simulation scenarios` | 无 | Advanced | 查看可用场景列表 |
| `simulation suite <name> [key=value...]` | suite 名 + 可选覆盖 | Advanced | 运行场景序列 |
| `simulation report [list\|clear]` | 可选子命令 | Advanced | 查看模拟报告 |
| `simulation tick [count]` | 可选：步数 | Developer | 手动推进模拟 |
| `simulation inspect [limit]` | 可选：条数限制 | Developer | 查看影子世界数据 |
| `simulation seed <u64>` | 种子值 | Developer | 设置确定性随机种子 |
| `simulation persist` | 无 | Developer | 发送快照到持久化 Worker |
| `simulation sample` | 无 | Developer | 查看样本数据 |

预置场景：`baseline`（默认）、`small`、`medium`、`large`、`custom`。

### Runtime v2 诊断

| 命令 | 参数 | 级别 | 说明 |
|------|------|------|------|
| `runtime status` | 无 | Primary | Runtime v2 诊断总览 |
| `runtime cutover [direct_only\|worker_preferred]` | 可选：切换模式 | Advanced | 查看/切换遥测持久化 Cutover 模式 |
| `runtime persistence` | 无 | Advanced | 持久化 Worker 和批处理器统计 |
| `runtime roadmap` | 无 | Developer | Runtime v2 路线图 |
| `runtime phira` | 无 | Developer | Phira HTTP RetryClient 统计 |
| `runtime commands` | 无 | Developer | 命令注册表统计 |
| `runtime events` | 无 | Developer | 事件总线统计 |
| `runtime schema` | 无 | Developer | 持久化 Schema 信息 |
| `runtime rooms` | 无 | Developer | 房间 Actor 迁移状态 |
| `runtime actors` | 无 | Developer | Actor 模型迁移蓝图 |

### 扩展字段（已废弃）

| 命令 | 参数 | 说明 |
|------|------|------|
| `extension list` | 无 | 查看已注册扩展字段 |
| `extension get <target> <key>` | 目标 ID, Key | 获取扩展数据 |

这些命令仅用于兼容性查看，扩展数据管理已迁移到数据库持久化。

---

## 命令级别说明

| 级别 | 说明 |
|------|------|
| Primary | 基础管理命令，`help` 默认显示 |
| Advanced | 高级操作，需 `help advanced` 查看 |
| Developer | 开发诊断，需 `help dev` 查看 |
