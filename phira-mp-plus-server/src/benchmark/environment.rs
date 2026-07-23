//! Benchmark environment detection
//!
//! 基准测试运行环境检测：CPU、内存、操作系统、Rust 版本、PostgreSQL 版本等信息。
//! 这些信息会被包含在报告中，帮助复现和对比测试结果。

use serde::{Deserialize, Serialize};
use std::time::Duration;

/// 运行环境快照
///
/// 在基准测试启动时采集一次，附加到报告中。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EnvironmentSnapshot {
    // ── 硬件 ──
    /// CPU 逻辑核心数
    pub cpu_cores: usize,
    /// CPU 型号名称
    pub cpu_model: String,
    /// 总内存（字节）
    pub total_memory_bytes: u64,
    /// 可用内存（字节）
    pub available_memory_bytes: u64,

    // ── 操作系统 ──
    /// 操作系统名称
    pub os_name: String,
    /// 操作系统版本
    pub os_version: String,
    /// 内核版本（uname -r）
    pub kernel_version: String,
    /// 主机名
    pub hostname: String,

    // ── Rust ──
    /// Rust 编译器版本（rustc --version）
    pub rust_version: String,
    /// 编译目标 triple
    pub target_triple: String,

    // ── PostgreSQL ──
    /// PostgreSQL 版本（从数据库查询）
    pub postgres_version: Option<String>,

    // ── 时间 ──
    /// 采集时间戳（Unix 毫秒）
    pub captured_at_ms: i64,
}

impl EnvironmentSnapshot {
    /// 采集当前环境信息
    ///
    /// TODO: 实现在基准测试启动时采集 CPU、内存、OS、Rust 版本等信息。
    pub async fn capture() -> Self {
        Self {
            cpu_cores: Self::detect_cpu_cores(),
            cpu_model: Self::detect_cpu_model(),
            total_memory_bytes: Self::detect_total_memory(),
            available_memory_bytes: 0, // TODO: 实现可用内存检测
            os_name: std::env::consts::OS.to_string(),
            os_version: String::new(),    // TODO: 实现 OS 版本检测
            kernel_version: String::new(), // TODO: 实现内核版本检测
            hostname: Self::detect_hostname(),
            rust_version: Self::detect_rust_version(),
            target_triple: std::env::consts::ARCH.to_string(),
            postgres_version: None, // TODO: 实现 PG 版本查询
            captured_at_ms: Self::now_ms(),
        }
    }

    /// 检测 CPU 逻辑核心数
    fn detect_cpu_cores() -> usize {
        std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(1)
    }

    /// 检测 CPU 型号
    fn detect_cpu_model() -> String {
        #[cfg(target_os = "linux")]
        {
            // TODO: 读取 /proc/cpuinfo 获取 model name
        }
        "unknown".to_string()
    }

    /// 检测总内存
    fn detect_total_memory() -> u64 {
        #[cfg(target_os = "linux")]
        {
            // TODO: 读取 /proc/meminfo 获取 MemTotal
        }
        0
    }

    /// 检测主机名
    fn detect_hostname() -> String {
        std::env::var("HOSTNAME")
            .or_else(|_| std::env::var("HOST"))
            .unwrap_or_else(|_| "unknown".to_string())
    }

    /// 检测 Rust 版本
    fn detect_rust_version() -> String {
        option_env!("CARGO_PKG_RUST_VERSION")
            .map(|s| s.to_string())
            .unwrap_or_else(|| "unknown".to_string())
    }

    fn now_ms() -> i64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as i64)
            .unwrap_or(0)
    }

    /// 格式化环境信息为人类可读字符串
    pub fn format_text(&self) -> String {
        let mut out = String::new();
        out.push_str(&format!("  CPU: {} cores ({})\n", self.cpu_cores, self.cpu_model));
        out.push_str(&format!("  Memory: {} MB total\n", self.total_memory_bytes / 1024 / 1024));
        out.push_str(&format!("  OS: {} {}\n", self.os_name, self.os_version));
        out.push_str(&format!("  Rust: {} ({})\n", self.rust_version, self.target_triple));
        if let Some(pg_ver) = &self.postgres_version {
            out.push_str(&format!("  PostgreSQL: {}\n", pg_ver));
        }
        out
    }
}
