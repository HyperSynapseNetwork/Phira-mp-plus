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

本环境已从可用镜像组装 Rust 1.88 的 `rustc`、`cargo`、`rustfmt` 和 `clippy`。相对上传基线发生变化的 Rust 文件已逐一通过：

```bash
rustfmt --edition 2021 --check <changed-files>
```

同时成功执行 `cargo metadata --locked --no-deps --offline`。真实命令：

```bash
cargo check --locked --workspace --all-targets
```

已经启动，但因环境无法解析 `index.crates.io`、且本地没有完整依赖缓存，在线模式在获取 `anyhow` 前因 `index.crates.io` DNS 失败而终止；离线模式则明确报告本地缺少 `axum`。因此尚未获得 Rust 名称解析、trait、借用检查、条件编译和依赖 API 层的通过证明，也未实际执行 workspace tests 或 Clippy。

全仓 `cargo fmt --check` 还会命中上传基线已有的大量格式债。为避免本轮架构补丁产生大面积无关格式 diff，CI 当前对**本次变化的 Rust 文件**执行 rustfmt 门禁；全仓格式债应以独立提交清理。

## 合并前必须执行的 CI 门禁

```bash
scripts/check-changed-rustfmt.sh
cargo check --locked --workspace --all-targets
cargo check --locked -p phira-mp-plus-server --no-default-features
cargo check --locked -p phira-mp-plus-server --no-default-features --features postgres
cargo check --locked -p phira-mp-plus-server --no-default-features --features wit-bindgen
cargo test --locked --workspace
cargo clippy --locked --workspace --all-targets
cargo audit
```

还应增加慢认证、会话容量竞态、慢消费者、mailbox reply 丢失、SIGTERM、数据库中断、WASM 无限循环/内存增长/越权/超时、插件重载，以及至少 6 小时 soak test。

## 结论边界

本轮消除了此前审计中最直接的拒绝服务、重复副作用、权限绕过、静默队列丢弃和假监督路径。但在 Rust 编译与动态回归通过前，不应标记为“生产验证完成”；Room 全状态 Actor 化、崩溃一致性 WAL 和不可信插件进程级隔离仍是结构性后续工作。


## 第二阶段静态验证补充

本阶段再次解析 8 个 Cargo TOML、示例 YAML 和 JSON，并对 126 个 Rust 文件执行注释/字符串/raw-string/字符字面量/生命周期感知的分隔符扫描。另对建表语句与 INSERT/UPDATE 字段进行静态交叉检查；动态 `{field}` 遥测更新语句除外。

静态检查发现并修复了额外数据库契约问题、配置测试标注错误、无数据库 RoundStore fallback 和在线时长单位错误。由于 crates.io 依赖解析仍受阻，以上结果不能替代 Rust 类型系统、SQLx 运行时和真实 PostgreSQL 迁移验证。


### 第二阶段新增契约检查

- Session/Room 有序业务路径中不存在可执行的 direct/inline fallback；旧 fallback 字段只保留为诊断结构兼容。
- `worker_authoritative` 的配置校验、运行时切换校验和 PostgreSQL 初始化失败路径保持一致，不允许静默降级。
- Touch/Judge、普通事件、Simulation 与 Benchmark 的重试使用稳定幂等键；相关表具有对应唯一索引。
- PostgreSQL 建表字段与静态 INSERT/UPDATE 字段进行了交叉扫描；唯一剩余报警来自运行时受控的动态遥测字段名，不是表字段缺失。
- dead-letter 负载覆盖所有数据型 `PersistenceEvent`，Flush/Shutdown 控制标记明确不进入 dead-letter。
- Supervisor 在自身命令通道已关闭时仍通过进程级关键失败计数保留 degraded 信号。

- Room mailbox/snapshot cleanup is UUID-generation guarded; stale Room Actor cleanup cannot remove a replacement room generation.
- Supervisor cleanup is generation guarded so a completed old supervisor cannot clear a newly initialized process handle.

- Room Actor snapshots carry `room_uuid`; orphaned/replaced actors perform bounded lifecycle revalidation before cleanup.

## Tree-sitter syntax comparison

All 126 Rust files were parsed with `tree-sitter-rust`. The modified tree produced the same 11 parser-compatibility nodes as the uploaded baseline (`&raw` syntax and project macro `~` tokens) and introduced no new `ERROR` or missing nodes. This is stronger than delimiter scanning but still does not perform Rust name/type/borrow checking.

- GitHub Actions YAML for Build and Release was parsed structurally after the quality-gate rewrite.
- Workspace package versions and both corresponding Cargo.lock entries are checked by `scripts/sync-workspace-version.py --check`; the stale `0.4.0` lock entries were corrected to `0.4.169`.

## Local Rust toolchain validation

A Rust 1.88 toolchain (`rustc`, `cargo`, `rustfmt`, `clippy`) was assembled locally. All 42 Rust files changed relative to the uploaded baseline were formatted and passed direct `rustfmt --edition 2021 --check`. A real `cargo check --locked --workspace --all-targets` was started but dependency resolution could not proceed because this environment cannot resolve `index.crates.io`; therefore name/type/borrow checks and tests remain delegated to the mandatory CI quality gate.
