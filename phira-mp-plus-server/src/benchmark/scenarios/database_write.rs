//! Database write scenario
//!
//! PostgreSQL 和批量写入压力测试场景。模拟大量并发数据库写入操作，
//! 包括事件持久化、状态快照、游戏记录写入等。测试 PersistenceWorker
//! 和数据库连接池在高写入负载下的表现。
//!
//! Implementation: uses `SimulationManager` with `persist_every_ticks` set
//! to a small value so the shadow world emits periodic snapshot events
//! that the PersistenceWorker would write to the database.  All event
//! types are enabled to maximize write pressure.

use crate::benchmark::config::BenchmarkConfig;
use crate::benchmark::metrics::BenchmarkMetrics;

/// 数据库写入场景参数
#[derive(Debug, Clone)]
pub struct DatabaseWriteParams {
    /// 并发写入协程数
    pub concurrent_writers: u32,
    /// 每秒目标写入行数
    pub target_rows_per_sec: u32,
    /// 批量大小（每批写入行数）
    pub batch_size: u32,
    /// 写入数据平均大小（字节）
    pub row_size_bytes: usize,
    /// 是否使用事务批量写入
    pub use_transactions: bool,
    /// 表名列表（写入目标）
    pub target_tables: Vec<String>,
}

impl Default for DatabaseWriteParams {
    fn default() -> Self {
        Self {
            concurrent_writers: 10,
            target_rows_per_sec: 10_000,
            batch_size: 100,
            row_size_bytes: 256,
            use_transactions: true,
            target_tables: vec![
                "mp_sim_events".to_string(),
                "mp_benchmark_metrics".to_string(),
            ],
        }
    }
}

/// 执行数据库写入场景
///
/// When the `postgres` feature is enabled, runs a `SimulationManager`
/// with `persist_every_ticks=5` so that periodic snapshot events are
/// emitted every 5 ticks, generating database write pressure.
/// All event types are enabled for maximum volume.
#[cfg(feature = "postgres")]
pub async fn run_database_write(
    config: &BenchmarkConfig,
    _params: DatabaseWriteParams,
) -> Result<BenchmarkMetrics, String> {
    super::common::run_simulation(config, |sc| {
        sc.persist_every_ticks = 5; // frequent snapshot events
        sc.chat = true;
        sc.ready = true;
        sc.rounds = true;
        sc.touch = true;
        sc.judge = true;
    })
    .await
}

/// 未启用 postgres 特性时的回退
#[cfg(not(feature = "postgres"))]
pub async fn run_database_write(
    _config: &BenchmarkConfig,
    _params: DatabaseWriteParams,
) -> Result<BenchmarkMetrics, String> {
    Err("database_write scenario requires 'postgres' feature".to_string())
}
