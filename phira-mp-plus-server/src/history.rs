//! 会话历史：日志输出 + 管理输入的进程内环形缓冲。
//!
//! 供 OpenUDS 面板查询"本进程运行以来的历史输出与输入"（随进程重启清空）。
//! - 日志行由 logging 层写入（`push_log`），面板经 `logs.history` 查询；
//! - 管理输入（CLI / OpenUDS / 游戏内管理员）由对应入口记录（`record_input`），
//!   面板经 `logs.input` 查询。

use serde::Serialize;
use std::collections::VecDeque;
use std::sync::{Mutex, OnceLock};

/// 日志环形缓冲上限。
const LOG_RING_CAP: usize = 2000;
/// 输入环形缓冲上限。
const INPUT_RING_CAP: usize = 1000;

/// 一条管理输入记录。
#[derive(Debug, Clone, Serialize)]
pub struct InputEntry {
    /// Unix 毫秒时间戳
    pub time_ms: i64,
    /// 来源：cli / openuds / admin
    pub source: String,
    /// 命令文本
    pub command: String,
}

static LOG_RING: OnceLock<Mutex<VecDeque<String>>> = OnceLock::new();
static INPUT_RING: OnceLock<Mutex<VecDeque<InputEntry>>> = OnceLock::new();

fn log_ring() -> &'static Mutex<VecDeque<String>> {
    LOG_RING.get_or_init(|| Mutex::new(VecDeque::new()))
}
fn input_ring() -> &'static Mutex<VecDeque<InputEntry>> {
    INPUT_RING.get_or_init(|| Mutex::new(VecDeque::new()))
}

/// 记录一条日志行（logging 层调用）。
pub fn push_log(line: String) {
    let mut ring = log_ring().lock().unwrap_or_else(|e| e.into_inner());
    if ring.len() >= LOG_RING_CAP {
        ring.pop_front();
    }
    ring.push_back(line);
}

/// 记录一条管理输入（CLI / OpenUDS / 游戏内管理员）。
pub fn record_input(source: &str, command: &str) {
    let time_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0);
    let mut ring = input_ring().lock().unwrap_or_else(|e| e.into_inner());
    if ring.len() >= INPUT_RING_CAP {
        ring.pop_front();
    }
    ring.push_back(InputEntry {
        time_ms,
        source: source.to_string(),
        command: command.to_string(),
    });
}

/// 最近 N 条日志行（新→旧）。
pub fn recent_logs(limit: usize) -> Vec<String> {
    let ring = log_ring().lock().unwrap_or_else(|e| e.into_inner());
    ring.iter().rev().take(limit).cloned().collect()
}

/// 最近 N 条输入记录（新→旧）。
pub fn recent_inputs(limit: usize) -> Vec<InputEntry> {
    let ring = input_ring().lock().unwrap_or_else(|e| e.into_inner());
    ring.iter().rev().take(limit).cloned().collect()
}
