//! Benchmark 模块 —— 进程内纯内部调用版。
//!
//! 直接调用线上 `PlusServerState` 内部 API 生成负载（不跑 phira 线协议、
//! 不建子进程、不依赖独立数据库）。支持两种模式：
//! - Fixed：维持最大会话数 + 最大同时在线游玩房间数，持续到时长或取消；
//! - Ramp：自动加压直到 CPU / RAM 触顶后维持。
//!
//! - `mode`      — 模式与参数
//! - `harness`   — 进程内负载生成器
//! - `sampler`   — 进程内 CPU / RAM 采样
//! - `environment`— 运行环境快照（报告用）
//! - `report`    — 报告生成与格式化

pub mod environment;
pub mod harness;
pub mod mode;
pub mod report;
pub mod sampler;

pub use harness::BenchmarkHarness;
pub use mode::{BenchmarkMode, ModeParams};
pub use report::{BenchmarkReport, RampReached};
