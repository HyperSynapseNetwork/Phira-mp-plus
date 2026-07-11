# 静态验证记录

## 验证范围

本轮覆盖 PMP 的连接接入、Session/Room 命令、插件/WIT、持久化、Supervisor、关机、配置和相关文档。结构化检查统计：

- Rust 源文件：126 个；
- Cargo TOML：8 个；
- YAML：4 个；
- JSON：1 个。

## 已完成的静态检查

- 8 个 `Cargo.toml` 均可由 TOML 解析器读取；4 个 YAML 与 1 个 JSON 均可解析。
- 126 个 Rust 文件通过注释、普通/原始字符串、字符字面量和生命周期感知的分隔符扫描。
- 新增配置字段在默认值、YAML 解析、校验、示例配置和文档中保持同名。
- capability 检查位于真实逐插件 host 调用路径；未知方法默认拒绝。
- Wasmtime fuel 与 Store limiter 已接入实例化和每次 guest 调用路径。
- 持久化和插件事件的 Flush/Shutdown 均有顺序控制消息与 acknowledgment。
- Session/Room mailbox 在命令已入队后不再因 reply 丢失或超时重放副作用。
- disconnect 不再同时走直接插件回调和 EventBus 二次回调。
- 网络流构造器中的重复 `version` 字段初始化已在静态复核中发现并修复。
- README、配置文档和架构报告统一声明 PPB/PMP 边界；可信转发头端口明确不是 HAProxy PROXY v1/v2。

## 未能执行的验证

当前执行环境没有 Rust/Cargo 工具链，且无法解析 Rust 官方下载域名，因此没有执行：

```bash
cargo fmt --check
cargo check --workspace --all-targets
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
```

分隔符扫描只能发现词法/结构类错误，不能替代 Rust 编译器的名称解析、trait 约束、借用检查、条件编译和依赖 API 校验。因此交付状态是“代码已修改并完成静态审阅”，不是“已证明编译与测试全部通过”。

## 合并前必须执行的 CI 门禁

```bash
cargo fmt --check
cargo check --workspace --all-targets
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo audit
```

还应增加慢认证、会话容量竞态、慢消费者、mailbox reply 丢失、SIGTERM、数据库中断、WASM 无限循环/内存增长/越权/超时、插件重载，以及至少 6 小时 soak test。

## 结论边界

本轮消除了此前审计中最直接的拒绝服务、重复副作用、权限绕过、静默队列丢弃和假监督路径。但在 Rust 编译与动态回归通过前，不应标记为“生产验证完成”；Room 全状态 Actor 化、崩溃一致性 WAL 和不可信插件进程级隔离仍是结构性后续工作。
