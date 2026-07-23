//! Benchmark module — redesigned benchmark framework for Phira-mp+
//!
//! 重新设计的基准测试模块，支持双模式（Simulation / Real）和 11 种场景，
//! 提供统一指标收集、报告生成和性能分析能力。
//!
//! ## 架构概览
//!
//! - `command`    — CLI 入口参数类型
//! - `config`     — 基准测试配置，由 CLI 参数派生
//! - `runner`     — 顶层运行调度器，分发到模式特定 runner
//! - `environment`— 运行环境检测（CPU、内存、OS 等）
//! - `mock_phira` — 本地 Mock Phira HTTP 服务器
//! - `profile`    — CPU / 堆内存性能分析（profiling）
//! - `metrics`    — 运行期间实时指标采集
//! - `report`     — 基准测试报告生成与格式化
//! - `presets`    — Quick / Standard / Stress / Soak 预设参数
//! - `modes`      — Simulation / Real 两种运行模式
//! - `scenarios`  — 11 种负载场景定义

pub mod command;
pub mod config;
pub mod environment;
pub mod metrics;
pub mod mock_phira;
pub mod presets;
pub mod profile;
pub mod report;
pub mod runner;

pub mod modes {
    pub mod real;
    pub mod simulation;
}

pub mod scenarios {
    pub mod common;
    pub mod connection;
    pub mod database_write;
    pub mod gameplay;
    pub mod hot_room;
    pub mod long_run;
    pub mod mixed;
    pub mod plugin_load;
    pub mod reconnect;
    pub mod room_lifecycle;
    pub mod slow_consumer;
    pub mod steady_state;
}

// ── Re-exports ──

pub use command::{BenchmarkCommand, BenchmarkRunArgs, BenchmarkScenario};
pub use command::BenchmarkPreset;
pub use config::BenchmarkConfig;
pub use metrics::BenchmarkMetrics;
pub use presets::BenchmarkPresetParams;
pub use report::BenchmarkReport;
pub use runner::BenchmarkRunner;
