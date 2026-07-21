# 测试指南

## 运行测试

```bash
# 全部测试
cargo test --workspace --all-features

# 仅单元测试
cargo test --lib

# 仅集成测试
cargo test --test cli_command_contracts

# WAL 测试
cargo test --lib -- wal
```

## 测试类型

| 类型 | 位置 | 说明 |
|------|------|------|
| 单元测试 | `src/*/tests/` | 模块内部测试 |
| 集成测试 | `tests/*.rs` | 端到端契约测试 |
| WAL fuzz | `wal.rs` tests | 损坏/截断/并发 |
| 状态机 | `room.rs` tests | Room 状态转换 |
| 故障注入 | `wal.rs` fault_* | 删除/清零/并发 |
| WASM | `wasm_host.rs` tests | 插件 API 测试 |

## WASM fixture

WASM 测试依赖预编译组件。WIT 变更后需重建：

```bash
cargo build --package test-wasm-plugin --target wasm32-unknown-unknown
wasm-tools component new \
  target/wasm32-unknown-unknown/release/test_wasm_plugin.wasm \
  -o phira-mp-plus-server/tests/test-plugin.component.wasm
```

## 故障注入测试

```bash
cargo test --lib -- fault_
```

涵盖：WAL 删除、清零、并发 compact+admit、checksum 损坏。
