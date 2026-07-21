# 贡献指南

## 开发环境

参见 `docs/getting-started/quick-start.md`。

## 代码风格

- 遵循 Rust 2021 Edition 惯例
- `cargo clippy` 必须通过
- 提交前运行 `cargo test`

## 提交信息

```
<type>: <简短描述>

<详细说明（可选）>

Co-Authored-By: ...
```

类型：`feat` / `fix` / `docs` / `style` / `refactor` / `test` / `chore`

## PR 流程

1. Fork 仓库
2. 创建 feature 分支
3. 提交改动
4. 创建 Pull Request
5. CI 通过后合并

## 测试

- 单元测试：`cargo test --lib`
- 集成测试：`cargo test`
- WASM 插件测试需要重建 fixture
