//! Phira-mp+ 压测插件
//!
//! CLI `benchmark` — 分析配置与状态，估算最大房间数/人数/推荐值。

use phira_mp_plus_server_api::{
    NativePlugin, PluginContext, PluginEvent, PluginInfo,
};
use std::sync::Arc;
use std::time::Instant;
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
            description: "服务端容量压测与分析".to_string(),
        }
    }

    fn init(&mut self, ctx: &PluginContext) -> Result<(), String> {
        info!("StressTest plugin initializing...");

        if let Some(cli) = &ctx.cli {
            let state = ctx.state.clone();
            let _ = cli.register(
                "benchmark",
                "运行服务端容量压测",
                "benchmark",
                Arc::new(move |args| {
                    let _ = args;
                    let st = match &state {
                        Some(s) => s,
                        None => return vec!["  · 状态查询不可用".into()],
                    };
                    let start = Instant::now();
                    let mut out = vec![format!("  ◆ 服务端容量压测")];

                    let rooms = st.call("rooms.list", &[])
                        .ok().and_then(|v| v.as_array().map(|a| a.len())).unwrap_or(0);
                    out.push(format!("  │ 活跃房间: {rooms}"));

                    const MAX: usize = 4096;
                    out.push(format!("  │"));
                    out.push(format!("  ├─ 容量上限（{MAX} 会话）"));
                    out.push(format!("  │  ┌ 2人/间 → ≤{} 间", MAX / 2));
                    out.push(format!("  │  ├ 4人/间 → ≤{} 间", MAX / 4));
                    out.push(format!("  │  └ 8人/间 → ≤{} 间", MAX / 8));

                    let mut us = Vec::new();
                    for _ in 0..5 {
                        let t = Instant::now();
                        let _ = st.call("rooms.list", &[]);
                        us.push(t.elapsed().as_micros());
                    }
                    let avg: f64 = us.iter().sum::<u128>() as f64 / us.len() as f64;

                    let recommended = if rooms > 50 { 4 } else if rooms > 20 { 6 } else { 8 };

                    out.push(format!("  │"));
                    out.push(format!("  ├─ 查询延迟 (5次 avg)"));
                    out.push(format!("  │  {avg:.0}µs"));
                    out.push(format!("  │"));
                    out.push(format!("  ├─ 推荐"));
                    out.push(format!("  │  每房间最多 {recommended} 人"));
                    out.push(format!("  │  连接限流 30次/10s (可调)"));
                    out.push(format!("  │  改 server_config.yml 后重启"));
                    out.push(format!("  │"));
                    out.push(format!("  └─ 耗时 {:.1}ms", start.elapsed().as_secs_f64() * 1000.0));
                    out
                }),
            );

            let state = ctx.state.clone();
            let _ = cli.register(
                "benchmark-config",
                "查看/设置每房间最大人数",
                "benchmark-config [<新值>]",
                Arc::new(move |args| {
                    let st = match &state {
                        Some(s) => s,
                        None => return vec!["  · 状态查询不可用".into()],
                    };
                    let now = st.call("rooms.list", &[])
                        .ok().and_then(|v| v.as_array().map(|a| a.len())).unwrap_or(0);
                    let mut out = vec![
                        format!("  ◆ 房间容量配置"),
                        format!("  │ 活跃房间: {now}"),
                    ];
                    if args.is_empty() {
                        out.push(format!("  │ 当前 max_users_per_room 见 server_config.yml"));
                        out.push(format!("  │ 用法: benchmark-config <2~64>"));
                    } else if let Ok(n) = args[0].parse::<usize>() {
                        if n >= 2 && n <= 64 {
                            out.push(format!("  │ ✓ 建议值: 每房间最多 {n} 人"));
                            out.push(format!("  │   请更新 server_config.yml 后重启"));
                        } else {
                            out.push(format!("  │ ✗ 范围 2~64"));
                        }
                    } else {
                        out.push(format!("  │ ✗ 参数须为数字"));
                    }
                    out
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
