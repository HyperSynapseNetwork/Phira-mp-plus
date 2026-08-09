//! Benchmark 模式与运行参数。

use serde::{Deserialize, Serialize};
use std::time::Duration;

/// 运行模式。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BenchmarkMode {
    /// 模式一：维持固定负载上限（会话数 + 同时游玩房间数），持续到时长或取消。
    Fixed,
    /// 模式二：自动加压直到 CPU / RAM 触顶后维持，持续到时长或取消。
    Ramp,
}

impl BenchmarkMode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Fixed => "fixed",
            Self::Ramp => "ramp",
        }
    }
}

/// 基准测试运行参数。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModeParams {
    pub mode: BenchmarkMode,
    /// 模式一：最大同时在线游玩房间数。会话数由房间推导（每房 2 个独立成员）。
    pub max_playing_rooms: u32,
    /// 模式二：CPU 上限（百分比 0-100）。
    pub max_cpu_pct: f64,
    /// 模式二：RAM 上限（字节）。
    pub max_ram_bytes: u64,
    /// 持续时间。None = 永久（直到 x 键取消）。
    pub duration: Option<Duration>,
}

impl ModeParams {
    pub fn validate(&self) -> Result<(), String> {
        match self.mode {
            BenchmarkMode::Fixed => {
                if self.max_playing_rooms == 0 {
                    return Err("fixed 模式需要 --playing-rooms 大于 0".to_string());
                }
            }
            BenchmarkMode::Ramp => {
                if self.max_cpu_pct <= 0.0 && self.max_ram_bytes == 0 {
                    return Err("ramp 模式至少需要 --cpu 或 --ram 大于 0".to_string());
                }
                if self.max_cpu_pct > 100.0 {
                    return Err("ramp --cpu 不能超过 100".to_string());
                }
            }
        }
        Ok(())
    }
}
