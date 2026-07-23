//! Top-level benchmark runner
//!
//! 顶层基准测试调度器。根据 BenchmarkConfig 中的模式（Simulation / Real），
//! 分发到模式特定的 runner。负责启动、监控、指标采集和报告生成。

use crate::benchmark::command::BenchmarkRunArgs;
use crate::benchmark::config::BenchmarkConfig;
use crate::benchmark::environment::EnvironmentSnapshot;
use crate::benchmark::metrics::BenchmarkMetrics;
use crate::benchmark::report::BenchmarkReport;
use serde::{Deserialize, Serialize};
use std::sync::Arc;

/// 基准测试运行器状态
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunnerState {
    /// 未启动
    Idle,
    /// 预热中
    WarmingUp,
    /// 运行中
    Running,
    /// 正在停止
    Stopping,
    /// 已完成
    Completed,
    /// 出错中止
    Failed,
}

/// 基准测试运行器
///
/// 顶层调度器，负责：
/// 1. 解析 args 生成 BenchmarkConfig
/// 2. 采集环境快照
/// 3. 分发到 Simulation 或 Real 模式 runner
/// 4. 周期采集指标
/// 5. 生成 BenchmarkReport
pub struct BenchmarkRunner {
    config: BenchmarkConfig,
    state: RunnerState,
}

impl BenchmarkRunner {
    /// 从运行参数创建新的运行器
    pub fn from_args(args: BenchmarkRunArgs) -> Self {
        let config = Self::build_config(&args);
        Self {
            config,
            state: RunnerState::Idle,
        }
    }

    /// 从配置直接创建运行器
    pub fn new(config: BenchmarkConfig) -> Self {
        Self {
            config,
            state: RunnerState::Idle,
        }
    }

    /// 构建配置（从 args 合并预设参数）
    fn build_config(args: &BenchmarkRunArgs) -> BenchmarkConfig {
        let mut config = BenchmarkConfig::from_preset(args.preset);
        config.mode = args.mode;
        config.scenario = args.scenario;
        config.clients = args.clients;
        config.rooms = args.rooms;
        config.duration = args.duration;
        config.seed = args.seed;
        config.plugins = args.plugins.clone();
        config
    }

    /// 运行基准测试
    ///
    /// TODO: 实现完整的运行循环：
    /// 1. 采集环境快照
    /// 2. 根据模式分发到 mode::simulation::run() 或 mode::real::run()
    /// 3. 启动指标采集循环
    /// 4. 等待运行完成或超时
    /// 5. 生成并返回 BenchmarkReport
    pub async fn run(&mut self) -> Result<BenchmarkReport, String> {
        self.state = RunnerState::Running;

        // 采集环境快照
        let environment = EnvironmentSnapshot::capture().await;

        let report = match self.config.mode {
            crate::benchmark::command::BenchmarkRunMode::Simulation => {
                self.run_simulation().await?
            }
            crate::benchmark::command::BenchmarkRunMode::Real => {
                self.run_real().await?
            }
        };

        self.state = RunnerState::Completed;
        Ok(report)
    }

    /// 仿真模式运行
    ///
    /// TODO: 委托到 `crate::simulation::SimulationManager` 或
    /// `modes::simulation::run_simulation_scenario()`。
    async fn run_simulation(&self) -> Result<BenchmarkReport, String> {
        // TODO: 实现仿真模式运行
        Err("Simulation mode runner not yet implemented".to_string())
    }

    /// 真实模式运行
    ///
    /// TODO: 启动真实 PMP 服务，使用 `phira_client` 连接模拟客户端。
    async fn run_real(&self) -> Result<BenchmarkReport, String> {
        // TODO: 实现真实模式运行
        Err("Real mode runner not yet implemented".to_string())
    }

    /// 返回当前状态
    pub fn state(&self) -> RunnerState {
        self.state
    }

    /// 返回配置引用
    pub fn config(&self) -> &BenchmarkConfig {
        &self.config
    }
}
