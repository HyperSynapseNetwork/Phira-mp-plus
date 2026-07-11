# PMP 架构加固报告索引

第一阶段与第二阶段修改已经合并到当前工程。最新、可作为交付依据的说明见：

- [`PHASE2_HARDENING_REPORT.md`](PHASE2_HARDENING_REPORT.md)：第二阶段实现、保证边界、残余结构性问题与结论；
- [`docs/architecture-hardening.md`](docs/architecture-hardening.md)：运行时不变量与架构边界；
- [`docs/static-verification.md`](docs/static-verification.md)：本地静态/工具链验证及未完成的动态验证；
- [`docs/architecture-change-log.md`](docs/architecture-change-log.md)：逐阶段变更记录。

PMP 在 PP 架构中仍定位为 PPB 后方的受控游戏服务端。公共 Web API、统一认证、TLS、边缘限流和公网治理由 PPB 提供；PMP 现有 HTTP/SSE/WebSocket 保留为内部兼容、诊断和插件接口。

当前状态是 **hardening candidate**，不是 production verified。核心剩余边界仍是：Room 全状态尚未完全 Actor-owned、没有 enqueue-before WAL/replay、进程内插件没有操作系统级硬隔离，以及真实 PostgreSQL/并发/负载/soak 验证尚未完成。
