//! Benchmark environment detection
//!
//! 基准测试运行环境检测：CPU、内存、操作系统、Rust 版本、PostgreSQL 版本等信息。
//! 这些信息会被包含在报告中，帮助复现和对比测试结果。

use serde::{Deserialize, Serialize};

/// 运行环境快照
///
/// 在基准测试启动时采集一次，附加到报告中。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EnvironmentSnapshot {
    // ── PMP 版本 ──
    /// PMP 版本号（来自 Cargo.toml）
    pub version: String,
    /// Git commit hash（编译时注入）
    pub git_commit: String,

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
    /// 在基准测试启动时采集 CPU、内存、OS、Rust 版本等信息。
    pub async fn capture() -> Self {
        Self {
            version: Self::detect_version(),
            git_commit: Self::detect_git_commit(),
            cpu_cores: Self::detect_cpu_cores(),
            cpu_model: Self::detect_cpu_model(),
            total_memory_bytes: Self::detect_total_memory(),
            available_memory_bytes: Self::detect_available_memory(),
            os_name: std::env::consts::OS.to_string(),
            os_version: Self::detect_os_version(),
            kernel_version: Self::detect_kernel_version(),
            hostname: Self::detect_hostname(),
            rust_version: Self::detect_rust_version(),
            target_triple: Self::detect_target_triple(),
            postgres_version: None, // 由 runner 在连接后填入
            captured_at_ms: Self::now_ms(),
        }
    }

    /// 检测 PMP 版本（编译时注入的 CARGO_PKG_VERSION）
    fn detect_version() -> String {
        env!("CARGO_PKG_VERSION").to_string()
    }

    /// 检测 Git commit hash
    ///
    /// 通过编译时环境变量（由 vergen/build.rs 注入），
    /// 或回退到从 .git/HEAD 读取。
    fn detect_git_commit() -> String {
        option_env!("VERGEN_GIT_SHA")
            .or(option_env!("GIT_COMMIT"))
            .or(option_env!("BUILD_GIT_HASH"))
            .map(|s| s.to_string())
            .unwrap_or_else(|| {
                Self::read_git_head().unwrap_or_else(|| "unknown".to_string())
            })
    }

    /// 尝试从 .git/HEAD 读取 commit
    fn read_git_head() -> Option<String> {
        let repo_dir = if let Ok(manifest) = std::env::var("CARGO_MANIFEST_DIR") {
            manifest
        } else {
            std::env::current_dir().ok()?.to_str()?.to_string()
        };
        let git_dir = format!("{}/../.git", repo_dir);
        let head_paths = [
            format!("{}/HEAD", git_dir),
            format!("{}/ORIG_HEAD", git_dir),
        ];
        for path in &head_paths {
            if let Ok(content) = std::fs::read_to_string(path) {
                let trimmed = content.trim();
                if let Some(ref_path) = trimmed.strip_prefix("ref: ") {
                    let resolved = format!("{}/{}", git_dir, ref_path);
                    if let Ok(resolved_content) = std::fs::read_to_string(&resolved) {
                        return Some(resolved_content.trim().to_string());
                    }
                } else if trimmed.len() == 40 {
                    return Some(trimmed.to_string());
                }
            }
        }
        None
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
            if let Ok(content) = std::fs::read_to_string("/proc/cpuinfo") {
                for line in content.lines() {
                    if let Some(model) = line.strip_prefix("model name") {
                        let model = model.trim_start_matches(':').trim();
                        if !model.is_empty() {
                            return model.to_string();
                        }
                    }
                }
            }
        }
        #[cfg(target_os = "macos")]
        {
            use std::process::Command;
            if let Ok(output) = Command::new("sysctl")
                .args(["-n", "machdep.cpu.brand_string"])
                .output()
            {
                if let Ok(name) = String::from_utf8(output.stdout) {
                    let name = name.trim().to_string();
                    if !name.is_empty() {
                        return name;
                    }
                }
            }
        }
        "unknown".to_string()
    }

    /// 检测总物理内存（字节）
    fn detect_total_memory() -> u64 {
        #[cfg(target_os = "linux")]
        {
            if let Ok(content) = std::fs::read_to_string("/proc/meminfo") {
                for line in content.lines() {
                    if let Some(rest) = line.strip_prefix("MemTotal:") {
                        let parts: Vec<&str> = rest.split_whitespace().collect();
                        if let Some(kb_str) = parts.first() {
                            if let Ok(kb) = kb_str.parse::<u64>() {
                                return kb.saturating_mul(1024);
                            }
                        }
                    }
                }
            }
        }
        #[cfg(target_os = "macos")]
        {
            use std::process::Command;
            if let Ok(output) = Command::new("sysctl")
                .args(["-n", "hw.memsize"])
                .output()
            {
                if let Ok(bytes) = String::from_utf8(output.stdout) {
                    if let Ok(val) = bytes.trim().parse::<u64>() {
                        return val;
                    }
                }
            }
        }
        0
    }

    /// 检测可用物理内存（字节）
    fn detect_available_memory() -> u64 {
        #[cfg(target_os = "linux")]
        {
            if let Ok(content) = std::fs::read_to_string("/proc/meminfo") {
                for line in content.lines() {
                    if let Some(rest) = line.strip_prefix("MemAvailable:") {
                        let parts: Vec<&str> = rest.split_whitespace().collect();
                        if let Some(kb_str) = parts.first() {
                            if let Ok(kb) = kb_str.parse::<u64>() {
                                return kb.saturating_mul(1024);
                            }
                        }
                    }
                }
            }
        }
        0
    }

    /// 检测 OS 版本
    fn detect_os_version() -> String {
        #[cfg(target_os = "linux")]
        {
            if let Ok(content) = std::fs::read_to_string("/etc/os-release") {
                for line in content.lines() {
                    if let Some(val) = line.strip_prefix("PRETTY_NAME=") {
                        return val.trim_matches('"').to_string();
                    }
                }
            }
            for path in &[
                "/etc/lsb-release",
                "/etc/debian_version",
                "/etc/redhat-release",
                "/etc/alpine-release",
            ] {
                if let Ok(content) = std::fs::read_to_string(path) {
                    if let Some(line) = content.lines().next() {
                        if !line.is_empty() {
                            return line.to_string();
                        }
                    }
                }
            }
        }
        String::new()
    }

    /// 检测内核版本
    fn detect_kernel_version() -> String {
        #[cfg(target_os = "linux")]
        {
            if let Ok(content) = std::fs::read_to_string("/proc/version") {
                let parts: Vec<&str> = content.split_whitespace().collect();
                if parts.len() >= 3 && parts[0] == "Linux" {
                    return format!("{} {}", parts[0], parts[2]);
                }
                return content.trim().to_string();
            }
        }
        #[cfg(target_os = "macos")]
        {
            use std::process::Command;
            if let Ok(output) = Command::new("uname").args(["-r"]).output() {
                if let Ok(ver) = String::from_utf8(output.stdout) {
                    return ver.trim().to_string();
                }
            }
        }
        String::new()
    }

    /// 检测主机名
    fn detect_hostname() -> String {
        std::env::var("HOSTNAME")
            .or_else(|_| std::env::var("HOST"))
            .unwrap_or_else(|_| "unknown".to_string())
    }

    /// 检测 Rust 编译器版本
    fn detect_rust_version() -> String {
        option_env!("CARGO_PKG_RUST_VERSION")
            .map(|s| s.to_string())
            .unwrap_or_else(|| "unknown".to_string())
    }

    /// 检测编译目标 triple
    fn detect_target_triple() -> String {
        format!("{}-{}", std::env::consts::ARCH, std::env::consts::OS)
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
        out.push_str(&format!("  Version: {}\n", self.version));
        if !self.git_commit.is_empty() && self.git_commit != "unknown" {
            let short = &self.git_commit[..self.git_commit.len().min(12)];
            out.push_str(&format!("  Commit: {}\n", short));
        }
        out.push_str(&format!(
            "  CPU: {} cores ({})\n",
            self.cpu_cores, self.cpu_model
        ));
        let total_mb = self.total_memory_bytes / 1024 / 1024;
        let avail_mb = self.available_memory_bytes / 1024 / 1024;
        if avail_mb > 0 {
            out.push_str(&format!(
                "  Memory: {} MB total, {} MB available\n",
                total_mb, avail_mb
            ));
        } else if total_mb > 0 {
            out.push_str(&format!("  Memory: {} MB total\n", total_mb));
        } else {
            out.push_str("  Memory: unknown\n");
        }
        out.push_str(&format!("  OS: {}", self.os_name));
        if !self.os_version.is_empty() {
            out.push_str(&format!(" ({})", self.os_version));
        }
        out.push('\n');
        if !self.kernel_version.is_empty() {
            out.push_str(&format!("  Kernel: {}\n", self.kernel_version));
        }
        out.push_str(&format!(
            "  Rust: {} ({})\n",
            self.rust_version, self.target_triple
        ));
        if let Some(pg_ver) = &self.postgres_version {
            out.push_str(&format!("  PostgreSQL: {}\n", pg_ver));
        }
        out
    }
}
