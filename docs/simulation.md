# Simulation 压力测试

Simulation 是 Phira-mp+ 的默认推荐压测工具，在隔离的 shadow world 中运行，**默认不访问 Phira 官方服务**，不需要 token，不需要真实账号。

## 核心特性

- **默认隔离** — 所有操作在 shadow world 中执行，不影响真实用户、房间、数据
- **无需认证** — 不需要 Phira token、账号、密码
- **确定性种子** — 指定 `seed` 可复现相同 Touches/Judges 数据，用于回归测试
- **多种场景** — Balanced / ChatStorm / ReadyStorm / RoundStorm / TouchJudgeBurst
- **预置配置** — Baseline / Small / Medium / Large / Custom
- **数据自清理** — `simulation cleanup` 可重置所有 shadow 状态
- **支持 Web / WIT / TUI / 插件调用**

## 快速开始

```bash
# 查看当前状态
simulation status

# 运行基线测试（5 用户，2 房间，60 秒）
simulation run baseline

# 运行小规模测试
simulation run small

# 停止运行
simulation stop

# 清理数据
simulation cleanup
```

## 预设配置

| 预设 | 用户 | 房间 | 时长（秒） | 用途 |
|------|------|------|-----------|------|
| Baseline | 20 | 5 | 60 | 快速验证 |
| Small | 100 | 10 | 120 | 轻量负载 |
| Medium | 500 | 50 | 300 | 中等压力 |
| Large | 1500 | 150 | 600 | 重型测试 |
| Custom | 按需 | 按需 | 按需 | `simulation run custom users=N rooms=N duration=N` |

## 场景说明

| 场景 | 行为 |
|------|------|
| `balanced` | 混合聊天/准备/回合/触摸/判定（默认） |
| `chat_storm` | 高频率聊天消息 |
| `ready_storm` | 频繁准备/取消准备 |
| `round_storm` | 快速开始/结束轮次 |
| `touch_judge_burst` | 集中 Touch/Judge 数据 |
| `idle` | 连接但不操作 |

## 自定义配置

```bash
simulation run custom users=500 rooms=50 duration=300 \
  scenario=touch_judge_burst tick_ms=1000 persist_every=30
```

## 数据隔离

- Simulation 不创建真实房间，不写入真实 `mp_round_player_data`
- 所有 events 带有 `scope = "simulation"` 和唯一 `run_id`
- 持久化走独立 `mp_sim_events` 表
- `cleanup` 重置所有 shadow users/rooms/rounds/counters

## Deterministic Seed

```bash
simulation seed 114514
simulation run baseline
```

相同 seed 产生完全相同的 sample touches/judges，适合：

- 回归测试
- 负载对比
- CI 验证

## 高级用法

```bash
# 查看 sample touches/judges 规模和分布
simulation sample

# 手动推进 tick
simulation tick 10

# 查看 shadow 数据样本
simulation inspect 20

# 运行 Suite（多个 scenario 按顺序）
simulation suite smoke
simulation suite mixed
simulation suite stress
```


---

# 压测

# Real Benchmark — 真实网络压测

> ⚠️ **advanced / explicit 模式**
> Real Benchmark 是显式真实 TCP 网络测试，需要 Phira token，**不推荐对 Phira 官方服务频繁压测**。
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
| `real` | 显式真实 TCP 网络测试，需要 Phira token |
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
