# PMP 产品概览

**Phira-mp+（PMP）** 是增强版多人游戏服务端。

### 核心功能

- **游戏协议运行时** — TCP 会话管理、房间状态机、轮次生命周期
- **WASM 插件系统** — WIT ABI v2 组件模型，逐插件 capability/fuel/内存隔离
- **可靠持久化** — WAL + PostgreSQL + dead-letter，崩溃恢复保证
- **插件 TCP 连接 API** — 纯 TCP connect/listen/send/close

### 架构

```
PMP (游戏运行时) → PostgreSQL
```

单进程部署，HTTP/SSE/WS 端口仅用于内部集成，不直接暴露公网。

---

# 兼容矩阵

> 英文版：[Compatibility Matrix (English)](product/compatibility-matrix.en.md)

### PMP 版本兼容

| PMP 版本 | WIT ABI | 插件 SDK | PostgreSQL | 客户端协议 |
|----------|---------|----------|------------|------------|
| 0.4.x | v2 | 0.4.x | 15+ | phira-mp 协议 |  

### WASM 运行时

| 运行时 | 版本 | 说明 |
|--------|------|------|
| wasmtime | 36.x | 组件模型 |
| wit-bindgen | 0.58 | 代码生成 |
