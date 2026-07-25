//! Benchmark configuration types
//!
//! 基准测试配置结构体，由 CLI 参数（`BenchmarkRunArgs`）派生。
//! 包含模式特有配置、场景参数和全局运行时设置。

use crate::benchmark::command::{BenchmarkPreset, BenchmarkRunMode, BenchmarkScenario};
use serde::{Deserialize, Serialize};
use std::time::Duration;

/// 基准测试全局配置
///
/// 由 `BenchmarkRunArgs` 和预设参数合并生成，是 runner 和各场景的最终配置。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BenchmarkConfig {
    /// 运行模式
    pub mode: BenchmarkRunMode,
    /// 负载场景
    pub scenario: BenchmarkScenario,
    /// 预设名称
    pub preset: BenchmarkPreset,

    // ── 客户端 / 房间规模 ──
    /// 并发客户端数量
    pub clients: u32,
    /// 并发房间数量
    pub rooms: u32,
    /// 每个房间的最大成员数
    pub members_per_room: u32,

    // ── 时间控制 ──
    /// 运行总时长
    pub duration: Duration,
    /// 预热时长（不计入指标）
    pub warmup: Duration,
    /// 内部 tick 间隔（毫秒）
    pub tick_interval_ms: u64,

    // ── 确定性 ──
    /// 随机种子
    pub seed: u64,

    // ── 插件 ──
    /// 启用的插件列表
    pub plugins: Vec<String>,

    // ── 模式特有配置 ──
    /// Simulation 模式下，是否使用影子世界（仿真模式）
    pub shadow_world: bool,
    /// Real 模式下，监听地址
    pub listen_addr: Option<String>,
    /// Real 模式下，数据库连接字符串
    pub database_url: Option<String>,

    // ── 数据采集 ──
    /// 指标采样间隔（毫秒）
    pub metrics_interval_ms: u64,
    /// 是否启用 profiling
    pub profile_enabled: bool,
    /// 是否持久化事件到数据库
    pub persist_events: bool,

    // ── Mock Phira ──
    /// 是否启动 Mock Phira 服务器
    pub mock_phira: bool,
    /// Mock Phira 监听端口
    pub mock_phira_port: u16,
}

impl BenchmarkConfig {
    /// 创建默认配置，由预设参数覆盖
    pub fn from_preset(preset: BenchmarkPreset) -> Self {
        let params = crate::benchmark::presets::BenchmarkPresetParams::from_preset(preset);
        Self {
            mode: BenchmarkRunMode::Simulation,
            scenario: BenchmarkScenario::SteadyState,
            preset,
            clients: params.clients,
            rooms: params.rooms,
            members_per_room: params.members_per_room,
            duration: params.duration,
            warmup: params.warmup,
            tick_interval_ms: params.tick_interval_ms,
            seed: 114_514,
            plugins: Vec::new(),
            shadow_world: true,
            listen_addr: None,
            database_url: None,
            metrics_interval_ms: 1_000,
            profile_enabled: false,
            persist_events: false,
            mock_phira: false,
            mock_phira_port: 9877,
        }
    }

    /// 验证配置是否合法
    pub fn validate(&self) -> Result<(), String> {
        if self.clients == 0 {
            return Err("clients must be greater than 0".to_string());
        }
        if self.rooms == 0 {
            return Err("rooms must be greater than 0".to_string());
        }
        if self.duration.is_zero() {
            return Err("duration must be greater than 0".to_string());
        }
        if self.members_per_room == 0 || self.members_per_room > 32 {
            return Err("members_per_room must be between 1 and 32".to_string());
        }
        if self.tick_interval_ms < 50 || self.tick_interval_ms > 60_000 {
            return Err("tick_interval_ms must be between 50 and 60000".to_string());
        }
        Ok(())
    }

    /// 返回运行总秒数
    pub fn total_secs(&self) -> u64 {
        self.duration.as_secs()
    }

    /// 返回是否应采集 CPU profile
    pub fn should_profile(&self) -> bool {
        self.profile_enabled && self.duration >= Duration::from_secs(30)
    }
}

/// 模式特有配置
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SimulationModeConfig {
    /// 是否使用影子世界
    pub shadow_world: bool,
    /// Simulation 内部 tick 是否自动推进
    pub auto_tick: bool,
    /// 每 N tick 持久化事件快照（0 = 禁用）
    pub persist_every_ticks: u64,
}

impl Default for SimulationModeConfig {
    fn default() -> Self {
        Self {
            shadow_world: true,
            auto_tick: true,
            persist_every_ticks: 0,
        }
    }
}

/// Real 模式特有配置
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RealModeConfig {
    /// 服务端监听地址
    pub listen_addr: String,
    /// 数据库连接字符串
    pub database_url: String,
    /// 每个客户端的连接超时
    pub connect_timeout: Duration,
    /// 客户端重连间隔
    pub reconnect_delay: Duration,
    /// 是否启用 Mock Phira 代替真实 Phira API
    pub mock_phira: bool,
    /// Mock Phira 服务端口
    pub mock_phira_port: u16,
}

impl Default for RealModeConfig {
    fn default() -> Self {
        Self {
            listen_addr: "0.0.0.0:9799".to_string(),
            database_url: String::new(),
            connect_timeout: Duration::from_secs(10),
            reconnect_delay: Duration::from_millis(500),
            mock_phira: false,
            mock_phira_port: 9877,
        }
    }
}
