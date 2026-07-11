# WASM 插件配置规范

> 当前状态：WIT `phira-config` 接口与宿主文件型实现已经接线；配置写入受逐插件 `config` capability 约束。CLI `plugin config` 子命令、JSON Schema 校验和具体变更键通知尚未实现。

## 存储布局

每个插件使用独立命名空间：

```text
data/plugins/<plugin-id>/
├── config.json         # 插件配置
├── data.json           # 插件私有数据
└── schema.json         # 预留；当前不执行 Schema 校验
```

`plugin-id` 使用插件文件的稳定标识，而不是可变的显示名称。配置键必须是由字母、数字、`_`、`-` 组成的点分路径，例如 `api.timeout`；空段、控制字符和超长键会被拒绝。

## WIT 接口

```wit
interface phira-config {
    use phira-types.{api-result};
    get-config: func(key-path: string) -> api-result;
    set-config: func(key-path: string, value: string) -> api-result;
    list-config: func(prefix: string) -> api-result;
    reload-config: func() -> api-result;
    poll-config-changes: func(since-version: u64) -> api-result;
}
```

当前语义：

- `get-config`、`list-config` 读取插件自己的 `config.json`。
- `set-config` 采用临时文件后重命名的方式写入，并要求 `config` capability。
- `reload-config` 验证磁盘 JSON 可读取、可解析；当前没有独立长期内存缓存需要替换。
- `poll-config-changes` 以文件修改时间作为版本信号，不返回精确 changed keys。
- 插件不能通过配置键或文件路径逃逸到其他插件命名空间。

## Capability

插件旁可放置 `<plugin>.capabilities.json`：

```json
{
  "capabilities": ["state.read", "send", "config"]
}
```

未知 capability 会拒绝插件加载。`set-config`、`reload-config` 等配置写路径必须具备 `config`；检查发生在真实 WIT/通用 host 调用边界，而不是仅存在于测试包装器。

## 示例

```json
{
  "api": {
    "timeout": 30,
    "retry": 3
  },
  "features": {
    "enable_chat": true,
    "max_items": 100
  }
}
```

## 尚未完成

| 项目 | 状态 | 影响 |
|---|---|---|
| `plugin config` CLI | 未实现 | 仍需直接编辑文件或由插件调用 WIT |
| `schema.json` 校验 | 未实现 | 宿主只保证 JSON 与键路径合法，不验证业务类型/范围 |
| 精确 changed keys | 未实现 | `poll-config-changes` 只能判断版本变化 |
| 全插件统一热重载 | 未实现 | 服务端 `config reload` 不自动重载每个插件配置 |
