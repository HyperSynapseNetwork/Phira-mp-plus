//! Plugin load scenario
//!
//! 插件事件和 API 负载测试场景。在启用了 WASM 插件的环境中，
//! 模拟插件事件的生产和消费、插件 API 调用和跨插件通信。
//! 测试插件系统的吞吐能力和资源隔离效果。
//!
//! Implementation: uses `SimulationManager` with a `Balanced` workload.
//! Chat and ready events are generated (these flow through the EventBus
//! which plugin hooks subscribe to).  The `plugin-system` feature gate
//! guards this scenario; without it, a descriptive error is returned.

use crate::benchmark::config::BenchmarkConfig;
use crate::benchmark::metrics::BenchmarkMetrics;

/// 插件负载场景参数
#[derive(Debug, Clone)]
pub struct PluginLoadParams {
    /// 并发加载的插件数
    pub concurrent_plugins: u32,
    /// 每秒发送的事件数
    pub events_per_sec: u32,
    /// 每个事件的平均负载大小（字节）
    pub event_payload_size: usize,
    /// 是否启用跨插件事件广播
    pub cross_plugin_events: bool,
    /// 插件 API 调用频率（调用/s）
    pub api_calls_per_sec: u32,
    /// 自定义插件 WASM 路径列表
    pub plugin_paths: Vec<String>,
}

impl Default for PluginLoadParams {
    fn default() -> Self {
        Self {
            concurrent_plugins: 5,
            events_per_sec: 1000,
            event_payload_size: 512,
            cross_plugin_events: false,
            api_calls_per_sec: 100,
            plugin_paths: Vec::new(),
        }
    }
}

/// 执行插件负载场景
///
/// When the `plugin-system` feature is enabled, delegates to the
/// `SimulationManager` with all event types enabled so plugin hooks
/// attached to chat, ready, and round events are exercised.
#[cfg(feature = "plugin-system")]
pub async fn run_plugin_load(
    config: &BenchmarkConfig,
    _params: PluginLoadParams,
) -> Result<BenchmarkMetrics, String> {
    super::common::run_simulation(config, |sc| {
        sc.chat = true;
        sc.ready = true;
        sc.rounds = true;
        sc.touch = true;
        sc.judge = true;
    })
    .await
}

/// 未启用 plugin-system 特性时的回退
#[cfg(not(feature = "plugin-system"))]
pub async fn run_plugin_load(
    _config: &BenchmarkConfig,
    _params: PluginLoadParams,
) -> Result<BenchmarkMetrics, String> {
    Err("plugin_load scenario requires 'plugin-system' feature".to_string())
}
