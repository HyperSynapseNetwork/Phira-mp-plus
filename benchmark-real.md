# Real Benchmark — 真实网络压测

> ⚠️ **advanced / explicit 模式**
> Real Benchmark 是真实 TCP 兼容性测试，需要 Phira token，**不推荐对 Phira 官方服务频繁压测**。
> 默认推荐使用 [Simulation](simulation.md) 进行压力测试（不需要 token，隔离运行）。

## 前提条件

需要 Phira 账号 token，可通过以下方式配置：

1. `server_config.yml` 的 `benchmark_phira_tokens` 字段
2. `data/benchmark-auth.json` 文件

```yaml
# server_config.yml
benchmark_phira_tokens:
  - "your-phira-token-1"
  - "your-phira-token-2"
```

## 使用

```bash
# 查看三种压测模式
benchmark modes

# 运行真实 TCP 压测（30 秒，100 房间）
benchmark run real 30 100

# 运行 Hybrid 探测
benchmark run hybrid authenticate=true chart_lookup=1
```

## 模式说明

| 模式 | 说明 |
|------|------|
| `simulation` | **默认推荐**。隔离 shadow world，不需要 token |
| `real` | 显式真实 TCP 兼容性测试，需要 Phira token |
| `hybrid` | Hybrid Phira 探测（chart_lookup / record_lookup） |

## 报告

```bash
# 查看最近报告
benchmark report

# 查看历史
benchmark history
```

## 安全提醒

- **不要将真实 token 提交到 Git** — `benchmark_phira_tokens` 已在 `.gitignore` 中推荐排除
- **不要对 Phira 官方服务器做高频压测**
- 建议使用 Simulation 作为日常压力测试工具
