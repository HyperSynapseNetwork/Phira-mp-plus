//! Top-level benchmark runner
//!
//! 顶层基准测试调度器。分发到 Real 模式 runner。负责启动、监控、指标采集和报告生成。

use crate::benchmark::command::BenchmarkRunArgs;
use crate::benchmark::config::BenchmarkConfig;
use crate::benchmark::environment::EnvironmentSnapshot;
use crate::benchmark::report::BenchmarkReport;
use crate::persistence::message::PersistenceEvent;
use crate::server::PlusServerState;
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
/// 3. 分发到 Real 模式 runner
/// 4. 周期采集指标
/// 5. 生成 BenchmarkReport
pub struct BenchmarkRunner {
    config: BenchmarkConfig,
    state: RunnerState,
    /// Optional server state reference (required for Real mode Mock Phira integration).
    pub server_state: Option<Arc<PlusServerState>>,
}

impl BenchmarkRunner {
    /// 从运行参数创建新的运行器
    pub fn from_args(args: BenchmarkRunArgs) -> Self {
        let config = Self::build_config(&args);
        Self {
            config,
            state: RunnerState::Idle,
            server_state: None,
        }
    }

    /// 从配置直接创建运行器
    pub fn new(config: BenchmarkConfig) -> Self {
        Self {
            config,
            state: RunnerState::Idle,
            server_state: None,
        }
    }

    /// 设置服务器状态引用（Real mode 需要）
    pub fn set_server_state(&mut self, state: Arc<PlusServerState>) {
        self.server_state = Some(state);
    }

    /// 构建配置（从 args 合并预设参数和 overrides）
    fn build_config(args: &BenchmarkRunArgs) -> BenchmarkConfig {
        let mut config = BenchmarkConfig::from_preset(args.preset);
        config.scenario = args.scenario;
        config.clients = args.clients;
        config.rooms = args.rooms;
        config.duration = args.duration;
        config.seed = args.seed;
        config.plugins = args.plugins.clone();
        for (key, val) in &args.overrides {
            match key.as_str() {
                "listen-addr" => config.listen_addr = Some(val.clone()),
                "mock-phira-delay" => {
                    if let Ok(v) = val.parse() {
                        config.mock_phira_delay_ms = v;
                    }
                }
                "mock-phira-jitter" => {
                    if let Ok(v) = val.parse() {
                        config.mock_phira_jitter_ms = v;
                    }
                }
                "mock-phira-error-rate" => {
                    if let Ok(v) = val.parse() {
                        config.mock_phira_error_rate = v;
                    }
                }
                "mock-phira-timeout" => {
                    if let Ok(v) = val.parse() {
                        config.mock_phira_timeout_ms = v;
                    }
                }
                _ => {}
            }
        }
        config
    }

    /// 运行基准测试
    ///
    /// TODO: 实现完整的运行循环：
    /// 1. 采集环境快照
    /// 2. 分发到 mode::real::run()
    /// 3. 启动指标采集循环
    /// 4. 等待运行完成或超时
    /// 5. 生成并返回 BenchmarkReport
    pub async fn run(&mut self) -> Result<BenchmarkReport, String> {
        self.state = RunnerState::Running;

        // 采集环境快照
        let _environment = EnvironmentSnapshot::capture().await;

        let report = self.run_real().await?;

        self.state = RunnerState::Completed;

        // Enqueue BenchmarkReport to persistence worker for mp_runtime_benchmark_reports table.
        if let Some(state) = &self.server_state {
            let _ = state
                .persistence_worker
                .enqueue(PersistenceEvent::BenchmarkReport {
                    report: report.clone(),
                })
                .await;
        }

        Ok(report)
    }

    /// 真实模式运行
    ///
    /// 委托到 `modes::real::run_real()`，连接真实 PMP 服务并执行
    /// 二进制协议认证与房间命令。
    async fn run_real(&self) -> Result<BenchmarkReport, String> {
        let state = self
            .server_state
            .as_ref()
            .ok_or_else(|| "Real mode requires server_state to be set via set_server_state()".to_string())?;
        // 传递 run_id 给每一个客户端，用于生成唯一的 token / user / room
        let run_id = uuid::Uuid::new_v4();
        let result = crate::benchmark::modes::real::run_real(self.config.clone(), state.as_ref(), run_id)
            .await?;
        Ok(result.report)
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
