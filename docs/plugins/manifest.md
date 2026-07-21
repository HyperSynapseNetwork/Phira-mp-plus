# 插件 Manifest

插件由 `.wasm` 文件和同目录的 `{name}.capabilities.json` 构成。

## capabilities.json

```json
{
    "http": true,
    "crypto": false,
    "federation": false,
    "storage": true,
    "send": false,
    "max_concurrent_calls": 1
}
```

| 权限 | 说明 |
|------|------|
| `http` | 注册 HTTP 路由 |
| `crypto` | 调用 sign/verify/sha256 |
| `federation` | 使用联邦连接 |
| `storage` | 读写扩展数据 |
| `send` | 发送聊天消息 |
| `max_concurrent_calls` | 并发 API 调用数（默认 1） |

## 构建与部署

```bash
# 构建
cargo build --target wasm32-unknown-unknown --release
wasm-tools component new \
  target/wasm32-unknown-unknown/release/my_plugin.wasm \
  -o my_plugin.component.wasm

# 部署
cp my_plugin.component.wasm plugins/
cp my_plugin.capabilities.json plugins/
```

## 热加载

```bash
# 重载单个插件
plugin reload my_plugin

# 重载所有插件
plugin reload
```
