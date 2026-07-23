//! Benchmark CLI command types
//!
//! 定义基准测试 CLI 入口的参数类型，包括运行模式、场景、预设等枚举和参数结构体。

use serde::{Deserialize, Serialize};
use std::time::Duration;

/// 基准测试顶层子命令
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BenchmarkCommand {
    /// 运行一次基准测试
    Run(BenchmarkRunArgs),
    /// 查看历史报告
    Report,
    /// 查看可用场景列表
    Scenarios,
    /// 查看可用预设列表
    Presets,
    /// 查看运行模式说明
    Modes,
}

/// 11 种基准测试场景
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BenchmarkScenario {
    /// 房间全生命周期：创建、加入、选曲、准备、开始、结束、离开
    RoomLifecycle,
    /// 实时 Touch/Judge 游戏操作
    Gameplay,
    /// TCP 连接、认证和重连
    Connection,
    /// 稳定房间 + 定期 ping 和低频控制指令
    SteadyState,
    /// 单个热点房间，广播和状态更新密集
    HotRoom,
    /// 慢客户端隔离测试
    SlowConsumer,
    /// 重连风暴 + 旧会话清理
    Reconnect,
    /// 插件事件和 API 负载测试
    PluginLoad,
    /// PostgreSQL 和批量写入压力测试
    DatabaseWrite,
    /// 多种并发负载类型（非串行）
    Mixed,
    /// 6 / 24 / 72 小时稳定性测试
    LongRun,
}

impl BenchmarkScenario {
    /// 返回场景的简短英文名称
    pub fn as_str(self) -> &'static str {
        match self {
            Self::RoomLifecycle => "room_lifecycle",
            Self::Gameplay => "gameplay",
            Self::Connection => "connection",
            Self::SteadyState => "steady_state",
            Self::HotRoom => "hot_room",
            Self::SlowConsumer => "slow_consumer",
            Self::Reconnect => "reconnect",
            Self::PluginLoad => "plugin_load",
            Self::DatabaseWrite => "database_write",
            Self::Mixed => "mixed",
            Self::LongRun => "long_run",
        }
    }

    /// 返回场景的中文描述
    pub fn description(self) -> &'static str {
        match self {
            Self::RoomLifecycle => "房间全生命周期压力：创建/加入/选曲/准备/开始/结束/离开",
            Self::Gameplay => "实时 Touch/Judge 游戏操作压力",
            Self::Connection => "TCP 连接、认证和重连压力",
            Self::SteadyState => "稳定房间 + 定期 ping 和低频控制指令",
            Self::HotRoom => "单个热点房间广播和状态更新密集压力",
            Self::SlowConsumer => "慢客户端隔离测试",
            Self::Reconnect => "重连风暴 + 旧会话清理",
            Self::PluginLoad => "插件事件和 API 负载测试",
            Self::DatabaseWrite => "PostgreSQL 和批量写入压力测试",
            Self::Mixed => "多种并发负载类型混合",
            Self::LongRun => "6/24/72 小时稳定性测试",
        }
    }

    /// 返回所有场景的列表
    pub fn all() -> &'static [Self] {
        &[
            Self::RoomLifecycle,
            Self::Gameplay,
            Self::Connection,
            Self::SteadyState,
            Self::HotRoom,
            Self::SlowConsumer,
            Self::Reconnect,
            Self::PluginLoad,
            Self::DatabaseWrite,
            Self::Mixed,
            Self::LongRun,
        ]
    }

    /// 从字符串解析场景
    pub fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().replace('-', "_").as_str() {
            "room_lifecycle" | "room" | "lifecycle" => Some(Self::RoomLifecycle),
            "gameplay" | "game" | "play" => Some(Self::Gameplay),
            "connection" | "conn" | "connect" => Some(Self::Connection),
            "steady_state" | "steady" | "steady_state" => Some(Self::SteadyState),
            "hot_room" | "hot" => Some(Self::HotRoom),
            "slow_consumer" | "slow" => Some(Self::SlowConsumer),
            "reconnect" | "reconn" => Some(Self::Reconnect),
            "plugin_load" | "plugin" => Some(Self::PluginLoad),
            "database_write" | "db_write" | "db" => Some(Self::DatabaseWrite),
            "mixed" => Some(Self::Mixed),
            "long_run" | "long" | "stability" | "soak" => Some(Self::LongRun),
            _ => None,
        }
    }
}

/// 基准测试预设
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BenchmarkPreset {
    /// 快速（Quick）：短时验证，适合 CI
    Quick,
    /// 标准（Standard）：中等负载，适合开发环境
    Standard,
    /// 压力（Stress）：高负载，用于容量评估
    Stress,
    /// 浸泡（Soak）：长时间运行，用于稳定性测试
    Soak,
    /// 自定义参数
    Custom,
}

impl BenchmarkPreset {
    /// 返回预设的简短英文名称
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Quick => "quick",
            Self::Standard => "standard",
            Self::Stress => "stress",
            Self::Soak => "soak",
            Self::Custom => "custom",
        }
    }

    /// 从字符串解析预设
    pub fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "quick" | "q" => Some(Self::Quick),
            "standard" | "std" | "s" => Some(Self::Standard),
            "stress" | "heavy" | "load" => Some(Self::Stress),
            "soak" | "long" | "stability" => Some(Self::Soak),
            "custom" | "c" => Some(Self::Custom),
            _ => None,
        }
    }
}

/// 基准测试运行参数
///
/// 由 CLI 解析后传给 `BenchmarkRunner`。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BenchmarkRunArgs {
    /// 运行模式：Simulation（默认）或 Real
    pub mode: BenchmarkRunMode,
    /// 负载场景
    pub scenario: BenchmarkScenario,
    /// 预设参数集
    pub preset: BenchmarkPreset,
    /// 模拟客户端数量
    pub clients: u32,
    /// 模拟房间数量
    pub rooms: u32,
    /// 运行时长
    pub duration: Duration,
    /// 随机种子（用于确定性回放）
    pub seed: u64,
    /// 启用的插件列表（WASM 路径或名称）
    pub plugins: Vec<String>,
    /// 可选的自定义参数覆盖（key=value 格式）
    pub overrides: Vec<(String, String)>,
}

impl Default for BenchmarkRunArgs {
    fn default() -> Self {
        Self {
            mode: BenchmarkRunMode::Simulation,
            scenario: BenchmarkScenario::SteadyState,
            preset: BenchmarkPreset::Quick,
            clients: 20,
            rooms: 5,
            duration: Duration::from_secs(60),
            seed: 114_514,
            plugins: Vec::new(),
            overrides: Vec::new(),
        }
    }
}

/// 基准测试运行模式
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BenchmarkRunMode {
    /// 仿真模式（使用影子世界，不涉及真实网络）
    Simulation,
    /// 真实模式（启动真实 PMP 服务，连接真实客户端）
    Real,
}

impl BenchmarkRunMode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Simulation => "simulation",
            Self::Real => "real",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "simulation" | "sim" => Some(Self::Simulation),
            "real" => Some(Self::Real),
            _ => None,
        }
    }
}
