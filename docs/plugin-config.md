# WASM 插件配置规范

## 问题

当前配置散落在：

| 配置源 | 文件 | 管理方式 | 热重载 |
|--------|------|----------|--------|
| 服务端核心 | `server_config.yml` | CLI `config reload` | ✅ |
| 扩展 KV 存储 | `data/extensions.json` | `ext.get/set` | ✅ |
| 插件自有配置 | 无标准位置 | 各自为政 | ❌ |

WASM 插件缺乏标准方式来声明、读取和热更新配置。

## 设计方案

### 1. 插件配置目录结构

每个 WASM 插件的配置放在统一路径：

```
data/plugins/<plugin-name>/
├── config.json         # 插件配置（JSON，热重载）
├── data.json           # 插件持久化数据（运行时写入）
└── schema.json         # 配置 JSON Schema（可选）
```

`config.json` 由用户手动创建或通过 CLI 写入。`data.json` 由插件运行时管理（已有 `file.read/write`）。

### 2. WIT ABI 扩展

在 `wit/phira-plugin.wit` 中新增 `phira-config` interface：

```wit
/// Plugin configuration interface.
interface phira-config {
    use phira-types.{api-result};

    /// Get a configuration value by key path (e.g. "api.timeout").
    /// Returns null if the key does not exist.
    get-config: func(key-path: string) -> api-result;

    /// Set a configuration value by key path.
    /// The value is persisted to config.json and takes effect immediately.
    set-config: func(key-path: string, value: string) -> api-result;

    /// List all configuration keys at the given prefix.
    list-config: func(prefix: string) -> api-result;

    /// Reload config.json from disk.
    reload-config: func() -> api-result;

    /// Listen for configuration changes.
    /// Returns keys that have changed since the last poll.
    poll-config-changes: func(since-version: u64) -> api-result;
}
```

### 3. CLI 管理命令

在 CLI 中新增 `plugin config` 子命令家族：

```
plugin config list <plugin-name>          # 列出插件配置项
plugin config get <plugin-name> <key>     # 查看配置值
plugin config set <plugin-name> <key> <value>  # 设置配置值
plugin config reload <plugin-name>        # 从磁盘重载配置
```

### 4. 热重载集成

`LiveConfig` 结构扩展插件配置支持：

```rust
pub struct LiveConfig {
    // ... 现有字段 ...
    /// Per-plugin configuration stores (key-value, JSON).
    pub plugin_configs: HashMap<String, Arc<RwLock<serde_json::Map<String, Value>>>>,
}
```

`config reload` 命令自动重载所有插件的 `config.json`。

### 5. 文件格式示例

`data/plugins/my-plugin/config.json`：

```json
{
    "api": {
        "timeout": 30,
        "retry": 3
    },
    "display": {
        "title": "My Plugin",
        "color": "#ff6600"
    },
    "features": {
        "enable_chat": true,
        "max_items": 100
    },
    "_version": 1
}
```

### 6. 实现路径

| 步骤 | 内容 | 优先级 |
|------|------|--------|
| 1 | 新增 `docs/plugin-config.md`（本文档） | P0 |
| 2 | 定义 `phira-config` WIT interface | P0 |
| 3 | 服务端实现 `get-config` / `set-config` host 函数 | P1 |
| 4 | 新增 `plugin config` CLI 命令 | P1 |
| 5 | 配置变更事件（`poll-config-changes`） | P2 |
| 6 | JSON Schema 校验（`schema.json`） | P2 |

### 7. 与现有系统的关系

```
server_config.yml  ──→ LiveConfig (热重载)
                           │
                    plugin_configs: HashMap
                           │
              ┌────────────┼────────────┐
              │            │            │
         plugins/A    plugins/B    plugins/C
         config.json  config.json  config.json
```

- 插件配置**不和** `server_config.yml` 合并，避免命名空间冲突
- 每个插件独立文件，方便容器挂载/ConfigMap
- `set-config` 写入 `data/plugins/<name>/config.json`，下同读取
- 无 `schema.json` 时不做校验，有则验证写入值
