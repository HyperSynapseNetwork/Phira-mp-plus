# 架构

## 系统边界

```
Internet → PPB (auth/TLS/限流) → PMP (游戏运行时) → PostgreSQL
                                       ↑
                                   WASM 插件
```

- **PPB** (Phira+ Backend): 公共 Web API、OAuth、网关、TLS
- **PMP** (Phira-mp+): TCP 游戏协议、房间状态机、WASM 插件执行、事件持久化

## 核心组件

### Actor 模型

PMP 使用 Actor 模型串行化房间状态变更：

```
Client → Session Actor → Room Actor Mailbox → Room State
                        → Room Command Gateway → per-room mailbox
```

当前状态：

| 组件 | Actor 化程度 |
|------|-------------|
| Session | Mailbox 化，命令路径单写者 |
| Room 控制面 | Mailbox 化（16 个命令） |
| Room 数据面 | 迁移中（状态分散在 RwLock） |
| Supervisor | 关键任务注册与健康跟踪 |

### 数据持久化

```
Event → WAL (fsync) → Queue → Database/Dead-letter → ACK WAL
                              → TelemetryBatcher → DB batch commit
```

WAL 保证崩溃恢复，dead-letter 保证数据库失败后不丢数据。

### 插件系统

```
PMP → wasmtime (component model)
    → capability-based 权限
    → fuel/memory 资源限制
    → quarantine 故障隔离
```

插件通过 WIT ABI v2 定义 host API，每插件独立 capability 声明。

## 关键不变量

1. Room 状态变更必须经过 mailbox
2. WAL ACK 仅在数据到达不可逆终态后发出
3. 插件调用有 fuel/timeout 上限，不会阻塞 PMP 主循环
4. 会话发送有界队列，慢消费者不会阻塞其他客户端
