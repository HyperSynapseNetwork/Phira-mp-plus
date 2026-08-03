//! Ops / WAL / dead-letter command specifications.

use crate::command_registry::CommandSpec;
use std::sync::Arc;

use super::with_args;

pub fn specs() -> Vec<CommandSpec> {
    vec![
        CommandSpec::new("wal inspect", "ops", "查看 WAL 状态统计。", "wal inspect")
            .handler(Arc::new(|state, _args| {
                let state = Arc::clone(state);
                Box::pin(async move {
                    let mut lines = vec![];
                    let wal_path = &state.config.runtime.persistence_wal_path;
                    lines.push(format!("  ◆ WAL 路径: {wal_path}"));
                    if let Ok(meta) = std::fs::metadata(wal_path) {
                        lines.push(format!("  ◆ 文件大小: {} 字节", meta.len()));
                    }
                    lines
                })
            })),
        CommandSpec::new("dead-letter list", "ops", "列出 dead-letter 记录摘要。", "dead-letter list [limit]")
            .handler(Arc::new(|state, args| {
                let state = Arc::clone(state);
                let args: Vec<String> = args.iter().map(|s| s.to_string()).collect();
                Box::pin(async move {
                    let limit = args.first().and_then(|v| v.parse::<usize>().ok()).unwrap_or(10);
                    let mut lines = vec![format!("  ◆ dead-letter 最近 {limit} 条")];
                    if let Some(path) = &state.config.runtime.persistence_dead_letter_path {
                        if let Ok(content) = std::fs::read_to_string(path) {
                            let count = content.lines().filter(|l| !l.trim().is_empty()).count();
                            lines.push(format!("  ◆ 总记录数: {count}"));
                            let all_lines: Vec<&str> = content.lines().collect();
                            for line in all_lines.iter().rev().take(limit) {
                                if let Ok(val) = serde_json::from_str::<serde_json::Value>(line) {
                                    let summary = val.get("summary").and_then(|v| v.as_str()).unwrap_or("?");
                                    let kind = val.get("kind").and_then(|v| v.as_str()).unwrap_or("?");
                                    lines.push(format!("  · [{kind}] {summary}"));
                                }
                            }
                        } else {
                            lines.push("  ○ dead-letter 文件不存在或无法读取".to_string());
                        }
                    } else {
                        lines.push("  ○ dead-letter 未配置".to_string());
                    }
                    lines
                })
            })),
        CommandSpec::new("dead-letter replay", "ops", "重放 dead-letter 事件到持久化队列。", "dead-letter replay")
            .handler(Arc::new(|state, _args| {
                let state = Arc::clone(state);
                Box::pin(async move {
                    let mut lines = vec!["  ◆ dead-letter replay...".to_string()];
                    let path = &state.config.runtime.persistence_dead_letter_path.clone();
                    let Some(path) = path else {
                        lines.push("  ○ dead-letter 未配置".to_string());
                        return lines;
                    };
                    let content = match std::fs::read_to_string(path) {
                        Ok(c) => c,
                        Err(e) => {
                            lines.push(format!("  ✗ 读取 dead-letter 失败: {e}"));
                            return lines;
                        }
                    };
                    let mut count = 0usize;
                    for line in content.lines() {
                        let line = line.trim();
                        if line.is_empty() { continue; }
                        if let Ok(val) = serde_json::from_str::<serde_json::Value>(line) {
                            let event: Option<crate::persistence::message::PersistenceEvent> =
                                val.get("event").and_then(|v| serde_json::from_value(v.clone()).ok());
                            if let Some(ev) = event {
                                let pw = Arc::clone(&state.persistence_worker);
                                tokio::task::spawn(async move {
                                    if let Err(e) = pw.enqueue(ev).await {
                                        tracing::warn!(kind = %e.kind(), "dead-letter replay enqueue failed");
                                    }
                                });
                                count += 1;
                            }
                        }
                    }
                    lines.push(format!("  ✓ 已提交 {count} 个事件到持久化队列"));
                    lines
                })
            })),
        CommandSpec::new(
            "approve openuds",
            "ops",
            "批准挂起的 OpenUDS 连接（仅 Unix）。",
            "approve openuds <pending_id>",
        )
        .handler(with_args(|h, args| {
            Box::pin(async move {
                let mut full: Vec<String> = vec!["openuds".to_string()];
                full.extend(args.iter().cloned());
                let arg_refs: Vec<&str> = full.iter().map(|s| s.as_str()).collect();
                h.dispatch_approve_command(&arg_refs).await
            })
        })),
    ]
}
