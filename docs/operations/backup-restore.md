# 备份与恢复

## 创建备份

```bash
# CLI 命令（运行时）
backup create /var/backups/pmp

# 手动归档
tar czf pmp-backup-$(date +%s).tar.gz data/ server_config.yml plugins/*.capabilities.json
```

备份内容包括：配置文件、WAL、dead-letter、extension 数据。

## 验证备份

```bash
restore verify /var/backups/pmp
```

## PostgreSQL 备份（手动）

```bash
pg_dump -h localhost -U user -d phira_mp_plus > pmp-db-$(date +%s).sql
```

## 恢复步骤

1. 停止 PMP：`systemctl stop phira-mp-plus`
2. 恢复配置和数据：`tar xzf backup.tar.gz`
3. 恢复数据库（如需）：`psql -h localhost -U user -d phira_mp_plus < pmp-db.sql`
4. 启动 PMP：`systemctl start phira-mp-plus`
5. 验证：`doctor` + `check-config`
