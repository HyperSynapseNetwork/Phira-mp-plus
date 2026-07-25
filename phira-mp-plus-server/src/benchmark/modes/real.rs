//! Real mode benchmark runner
//!
//! 真实模式运行器。连接到一个正在运行的 PMP 服务器，使用真实二进制
//! 协议 (phira_mp_common::Stream) 进行完整的认证与房间命令交互。
//! 收集命令延迟和吞吐量指标。
//!
//! 运行流程：
//! 1. 可选：启动 Mock Phira 服务器（用于本地认证）
//! 2. TCP 连接到 PMP 服务器
//! 3. 建立 Stream 并完成认证
//! 4. 发送顺序房间命令：CreateRoom, SelectChart, Played
//! 5. 采集延迟指标
//! 6. 清理连接

use crate::benchmark::config::BenchmarkConfig;
use crate::benchmark::environment::EnvironmentSnapshot;
use crate::benchmark::metrics::LatencySampler;
use crate::benchmark::mock_phira::{MockPhiraConfig, MockPhiraServer};
use crate::benchmark::report::BenchmarkReport;
use phira_mp_common::{ClientCommand, RoomId, ServerCommand, Stream, Varchar};
use std::sync::Arc;
use tokio::net::TcpStream;
use tokio::sync::mpsc;
use tokio::time::Instant;
use tracing::info;

/// 真实模式运行结果
pub struct RealRunResult {
    /// 基准测试报告
    pub report: BenchmarkReport,
    /// 服务器进程 ID（本运行器不启动 PMP，始终为 None）
    pub server_pid: Option<u32>,
}

/// 运行真实模式基准测试
///
/// 连接到一个已运行的 PMP 服务器并执行基准测试：
/// 1. 解析配置，若 `config.mock_phira` 为 true 则启动 Mock Phira 服务器
/// 2. TCP 连接到 `config.listen_addr`（默认 127.0.0.1:12346）
/// 3. 通过二进制协议建立 Stream 并认证
/// 4. 依次发送 CreateRoom → SelectChart → Played 命令
/// 5. 测量每条命令的往返延迟
/// 6. 生成并返回基准测试报告
pub async fn run_real(_config: BenchmarkConfig) -> Result<RealRunResult, String> {
    Err("Real mode has been removed — use --mode simulation".to_string())
}

/// （如 Pong、Message 等），直到找到匹配项或流关闭。
async fn wait_for_response<T>(
    rx: &mut mpsc::UnboundedReceiver<ServerCommand>,
    predicate: impl Fn(&ServerCommand) -> Option<T>,
) -> Result<T, String> {
    loop {
        match rx.recv().await {
            Some(cmd) => {
                if let Some(result) = predicate(&cmd) {
                    return Ok(result);
                }
                // 忽略不匹配的命令
            }
            None => return Err("stream closed while waiting for response".to_string()),
        }
    }
}
