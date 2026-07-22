# 运维手册

## 备份与恢复

### 创建备份

```bash
pmp backup /path/to/backup/dir
```

备份内容：
- `data/` 目录（扩展数据、插件数据）
- `plugins/` 目录（插件文件 + 能力文件）
- `server_config.yml`

### 验证备份

```bash
pmp backup verify /path/to/backup/dir
```

### 恢复

手动将备份文件解压到目标目录，重启 PMP。

> 注意：当前备份不含自动恢复机制。需确保目标目录配置与备份时一致。

---

## 配置参考

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
pmp backup /tmp/pre-upgrade-backup

# 2. 替换二进制
cp phira-mp-plus-server /usr/local/bin/
systemctl restart pmp

# 3. 验证
pmp status
journalctl -u pmp -n 50
```

### 回滚步骤

```bash
# 1. 恢复旧二进制
cp phira-mp-plus-server.bak /usr/local/bin/
systemctl restart pmp

# 2. 如需恢复数据
pmp backup restore /tmp/pre-upgrade-backup
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
pmp check-config

# 检查端口占用
ss -tlnp | grep 12346

# 查看日志
journalctl -u pmp -n 100 --no-pager
```

### 玩家无法连接

1. `pmp status` 确认服务器运行
2. `pmp rooms` 查看房间列表
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
