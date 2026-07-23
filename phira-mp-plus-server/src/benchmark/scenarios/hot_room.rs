//! Hot room scenario
//!
//! 单个热点房间场景。将大量客户端集中到一个房间中，
//! 密集发送广播消息和状态更新。测试服务端在热点房间场景下的
//! 广播效率、状态同步性能和抗拥塞能力。
//!
//! Implementation: uses `SimulationManager` with `ChatStorm` workload.
//! The number of rooms is capped to a small handful so that most clients
//! are concentrated in a few rooms, simulating a broadcast-heavy hot room.
//! Chat messages (the primary broadcast vector) are enabled; all other
//! event types are disabled.

use crate::benchmark::config::BenchmarkConfig;
use crate::benchmark::metrics::BenchmarkMetrics;
use crate::simulation::SimulationScenario;

/// 热点房间场景参数
#[derive(Debug, Clone)]
pub struct HotRoomParams {
    /// 热点房间中的客户端数
    pub clients_in_hot_room: u32,
    /// 广播消息发送间隔（毫秒）
    pub broadcast_interval_ms: u64,
    /// 每条广播的消息大小（字节）
    pub message_size: usize,
    /// 看热闹的只读客户端数（只接收不发送）
    pub spectator_clients: u32,
    /// 热点房间外的普通房间数
    pub background_rooms: u32,
}

impl Default for HotRoomParams {
    fn default() -> Self {
        Self {
            clients_in_hot_room: 100,
            broadcast_interval_ms: 50,
            message_size: 256,
            spectator_clients: 50,
            background_rooms: 5,
        }
    }
}

/// 执行热点房间场景
///
/// Concentrates clients into a small number of rooms (`sc.rooms` is
/// clamped to 5) and uses the `ChatStorm` workload to generate high
/// broadcast traffic.  Ready, rounds, touch, and judge are disabled so
/// the measured load is dominated by broadcast messages.
pub async fn run_hot_room(
    config: &BenchmarkConfig,
    _params: HotRoomParams,
) -> Result<BenchmarkMetrics, String> {
    super::common::run_simulation(config, |sc| {
        // Keep rooms small so clients concentrate — hot room effect.
        sc.rooms = sc.rooms.clamp(1, 5);
        sc.chat = true;
        sc.ready = false;
        sc.rounds = false;
        sc.touch = false;
        sc.judge = false;
        sc.scenario = SimulationScenario::Balanced;
    })
    .await
}
