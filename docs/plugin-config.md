# WASM 插件配置规范

> 状态：部分落地。`wit/phira-plugin.wit` 已声明 `phira-config` 接口，WIT host 已提供基于 `data/plugins/<plugin-name>/config.json` 的 `get/set/list/reload/poll` 基础实现；CLI 仍未提供 `plugin config` 子命令，schema 校验、统一热重载和 capability enforcement 仍是后续工作。

## 问题

当前配置散落在：

| 配置源 | 文件 | 管理方式 | 热重载 |
|--------|------|----------|--------|
| 服务端核心 | 启动时 `--config` 指定的 YAML | CLI `config reload` | 部分字段 |
| 扩展 KV 存储 | `data/extensions.json` | CLI `extension list/get`；写入由内部逻辑或插件 API 完成 | ✅ |
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

`wit/phira-plugin.wit` 已声明 `phira-config` interface。当前 WIT host 已提供文件型基础实现：

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

当前限制：

- `get-config` / `set-config` / `list-config` 直接读写 `config.json`。
- `reload-config` 当前只验证磁盘 JSON 是否可读可解析，不会刷新独立的长期内存缓存。
- `poll-config-changes` 当前返回配置文件修改时间作为版本指示，不返回具体变更 key。
- WIT 写能力仍需接入显式 capability 检查。

### 3. CLI 管理命令

后续可在 CLI 中新增 `plugin config` 子命令家族：

```
plugin config list <plugin-name>          # 列出插件配置项
plugin config get <plugin-name> <key>     # 查看配置值
plugin config set <plugin-name> <key> <value>  # 设置配置值
plugin config reload <plugin-name>        # 从磁盘重载配置
```

### 4. 热重载集成

后续实现时可扩展 `LiveConfig` 结构，加入插件配置支持：

```rust
pub struct LiveConfig {
    // ... 现有字段 ...
    /// Per-plugin configuration stores (key-value, JSON).
    pub plugin_configs: HashMap<String, Arc<RwLock<serde_json::Map<String, Value>>>>,
}
```

目标行为：`config reload` 命令自动重载所有插件的 `config.json`。当前 `config reload` 只重新读取启动时指定的服务端 YAML，并热更新聊天、monitor 及配置源明确提供的管理员/压测凭据；插件配置仍需各插件调用 `reload-config`。

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
| 2 | 定义 `phira-config` WIT interface | 已完成 |
| 3 | 服务端实现 `get-config` / `set-config` / `list-config` / `reload-config` / `poll-config-changes` host 函数 | 已部分完成 |
| 4 | 新增 `plugin config` CLI 命令 | P1 |
| 5 | 配置变更事件（具体 changed keys，而不仅是文件 mtime） | P2 |
| 6 | JSON Schema 校验（`schema.json`） | P2 |
| 7 | WIT config 写接口 capability enforcement | P0 |

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
