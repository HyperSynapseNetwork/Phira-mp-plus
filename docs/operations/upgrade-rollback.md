# 升级与回滚

## 版本号方案

PMP 使用 SemVer（`major.minor.patch`）。

| 组件 | 版本位置 | 不兼容变更 |
|------|---------|-----------|
| 服务端 | `Cargo.toml` | Major 版本 = 可能不兼容 |
| 配置 schema | `config_version` 字段 | 配置格式不兼容时递增 |
| DB schema | `_pmp_schema_version` 表 | 迁移时递增 |
| 事件 schema | `MP_EVENT_SCHEMA_VERSION` | 事件格式变更时递增 |
| WIT ABI | `phira-plugin-v2` world 名 | world 改名 = 插件需重建 |
| 游戏协议 | `1`（稳定） | 不会变更 |

## 升级类型

| 类型 | 示例 | 可回滚 | 说明 |
|------|------|--------|------|
| Patch | `0.4.1` → `0.4.2` | ✅ | 仅 bug 修复，无 schema 变更 |
| Minor | `0.4.x` → `0.5.x` | ⚠️ | 检查 DB migration 向后兼容性 |
| Major | `0.x` → `1.0.0` | ❌ | 不兼容变更，需规划迁移 |

## 数据库迁移

- 迁移在启动时自动执行
- 每个迁移是 `migrations/` 下的编号 SQL 文件
- 已应用的迁移记录在 `_pmp_schema_version` 表
- 只进不退：回滚需要新迁移（而非撤销）
- 旧版本服务端可运行在已迁移的数据库上（只要忽略新列即可）

## 回滚步骤

1. 停止服务端
2. 恢复旧版本二进制文件
3. 恢复旧版本配置文件
4. 启动服务端
5. 验证：`check-config` + `doctor` 命令
6. 新版 WAL 与旧版兼容（仅追加格式）
7. DB schema 在同 patch 版本内向后兼容

## 升级前检查清单

- [ ] 阅读 CHANGELOG 了解不兼容变更
- [ ] 执行 `backup create` 备份当前状态
- [ ] 验证备份：`restore verify <路径>`
- [ ] 先在预发布环境测试升级
- [ ] 如迁移不向后兼容，规划回滚方案
