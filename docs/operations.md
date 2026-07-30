# 运维手册

## 备份与恢复

### 创建备份

```bash
pmp-admin backup /path/to/backup/dir
```

备份内容：
- `data/` 目录（扩展数据、插件数据）
- `plugins/` 目录（插件文件 + 能力文件）
- `server_config.yml`

### 验证备份

```bash
pmp-admin backup verify /path/to/backup/dir
```

### 恢复

手动将备份文件解压到目标目录，重启 PMP。

> 注意：当前备份不含自动恢复机制。需确保目标目录配置与备份时一致。

---

## 启动恢复

PMP 启动时自动执行恢复流程：

1. **WAL 重放** — Worker 按 WAL sequence 顺序重放未 ACK 事件（先 WAL 后 queue）
2. **扫描未完成轮次** — 查询 `mp_rounds WHERE finished_at IS NULL`，标记为 aborted
3. **Schema 验证** — 验证 `_pmp_schema_version` 可读，失败时 not-ready
4. **WAL 健康检查** — 验证 PersistenceWorker 状态，等待 replay drained 后才 ready
5. **Playtime 会话修复** — 关闭全部残留 `session_start`（最多补偿 1h，防止停机时间计入）
6. **持久空房恢复** — 从 `mp_settings` 读取 `persistent_rooms` 列表并重建
7. **DLQ 重放** — 先 rename active DLQ 文件再读取，避免与 Worker 并发写冲突。完成后删除 replaying 文件

以上任一步骤失败时，服务进入 **not-ready** 状态（不接收客户端连接），必须人工干预。

## Playing 重连宽限

玩家在 Playing 状态断线时不会立即被踢出房间。宽限时间默认 15 秒，可通过配置：

```yaml
idle:
  playing_reconnect_grace_secs: 15  # 0 = 关闭，恢复旧行为
```

宽限期内：
- 保留房间成员资格
- 新 Session 可替换旧 Session
- timeout 后通过 Actor 执行 `remove_user` + 持久化 offline

## 持久化 admission 顺序

关键事件（UserAuthenticated、RoundCompleted、RoomSnapshot 等）的持久化顺序：

```text
WAL append/fsync → queue reservation → background worker → PostgreSQL commit → WAL ACK
```

Queue 满时使用 100ms 有界等待，超过后返回 `WalOnly` 而不是在 WAL 前丢事件。
WalOnly 事件由 WAL recovery scanner 每 5 秒重新入队，保持 WAL sequence 顺序（不插入队尾）。

### Admission 返回语义

| 返回 | 含义 |
|------|------|
| `Queued` | WAL 已持久化，Worker 已收到通知 |
| `WalOnly` | WAL 已持久化，queue 满，scanner 会重试 |
| `RejectedBeforeWal` | 事件未进入持久化系统（极少发生，仅 WAL 文件系统错误） |

### WAL sequence gating

Worker 维护 `next_expected_sequence` 和 `BTreeMap` 缓冲区。
来自 channel 的消息如果 sequence 不连续，会先存入缓冲区，等缺失的消息到达后再按序处理。
来自 replay 和 scanner 的消息自带 sequence，确保 WalOnly 事件不会插队到 Queued 事件之前。

非关键事件（调试 telemetry 等）允许 best-effort 丢弃。

PMP 配置支持 YAML 文件、环境变量、CLI 参数三层覆盖（优先 CLI > 环境变量 > YAML）。

### 配置加载顺序

1. `--config <FILE>` 指定（或默认 `server_config.yml`）
2. 环境变量覆盖（如 `PMP_PORT=12346`）
3. CLI 参数覆盖（如 `--port 12346`）

### 关键配置项

| 项 | 默认值 | 说明 |
|----|--------|------|
| `port` | `12346` | TCP 游戏端口 |
| `http_port` | `12347` | HTTP/SSE/WS 端口 |
| `max_sessions` | `4096` | 最大在线会话数 |
| `database_url` | - | PostgreSQL 连接串 |
| `persistence_retention_days` | `30` | 事件保留天数 |

完整配置说明见 [configuration.md](configuration.md)。

---

## 升级与回滚

### 升级步骤

```bash
# 1. 备份当前状态
pmp-admin backup /tmp/pre-upgrade-backup

# 2. 替换二进制
cp phira-mp-plus-server /usr/local/bin/
systemctl restart pmp

# 3. 验证
pmp-admin status
journalctl -u pmp -n 50
```

