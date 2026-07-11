# 实用功能可靠性审计

本审计以用户提供的新源码压缩包为唯一基线，重点检查“配置或命令看似存在，但实际无法生效”“客户端必须重连才能恢复状态”“运行时注册成功但请求仍不可达”等实用功能故障。

## 已修复问题

| 范围 | 原问题 | 修复后的行为 |
|---|---|---|
| 认证与欢迎语 | 欢迎语可能早于认证成功包发送，客户端尚未建立用户状态时会丢弃 | 认证成功包完成 flush 后再发送欢迎语 |
| 重连 | 新连接替换用户会话后，旧 socket 可继续存活到心跳超时 | 新会话接管后立即关闭旧传输并移除旧 Session |
| 认证失败 | 远程认证失败只断开，不一定向客户端发送最终错误；拒绝分支还可能继续进入成功后处理 | 先发送并 flush 明确错误，未初始化 Session 的拒绝路径禁止落入成功分支 |
| 连接审计信息 | 认证事件在 Session 初始化前读取 IP，运行时事件中的 IP 为空 | 直接使用已接受 socket 的远端地址记录真实 IP |
| 建房/加入顺序 | 创建者或加入者在成功响应前收到增量事件/房主事件，存在客户端状态竞争 | 先发送 `CreateRoom/JoinRoom(Ok)` 建立完整状态，再发送创建或房主增量事件；加入增量仅发给原有成员 |
| 管理踢出/关房 | 服务端已移除成员，但客户端本地仍停留在房间或旧连接继续发命令 | 房间踢出/关房立即发送 `LeaveRoom(Ok)`；全局踢出 flush 提示后关闭传输；房间封禁先显示理由再移出 |
| 房间容量 | `max_rooms` 只存在于配置，没有在建房路径执行 | 客户端建房和管理/WIT 创建空房均执行上限 |
| Monitor | 旁观者仍受玩家锁房、房间封禁及游戏中二次确认限制 | 经授权 monitor 绕过玩家专属限制；普通玩家行为不变 |
| 聊天开关 | `chat_enabled=false` 时请求被静默丢弃，客户端看起来无响应 | 返回明确 `Chat(Err)`，使客户端可展示禁用原因 |
| 强制开局命令 | `room force-start`、`force-start` 只出现在元数据/文档，执行分支缺失 | 三种入口 `room start`、`room force-start`、`force-start` 均可执行 |
| 多级命令 | 注册表只按首词匹配，`config reload` 永远无法命中 | 使用最长命令前缀匹配并把剩余 token 作为参数 |
| 配置重载 | 固定读取 `server_config.yml`，且在 Tokio 线程中 `blocking_write` 可能 panic | 读取启动时指定路径，使用非阻塞锁并明确列出热更新字段 |
| 重载优先级/动态状态 | 重载可覆盖显式 CLI monitor，并清空由管理命令、数据库或持久化文件维护的管理员/凭据；损坏的凭据文件还会被当成空列表 | 保留 CLI 高优先级；仅在有效 YAML/持久化源明确提供时替换动态列表，损坏文件明确报错且不提交部分更新 |
| 配置覆盖 | CLI 自带默认值会覆盖 YAML，即使用户没有提供该参数 | 只有显式 CLI 参数覆盖 YAML |
| 配置错误 | 无效 YAML 静默回退默认配置，拼错字段名也会被忽略 | 文件存在但解析、未知顶层字段或校验失败时拒绝启动；缺失文件才使用默认值 |
| 配置值规范化 | 全局 Phira API 地址可带空白/尾斜杠或非法 scheme，空闲检查间隔可设为 0 | 启动和重载时规范化并校验 HTTP(S) endpoint，拒绝 0 秒空闲检查忙循环 |
| CLI 开关 | 文档声明 `--no-cli`，代码未实现 | `--no-cli` 真实覆盖 `cli_enabled=false` |
| 扩展数据 | 不通过 CLI 启动时默认不持久化扩展数据 | 内置默认路径统一为 `data/extensions.json` |
| 插件 SSE | SSE 路由只在 HTTP 启动时固化，插件重载后新端点 404 | catch-all 从实时注册表解析，重载后立即生效 |
| SSE 过滤 | `event_types` 被保存但从未使用 | 宿主在调用插件前执行过滤，并兼容历史事件名 |
| Linux Release | `ring/cc-rs` 找不到 `x86_64-linux-musl-gcc` | 安装 `musl-tools` 并显式设置 CC 与 linker |
| Build 版本 | auto-patch 推送新提交后，后续 job 仍 checkout 旧 SHA | auto-patch 输出实际提交 SHA，检查和构建统一使用该 SHA |

## 行为边界

- `config reload` 立即更新 `chat_enabled`、`monitors`，并在 YAML/持久化源明确提供时同步管理员与压测凭据；显式 `--monitor` 优先级保持不变。
- 端口、目录、数据库、连接限流和 Runtime v2 策略仍需重启。
- `max_users_per_room` 内置默认值为 100；`max_rooms` 未设置时不限制。
- 插件 SSE 内置房间事件名为 `create_room`、`update_room`、`join_room`、`leave_room`、`new_round`。

## 建议回归场景

1. 使用自定义 YAML 端口启动且不传 `--port`，确认 YAML 不被 12346 覆盖；再用显式 `--monitor` 启动并执行 `config reload`，确认 CLI 值不被覆盖。
2. 使用损坏 YAML 启动，确认进程返回错误而不是继续监听默认端口。
3. 同一账号快速重连，确认旧连接立即失效且欢迎语只在认证成功后显示。
4. 首位玩家加入空房、普通玩家加入非空房，确认无需超时重连即可看到完整状态。
5. 执行 `room kick`、`room close`、全局 `kick`、`ban`，确认客户端立即退出对应状态并显示理由/提示。
6. 分别执行 `room start`、`room force-start`、`force-start`。
7. 达到 `max_rooms` 后分别尝试客户端建房和 `room create-empty`。
8. 插件运行中重载并注册新 HTTP/SSE 路由，确认无需重启即可访问；验证 `event_types` 能过滤无关事件。
9. 在 GitHub Actions 运行 Build 与 Release，确认 Linux musl 的 `ring` 编译通过，且构建版本与 auto-patch 后提交一致。

## 验证限制

本次环境未提供 Rust/Cargo 工具链，因此无法在本地执行 `cargo check`、`cargo test` 与 `cargo fmt`。已执行源码差异审阅、TOML/YAML 解析、工作流结构检查、关键合同静态检查与压缩包完整性检查；最终仍应以 GitHub Actions 的 workspace check/test 为合并门槛。

## 架构加固后续说明

本文记录的是较早一轮实用功能修复。连接接入、Actor 不确定结果、插件 capability/资源限制、持久化 flush、Supervisor 和关闭顺序的最新状态以 [architecture-hardening.md](architecture-hardening.md) 与根目录 `HARDENING_REPORT.md` 为准。两份文档发生冲突时，以后者及当前代码为准。
