# 威胁模型

## 信任边界

```
PMP（受控网络内运行）
                                                          ├── PostgreSQL
                                                          ├── WASM 插件 (可信)
                                                          └── 运营 CLI
```

## 假设

- PMP 只监听受控网络（默认 127.0.0.1）
- 插件是运营方审核后加载的**可信**代码
- 数据库在受控网络中

## 威胁与缓解

| 威胁 | 影响 | 缓解 |
|------|------|------|
| 恶意游戏客户端 | 作弊、破坏房间 | 服务端权威验证、限速 |
| 被盗 token | 冒充用户 | 无持久 token，每次认证 |
| DB 凭据泄漏 | 数据泄露 | ENV/FILE 注入 + 脱敏配置 |
| 失控插件 | 进程崩溃 | fuel、memory limit、quarantine |
| WAL 损坏 | 数据丢失 | checksum + fail-closed + backup |
| 供应链攻击 | 恶意依赖 | cargo audit/deny + 锁定文件 |
