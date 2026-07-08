# MP+ Runtime v2

> **Final status** — Runtime v2 重构计划已完成。Step 38 closure gate 已关闭。
> 当前架构: actor-model blueprint + typed room command gateway + session dispatch mailbox。
> persistence worker + telemetry batcher + WIT-only plugin ABI (lifecycle wired, all host APIs implemented with capability enforcement, WASM integration tests pass)。
> Telemetry 模式: `direct_only` / `worker_preferred`。

## Final architecture

| 组件 | 状态 |
|------|------|
| Room actor | Owned — 全 7 命令 mailboxed, lock/cycle/host owned-tracked |
| Session actor | WriteRouted — 12 命令变体通过 mailbox |
| Server supervisor | ReadRouted — mailbox 骨架 |
| Persistence worker | 6/7 写入路径已迁移; round_store 直写为有意设计 |
| Phira HTTP | PhiraRetryClient 统一, 无裸 reqwest |
| EventBus | Runtime 脊椎, benchmark completed typed/cached |
| Plugin ABI | MIGRATION_PHASE 3, WIT-only, WASM 集成测试通过 |
| Simulation | 架构守卫, 18 个 contract tests 覆盖 |

## 关键决策

- **不要实现** privileged Web management API。
- **不要恢复** JSON ABI 为 current。
- **不要硬编码** 测试数量。
- round_store Touch/Judge 直写保持 permanent — 高频数据绕过 PersistenceWorker 为有意设计。
- 所有 objective 已 done。详细 objective 列表见历史 git 记录 `runtime_plan.rs` (已移除)。
