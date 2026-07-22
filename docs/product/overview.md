# PMP 产品概览

> 完整英文版：[Product Overview (English)](overview.en.md)

**Phira-mp+（PMP）** 是增强版多人游戏服务端，提供：

- **游戏协议运行时** — TCP 会话管理、房间状态机、轮次生命周期
- **WASM 插件系统** — WIT ABI v2 组件模型，逐插件 capability/fuel/内存隔离
- **可靠持久化** — WAL + PostgreSQL + dead-letter，崩溃恢复保证
- **插件 TCP 连接 API** — 纯 TCP connect/listen/send/close，无 TLS

### 架构

```
PMP (游戏运行时) → PostgreSQL
```

### 部署要点

- PMP 运行于可信内部网络
- HTTP/SSE/WS 端口仅用于内部集成，不直接暴露公网
- 单进程部署，无集群 HA

> 详细英文版：[Product Overview (English)](overview.en.md)