### 回滚步骤

```bash
# 1. 恢复旧二进制
cp phira-mp-plus-server.bak /usr/local/bin/
systemctl restart pmp

# 2. 如需恢复数据
pmp-admin backup restore /tmp/pre-upgrade-backup
```

### 迁移注意事项

- 数据库 migration 是版本化的，新版本会自动运行未应用的 migration
- 回滚时若已运行不可逆 migration，需手动处理
- WAL 格式向后兼容（当前版本 v1）

---

## 容量规划

### 参考指标

| 场景 | 会话数 | 内存 | CPU |
|------|--------|------|-----|
| 小型部署 | ≤ 100 | 256 MB | 1 核 |
| 中型部署 | 500 | 1 GB | 2 核 |
| 大型部署 | 2000+ | 4 GB | 4 核 |

### 关键资源

- **数据库连接池**：默认 20 连接，高并发下需增加
- **插件内存**：每个插件上限 64 MB，10 个插件用满可能 640 MB
- **文件数**：`data/` + `plugins/` + WAL 文件，通常 < 1000

---

## 排障指南

### 服务器无法启动

```bash
# 检查配置
pmp-admin check-config

# 检查端口占用
ss -tlnp | grep 12346

# 查看日志
journalctl -u pmp -n 100 --no-pager
```

### 玩家无法连接

1. `pmp-admin status` 确认服务器运行
2. `pmp-admin rooms` 查看房间列表
3. 检查防火墙端口
4. 检查认证服务可用性

### 持久化问题

- 数据库连接失败：检查 `database_url` 和 PostgreSQL 状态
- WAL 损坏：日志会输出 WAL 错误，按提示删除 `.wal.instance`（谨慎操作）
- Dead-letter 写入失败：检查 `data/persistence-dead-letter.jsonl` 权限

### 插件问题

```bash
plugin list          # 查看插件状态
plugin info <name>   # 查看详情和错误
plugin disable <name> # 临时禁用
plugin reload <name>  # 热重载
```

---

## 事故处理

### 1. 数据库连接丢失

**症状**：PersistenceWorker 日志持续报数据库错误

**处理**：
1. 检查 PostgreSQL 状态：`systemctl status postgresql`
2. 数据库恢复后 PMP 自动重试并恢复
3. 如自动恢复失败：`systemctl restart pmp`

### 2. WAL 损坏

**症状**：启动时 `WAL replay failed — persistence worker cannot start`

**处理**：
1. 确认所有 admission 已处理（查看日志）
2. 如有 `persistence-dead-letter.jsonl`，确认死信已处理
3. 手动移除 `.wal.instance` 标记文件
4. 重启 PMP（WAL 记录无法恢复的执行重放）

### 3. 磁盘空间不足

**症状**：WAL admission 被拒绝，日志 `low disk space`

**处理**：
1. `df -h` 确认磁盘使用
2. 清理过期数据：调整 `persistence_retention_days`
3. 手动清理：`journalctl --vacuum-time=7d`
4. 扩展磁盘或挂载更大的数据目录

### 4. 插件引发性能问题

**症状**：CPU 高、事件队列积压

**处理**：
1. `plugin list` 确认哪些插件活动
2. 逐个 `plugin disable` 定位问题插件
3. 检查插件日志和 `wasm_runtime` 配置
4. 降低 `fuel_per_call` 或 `max_event_concurrency`

---

## HighFrequency Flush/Shutdown

`HighFrequencyWriter` 用于 Touch/Judge 高频遥测（绕过 WAL，直接 PostgreSQL COPY）。

### Sequence 跟踪

- `admission_sequence` — 下一个待分配序号（从 1 开始）
- `last_accepted_sequence` — 最后成功进入 main/overflow 队列的序号（fetch_max 并发安全）
- `committed_sequence` — 已提交的最高序号
- `continuous_committed_watermark` — 从 1 开始连续已提交的最高序号（基于 interval set 合并）

### Flush target

Flush 使用 `last_accepted_sequence` 作为 target，避免等待不存在的序号。
Dropped 的序号进入 `dropped_range`，Flush 检测到 drop gap 时返回 `DataLoss`。

### Shutdown

Shutdown 以 `usize::MAX` 为 limit 循环 drain overflow，确保全部 accepted item 被处理。

### Retry

重试循环使用 `retry_max_age_ms` 作为硬截止时间（默认 30s），超时后放弃，不是固定 `max_retries` 次。
