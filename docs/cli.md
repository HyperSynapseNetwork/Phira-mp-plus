# Phira-mp+ CLI 命令文档

## 启动参数

Phira-mp+ 服务器启动时支持以下命令行参数：

```
phira-mp-plus-server [OPTIONS]
```

| 参数 | 简写 | 默认值 | 说明 |
|------|------|--------|------|
| `--port` | `-p` | `12346` | 服务器监听端口 |
| `--plugins-dir` | `-d` | `plugins` | WASM 插件目录路径 |
| `--ext-file` | `-e` | (无) | 扩展数据持久化 JSON 文件路径 |
| `--no-cli` | — | `false` | 禁用交互式 CLI 管理控制台 |
| `--log-file` | `-l` | `phira-mp-plus` | 日志文件基础名称 |
| `--monitor` | `-m` | `[2]` | 允许旁观的用户 ID（可多次指定，如 `-m 1 -m 2`） |
| `--help` | `-h` | — | 显示帮助信息 |
| `--version` | `-V` | — | 显示版本号 |

### 示例

```bash
# 默认启动
phira-mp-plus-server

# 指定端口和插件目录
phira-mp-plus-server --port 8080 --plugins-dir ./my-plugins

# 带多个监视器 + 扩展数据持久化
phira-mp-plus-server -p 12346 -m 1 -m 2 -m 100 -e ./data/extensions.json

# 无 CLI 模式（后台运行）
phira-mp-plus-server --no-cli

# 查看帮助
phira-mp-plus-server -h
```

---

## 交互式管理控制台

当服务器启动时（未使用 `--no-cli`），会自动进入交互式管理控制台。在控制台中可以直接输入命令进行管理操作。

### 命令速览

| 命令 | 别名 | 说明 |
|------|------|------|
| `help` | `h`, `?` | 显示帮助信息 |
| `exit` | `quit`, `q` | 关闭服务器 |
| `status` | `st` | 显示服务器状态 |
| `plugins` | `pl` | 列出所有已加载的插件 |
| `plug-enable` | `pe` | 启用指定插件 |
| `plug-disable` | `pd` | 禁用指定插件 |
| `plug-reload` | `pr` | 重载所有插件 |
| `users` | `u` | 列出在线用户 |
| `rooms` | `r` | 列出活跃房间 |
| `kick` | `k` | 踢出用户 |
| `broadcast` | `bc` | 广播消息到所有用户 |
| `ext-list` | `el` | 列出所有注册的扩展数据字段 |
| `ext-get` | `eg` | 获取指定用户/房间的扩展数据 |

---

## 命令详解

### 通用命令

#### `help` / `h` / `?`

显示所有可用命令及其说明。

#### `exit` / `quit` / `q`

安全关闭 Phira-mp+ 服务器。

#### `status` / `st`

显示服务器当前运行状态，包括：
- 服务器版本
- 运行状态
- 已加载插件数量

---

### 插件管理命令

#### `plugins` / `pl`

列出所有已加载的插件及其详细信息：

```
名称                      版本        状态      作者
------------------------------------------------------------
event-logger              0.1.0      启用      Phira-mp+
example-plugin            0.1.0      启用      user
```

状态说明：
- `启用` — 插件正常运行中
- `禁用` — 插件已加载但未启用
- `已加载` — 插件已加载但未初始化
- `错误` — 插件初始化或运行时出错

#### `plug-enable <插件名>` / `pe <插件名>`

启用指定插件。

示例：
```
> plug-enable my-plugin
  插件 'my-plugin' 已启用
```

#### `plug-disable <插件名>` / `pd <插件名>`

禁用指定插件。

#### `plug-reload` / `pr`

卸载所有插件并重新从插件目录加载。

---

### 用户管理命令

#### `users` / `u`

列出当前所有在线用户（显示用户 ID、名称、当前状态）。

#### `kick <目标ID>` / `k <目标ID>`

将指定用户踢出服务器。

示例：
```
> kick 42
  用户 42 已被踢出服务器
```

---

### 房间管理命令

#### `rooms` / `r`

列出所有当前活跃的房间（显示房间 ID、玩家数、状态、房主）。

#### `kick <房间ID> <用户ID>` / `k <房间ID> <用户ID>`

将指定用户踢出指定房间。

示例：
```
> kick my-room 42
  用户 42 已被踢出房间 my-room
```

---

### 消息命令

#### `broadcast <消息>` / `bc <消息>`

向所有在线的用户广播一条消息。

示例：
```
> broadcast 服务器将于5分钟后维护，请做好准备。
```

---

### 扩展数据命令

#### `ext-list` / `el`

列出所有已注册的扩展数据字段，包括用户字段和房间字段。

示例：
```
用户扩展字段:
    - ban-status
    - score-rank
房间扩展字段:
    - min-level
    - tags
```

#### `ext-get <用户ID|房间ID> <字段名>` / `eg <用户ID|房间ID> <字段名>`

获取指定用户或房间的扩展数据值。自动检测是用户 ID 还是房间 ID。

示例：
```
> eg 42 ban-status
  用户 42 的扩展数据 'ban-status': active

> eg my-room min-level
  房间 'my-room' 的扩展数据 'min-level': 5
```

---

## 退出码

| 退出码 | 说明 |
|--------|------|
| 0 | 正常退出 |
| 1 | 启动失败（端口被占用、配置错误等） |
| 2 | 运行时错误 |

---

## 日志文件

日志文件存储在 `log/` 目录下，按小时轮转：

```
log/
├── phira-mp-plus.2025-01-01-00.log
├── phira-mp-plus.2025-01-01-01.log
└── ...
```

日志级别可通过 `RUST_LOG` 环境变量控制：

```bash
# 仅显示 info 及以上级别
RUST_LOG=info phira-mp-plus-server

# 显示 debug 及以上级别
RUST_LOG=debug phira-mp-plus-server

# 特定模块的日志级别
RUST_LOG=phira_mp_plus_server=debug,info phira-mp-plus-server
```
