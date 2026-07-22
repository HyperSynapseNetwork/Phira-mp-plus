# PMP 保证

> 最后更新：2026-07-22 · 适用于 Phira-mp+ v0.4.x
> 完整英文版：[Guarantees (English)](guarantees.en.md)

## 1. 事件持久化

| 保证 | 前提 | 失败语义 | 证据 |
|------|------|----------|------|
| 已 admission 的事件在进程崩溃后存活 | WAL fsync 成功 | admission 前拒绝，无数据丢失 | WAL replay + ACK compaction 测试 |
| 已 ACK 的事件不会丢失 | 数据库提交或 dead-letter 写入成功 | 重试直到 durable | `worker.rs` 状态机 |
| Telemetry batcher 事件崩溃后存活 | 批次尚未提交 | WAL replay 恢复未提交事件 | WAL 集成测试 |

## 2. 房间状态

| 保证 | 前提 | 失败语义 | 证据 |
|------|------|----------|------|
| 房间命令串行执行 | 所有命令经过 mailbox | mailbox 拒绝保持队列顺序 | Room actor 测试 |
| 无重复房间代次 | UUID + generation fencing | 延迟命令被拒绝 | Mailbox fencing 测试 |

## 3. 插件隔离

| 保证 | 前提 | 失败语义 | 证据 |
|------|------|----------|------|
| Fuel 限制执行 | Wasmtime fuel 启用 | 耗尽时 trap | Fuel 测试 |
| 内存有界分配 | Store limiter 设置 | 分配失败 | WASM 运行时集成测试 |

## 4. 关闭

| 保证 | 前提 | 失败语义 | 证据 |
|------|------|----------|------|
| 关闭前接受的 events 会被 flush | Flush timeout 被尊重 | 超时事件返回给调用方 | 关闭序列测试 |
| 关闭时 WAL compaction | Compaction 成功 | WAL 下次启动时增长 | `shutdown()` + compact 测试 |

## 5. 已知不保证

- 集群级 HA（仅单进程）
- 跨进程插件隔离（仅进程内 Wasmtime）
- 多区域复制

> 详细英文版：[Guarantees (English)](guarantees.en.md)
