//! Phira-mp+ 压测插件
//!
//! CLI `benchmark` — 长时间压力测试，测试房间创建/用户加入/稳定性。

use phira_mp_plus_server_api::{
    NativePlugin, PluginContext, PluginEvent, PluginInfo,
};
use std::sync::Arc;
use tracing::info;

pub struct StressTestPlugin;

impl StressTestPlugin {
    pub fn create() -> Box<dyn NativePlugin> {
        Box::new(Self)
    }
}

impl NativePlugin for StressTestPlugin {
    fn info(&self) -> PluginInfo {
        PluginInfo {
            name: "stress-test".to_string(),
            version: "0.1.0".to_string(),
            author: "Phira-mp+".to_string(),
            description: "服务端压力测试与容量评估".to_string(),
        }
    }

    fn init(&mut self, ctx: &PluginContext) -> Result<(), String> {
        info!("StressTest plugin initializing...");

        if let Some(cli) = &ctx.cli {
            let state = ctx.state.clone();

            // ── benchmark — 全流程压测 ──
            let _ = cli.register(
                "benchmark",
                "运行压力测试 (创建房间/填充用户/持续负载)",
                "benchmark [时长s] [房间数]",
                Arc::new(move |args| {
                    let st = match &state {
                        Some(s) => s,
                        None => return vec!["  · 状态查询不可用".into()],
                    };

                    let duration: u64 = args.first().and_then(|a| a.parse().ok()).unwrap_or(30);
                    let rooms: usize = args.get(1).and_then(|a| a.parse().ok()).unwrap_or(200);

                    let mut out = vec![
                        format!("  ◆ 开始压测: {}s, {} 间房间", duration, rooms),
                        format!("  │ 这可能需要几分钟，请耐心等待..."),
                    ];

                    match st.call("test.run_benchmark", &[
                        serde_json::json!(duration),
                        serde_json::json!(rooms),
                    ]) {
                        Ok(result) => {
                            if let Some(text) = result.get("output").and_then(|v| v.as_str()) {
                                // 分行输出
                                for line in text.lines() {
                                    out.push(line.to_string());
                                }
                            } else {
                                out.push(format!("  ✓ 压测完成"));
                            }
                        }
                        Err(e) => {
                            out.push(format!("  ✗ 压测失败: {e}"));
                            // 尝试清理
                            let _ = st.call("test.cleanup", &[]);
                        }
                    }

                    out
                }),
            );

            // ── bench-cleanup — 清理残留 ──
            let state = ctx.state.clone();
            let _ = cli.register(
                "bench-cleanup",
                "清理压测残留数据",
                "bench-cleanup",
                Arc::new(move |_| {
                    let st = match &state {
                        Some(s) => s,
                        None => return vec!["  · 状态查询不可用".into()],
                    };
                    match st.call("test.cleanup", &[]) {
                        Ok(_) => vec!["  ✓ 已清理压测残留".into()],
                        Err(e) => vec![format!("  ✗ 清理失败: {e}")],
                    }
                }),
            );

            // ── bench-quick — 快速基准测试 ──
            let state = ctx.state.clone();
            let _ = cli.register(
                "bench-quick",
                "快速基准测试 (10s, 50 间房)",
                "bench-quick",
                Arc::new(move |_| {
                    let st = match &state {
                        Some(s) => s,
                        None => return vec!["  · 状态查询不可用".into()],
                    };
                    match st.call("test.run_benchmark", &[
                        serde_json::json!(10),
                        serde_json::json!(50),
                    ]) {
                        Ok(result) => {
                            let mut out = vec![];
                            if let Some(text) = result.get("output").and_then(|v| v.as_str()) {
                                for line in text.lines() {
                                    out.push(line.to_string());
                                }
                            }
                            out
                        }
                        Err(e) => vec![format!("  ✗ 压测失败: {e}")],
                    }
                }),
            );
        }

        info!("StressTest plugin initialized");
        Ok(())
    }

    fn on_event(&self, _ctx: &PluginContext, _event: &PluginEvent) -> Vec<String> {
        vec![]
    }

    fn cleanup(&mut self) {
        info!("StressTest plugin cleaned up");
    }
}
