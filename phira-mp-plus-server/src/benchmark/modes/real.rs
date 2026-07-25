//! Real mode benchmark runner
//!
//! 真实模式运行器。启动一个真实的 PMP 服务器（绑定本地端口），
//! 然后使用 `phira_client` 模块模拟客户端连接和执行场景操作。
//! 所有指标从真实运行时组件采集。
//!
//! 此模式依赖：
//! - PostgreSQL 数据库（用于持久化和事件记录）
//! - Mock Phira 服务器（可选，用于模拟 Phira API）
//! - PMP 服务器的完整启动流程

use crate::benchmark::config::BenchmarkConfig;
use crate::benchmark::report::BenchmarkReport;

/// 真实模式运行结果
pub struct RealRunResult {
    /// 基准测试报告
    pub report: BenchmarkReport,
    /// 服务器进程 ID
    pub server_pid: Option<u32>,
}

/// 运行真实模式基准测试
///
/// TODO: 实现真实模式运行逻辑：
/// 1. 解析配置，准备数据库 schema
/// 2. 可选：启动 Mock Phira 服务器
/// 3. 启动 PMP 服务器（PlusServer::new / PlusServer::run）
/// 4. 使用 phira_client 创建 N 个客户端连接和 M 个房间
/// 5. 按场景驱动客户端行为
/// 6. 周期采集指标
/// 7. 清理：停止客户端、停止服务器、停止 Mock Phira
pub async fn run_real(config: BenchmarkConfig) -> Result<RealRunResult, String> {
    // TODO: 实现真实模式运行逻辑
    let _ = config;
    Err("real mode runner not yet implemented".to_string())
}
