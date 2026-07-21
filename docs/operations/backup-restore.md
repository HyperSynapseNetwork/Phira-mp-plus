# 备份与恢复

## PostgreSQL 备份

PMP 的持久化数据（事件、房间快照、游玩记录）存储在 PostgreSQL 中。
PostgreSQL 备份应使用 PG 生态的标准工具：

```bash
# pg_dump（逻辑备份，适合迁移和少量数据）
pg_dump -Fc -f phira_mp_plus_backup.pgr phira_mp_plus

# pg_dumpall（备份全部数据库和全局对象）
pg_dumpall > cluster_backup.sql
```

生产环境推荐使用 [pgBackRest](https://pgbackrest.org/) 或 [WAL-G](https://github.com/wal-g/wal-g)：

```bash
# pgBackRest 热备示例（需预先配置 pgbackrest.conf）
pgbackrest --stanza=phira_mp_plus --type=full backup

# 时间点恢复（PITR）
pgbackrest --stanza=phira_mp_plus --type=time "--target=2026-07-21 12:00:00" restore
```

这些工具支持在线热备、增量备份和时间点恢复（PITR），是 PG 社区的标准方案。

## 文件级备份

PMP 同时提供文件级备份工具，覆盖非 PG 数据：

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
