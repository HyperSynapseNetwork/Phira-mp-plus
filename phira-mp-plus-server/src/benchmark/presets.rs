//! Benchmark presets
//!
//! Quick / Standard / Stress / Soak 四种预设参数集。
//! 每种预设定义了默认的客户端数、房间数、运行时长和 tick 间隔。

use crate::benchmark::command::BenchmarkPreset;
use serde::{Deserialize, Serialize};
use std::time::Duration;

/// 预设参数
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct BenchmarkPresetParams {
    /// 预设名称
    pub preset: BenchmarkPreset,
    /// 客户端数量
    pub clients: u32,
    /// 房间数量
    pub rooms: u32,
    /// 每房间最大成员数
    pub members_per_room: u32,
    /// 运行时长
    pub duration: Duration,
    /// 预热时长
    pub warmup: Duration,
    /// 内部 tick 间隔（毫秒）
    pub tick_interval_ms: u64,
    /// 是否默认启用 profiling
    pub profile: bool,
}

impl BenchmarkPresetParams {
    /// 根据预设生成参数集
    pub fn from_preset(preset: BenchmarkPreset) -> Self {
        match preset {
            BenchmarkPreset::Quick => Self::quick(),
            BenchmarkPreset::Standard => Self::standard(),
            BenchmarkPreset::Stress => Self::stress(),
            BenchmarkPreset::Soak => Self::soak(),
            BenchmarkPreset::Custom => Self::default(),
        }
    }

    /// Quick 预设：短时验证，适合 CI
    ///
    /// - 20 客户端，5 房间
    /// - 60 秒运行，5 秒预热
    /// - 500ms tick 间隔
    /// - 不启用 profiling
    fn quick() -> Self {
        Self {
            preset: BenchmarkPreset::Quick,
            clients: 20,
            rooms: 5,
            members_per_room: 8,
            duration: Duration::from_secs(60),
            warmup: Duration::from_secs(5),
            tick_interval_ms: 500,
            profile: false,
        }
    }

    /// Standard 预设：中等负载，适合开发环境
    ///
    /// - 200 客户端，20 房间
    /// - 300 秒运行，30 秒预热
    /// - 200ms tick 间隔
    /// - 不启用 profiling
    fn standard() -> Self {
        Self {
            preset: BenchmarkPreset::Standard,
            clients: 200,
            rooms: 20,
            members_per_room: 8,
            duration: Duration::from_secs(300),
            warmup: Duration::from_secs(30),
            tick_interval_ms: 200,
            profile: false,
        }
    }

    /// Stress 预设：高负载，用于容量评估
    ///
    /// - 1000 客户端，100 房间
    /// - 600 秒运行，60 秒预热
    /// - 100ms tick 间隔
    /// - 启用 profiling
    fn stress() -> Self {
        Self {
            preset: BenchmarkPreset::Stress,
            clients: 1000,
            rooms: 100,
            members_per_room: 16,
            duration: Duration::from_secs(600),
            warmup: Duration::from_secs(60),
            tick_interval_ms: 100,
            profile: true,
        }
    }

    /// Soak 预设：长时间运行，用于稳定性测试
    ///
    /// - 500 客户端，50 房间
    /// - 6 小时运行（可扩展至 24/72 小时），120 秒预热
    /// - 500ms tick 间隔
    /// - 启用 profiling（仅在开始和结束阶段）
    fn soak() -> Self {
        Self {
            preset: BenchmarkPreset::Soak,
            clients: 500,
            rooms: 50,
            members_per_room: 8,
            duration: Duration::from_secs(6 * 3600), // 6 小时，可被扩展
            warmup: Duration::from_secs(120),
            tick_interval_ms: 500,
            profile: true,
        }
    }

    /// 返回预设的中文描述
    pub fn description(&self) -> &'static str {
        match self.preset {
            BenchmarkPreset::Quick => "快速验证：20客户端/5房间/60秒，适合 CI 和烟雾测试",
            BenchmarkPreset::Standard => "标准负载：200客户端/20房间/300秒，适合开发环境评估",
            BenchmarkPreset::Stress => "压力测试：1000客户端/100房间/600秒，用于容量评估",
            BenchmarkPreset::Soak => "浸泡测试：500客户端/50房间/6小时，用于稳定性评估",
            BenchmarkPreset::Custom => "自定义参数：由用户指定所有运行时参数",
        }
    }
}

impl Default for BenchmarkPresetParams {
    fn default() -> Self {
        Self::quick()
    }
}
