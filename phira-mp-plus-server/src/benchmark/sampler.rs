//! 进程内 CPU / 内存采样（Linux /proc）。
//!
//! `CpuSampler` 用两次 `/proc/self/stat` 的 utime/stime 差值计算窗口内 CPU
//! 使用率（相对整机 0-100）。`read_rss_bytes` 读 `/proc/self/statm` 的常驻页数。

use std::time::Instant;

/// Linux USER_HZ（多数内核 100）。
const CLK_TCK: f64 = 100.0;

/// 读取进程累计用户态 / 内核态时钟 tick（utime/stime）。
///
/// `/proc/self/stat` 中 comm 可能含空格/括号，不能按空白切分；先找最后一个
/// `)`，其后的字段从 field 3（state）开始：utime=field14 → rest[11]，stime=field15 → rest[12]。
fn read_proc_stat_ticks() -> Option<(u64, u64)> {
    let content = std::fs::read_to_string("/proc/self/stat").ok()?;
    let close = content.rfind(')')?;
    let rest = content[close + 1..].trim();
    let fields: Vec<&str> = rest.split_whitespace().collect();
    let utime: u64 = fields.get(11)?.parse().ok()?;
    let stime: u64 = fields.get(12)?.parse().ok()?;
    Some((utime, stime))
}

/// 当前进程常驻内存（RSS，字节）。
pub fn read_rss_bytes() -> u64 {
    let Ok(content) = std::fs::read_to_string("/proc/self/statm") else {
        return 0;
    };
    // 格式: size resident shared text lib data dt；resident 以页为单位（4KiB）。
    let mut parts = content.split_whitespace();
    parts.next(); // size
    parts
        .next()
        .and_then(|v| v.parse::<u64>().ok())
        .map(|pages| pages * 4096)
        .unwrap_or(0)
}

/// 窗口差分 CPU 采样器。
pub struct CpuSampler {
    last_ticks: (u64, u64),
    last_at: Instant,
}

impl CpuSampler {
    pub fn new() -> Self {
        Self {
            last_ticks: read_proc_stat_ticks().unwrap_or((0, 0)),
            last_at: Instant::now(),
        }
    }

    /// 返回自上次采样以来的整体 CPU 使用率（0-100，100 = 所有逻辑核打满）。
    pub fn sample_pct(&mut self) -> f64 {
        let now = Instant::now();
        let elapsed = now.duration_since(self.last_at);
        self.last_at = now;
        let ticks = read_proc_stat_ticks().unwrap_or((0, 0));
        let dt_user = ticks.0.saturating_sub(self.last_ticks.0);
        let dt_sys = ticks.1.saturating_sub(self.last_ticks.1);
        self.last_ticks = ticks;

        let elapsed_secs = elapsed.as_secs_f64();
        if elapsed_secs <= 0.0 {
            return 0.0;
        }
        // 单核口径的占用（可 >100）；除以核数得到相对整机的 0-100 使用率。
        let single_core_pct = (dt_user + dt_sys) as f64 / (elapsed_secs * CLK_TCK) * 100.0;
        let cores = std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(1) as f64;
        (single_core_pct / cores).clamp(0.0, 100.0)
    }
}
