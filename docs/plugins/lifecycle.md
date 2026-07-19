# 插件生命周期

## 状态机

```
加载 → 验证 → 启用 → 运行 → 禁用 → 移除
                 ↓
              quarantine (故障隔离)
```

## 加载

PMP 启动时扫描 `plugins_dir` 目录下的 `.wasm` 文件：

```bash
# 手动重新加载
plugin reload
plugin reload <name>
```

加载后插件处于 `Enabled` 状态，开始接收事件。

## 权限（Capability）

插件必须在 `.wasm` 同级目录放置 `{name}.capabilities.json`：

```json
{
    "http": true,
    "storage": true,
    "admin": false,
    "network": false
}
```

默认拒绝所有权限。

## 资源限制

| 限制 | 默认值 | 说明 |
|------|--------|------|
| 燃料 | 10,000,000 | 每次调用消耗，耗尽后 trap |
| 内存 | 64 MB | 线性内存上限 |
| 超时 | 2000 ms | 同步调用超时 |
| 并发 | 8 | 同事件内最大并行插件数 |

## 故障隔离 (Quarantine)

插件连续超时或 trap 后进入 quarantine 状态：

- 不再接收新事件
- 已有事件继续处理
- 运营方可通过 CLI 恢复或移除

## 移除 vs 清理

```bash
# 从注册表移除（保留 .wasm 和数据）
plugin remove <name>

# 彻底清理文件和数据
plugin purge <name>
```

`remove` 只禁用和卸载，`purge` 才删除文件。生产环境建议先 remove，确认不影响业务后再 purge。

## 热重载

```text
验证新版本 → 初始化 shadow runtime → 健康检查 → 原子切换 → 回滚旧版本
```

当前热重载不中断正在进行的插件调用。
