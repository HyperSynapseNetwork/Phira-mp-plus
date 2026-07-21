# 排障指南

## 服务器无法启动

```
phira-mp-plus-server --config /etc/pmp/server_config.yml
```

**检查项：**
1. 配置文件YAML格式：`check-config`
2. 端口被占用：`ss -tlnp | grep 12346`
3. WAL 损坏：检查日志中 `persistence-wal-consistency`
4. 数据库无法连接：检查 `database_url` 和 PostgreSQL 状态

## 数据库连接失败

```
✗ PostgreSQL 连接失败
```

**解决：**
1. 确认 PostgreSQL 运行中：`systemctl status postgresql`
2. 检查连接串：`database_url` 格式是否正确
3. 检查网络：`psql -h localhost -U user -d phira_mp_plus`
4. 如无需数据库，移除 `database_url` 配置或用 `allow_database_degraded_mode: true`

## WAL 相关问题

```
WAL entry not ACKed (non-durable outcome); will replay on restart
```

**检查：**
- `wal inspect` 查看 WAL 大小
- `dead-letter list` 查看失败事件
- 如 WAL 持续增长，检查数据库写入路径

## 插件加载失败

```
plugin '{name}' not found
```

**解决：**
1. 确认 `.component.wasm` 文件在 `plugins/` 目录
2. 确认 WASM 是用 `wasm-tools component new` 构建的
3. 确认 WIT ABI 版本兼容
