# 事故处理手册

## WAL replay 失败

**症状：** 启动日志显示 `persistence-wal` critical failure

**处理：**
1. 检查 WAL 文件完整性：`wal inspect`
2. 检查磁盘空间：`df -h data/`
3. 如 WAL 损坏且无法恢复，删除 `.wal.instance` 标记文件后重启
4. 重启后验证事件完整性

## 数据库不可用

**症状：** `DB unavailable/retry exhausted`

**处理：**
1. 检查 PostgreSQL：`systemctl status postgresql`
2. 检查连接：`psql -h localhost -U user -d phira_mp_plus`
3. 如 DB 短时间内不可恢复，配置 `allow_database_degraded_mode: true`
4. DB 恢复后关闭 degraded 模式并重启

## 磁盘写满

**症状：** WAL 写入失败日志

**处理：**
1. 检查磁盘：`df -h`
2. 清理旧日志：`rm log/*.log`
3. 手动 compact WAL
4. 扩容磁盘或配置日志轮转

## 插件 quarantine

**症状：** 插件被自动 quarantine

**处理：**
1. `plugin list` 查看 quarantined 插件
2. 检查日志确定原因（超时/trap）
3. 如正常：`plugin enable <name>`
4. 如反复 quarantine：移除插件并联系开发者
