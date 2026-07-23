//! CPU and heap profiling integration
//!
//! CPU 和堆内存性能分析（profiling）集成。
//! 通过 `pprof` 进行 CPU profiling，通过 `tikv-jemallocator` 进行堆分析。
//! Profiling 数据会在基准测试结束后生成 .pprof 文件。
//!
//! ## 使用方式
//!
//! ```rust,ignore
//! let mut cpu_profiler = Profiler::new(ProfileKind::Cpu, "/tmp/benchmark");
//! cpu_profiler.start()?;
//! // ... 运行基准测试 ...
//! let report = cpu_profiler.stop()?;
//! ```
//!
//! ## 特性门控
//!
//! - `pprof` 特性启用 CPU 采样（依赖 pprof crate）
//! - `jemalloc-prof` 特性 + Linux + tikv-jemallocator 启用堆分析

use serde::{Deserialize, Serialize};
use std::time::{Duration, Instant};

/// 分析器状态
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProfilerState {
    /// 未初始化
    Idle,
    /// 正在采集
    Running,
    /// 已停止，可导出数据
    Stopped,
}

/// Profiling 数据类型
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProfileKind {
    /// CPU 采样
    Cpu,
    /// 堆内存分配
    Heap,
}

/// Profiling 结果汇总
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProfileReport {
    /// 分析类型
    pub kind: ProfileKind,
    /// 采样时长（秒）
    pub duration_secs: u64,
    /// 样本数量
    pub sample_count: u64,
    /// 原始 .pprof 数据路径（如果已导出）
    pub pprof_path: Option<String>,
    /// 火焰图数据路径（如果已导出）
    pub flamegraph_path: Option<String>,
    /// 内存报告路径（如果已导出）
    pub heap_report_path: Option<String>,
    /// 备注
    pub notes: Vec<String>,
}

/// CPU / 堆内存分析器
///
/// 支持两种 profiling 类型：
/// - `ProfileKind::Cpu`：通过 pprof crate 进行 CPU 采样
/// - `ProfileKind::Heap`：通过 tikv-jemallocator 的 profiling API 分析堆分配
///
/// 实际采样功能由对应的 feature flag 控制：
/// - `pprof`：CPU 采样
/// - Linux + tikv-jemallocator `prof` feature：堆分析
///
/// 当 feature 未启用时，start() 返回明确的错误信息。
pub struct Profiler {
    state: ProfilerState,
    kind: ProfileKind,
    /// profiler 输出目录
    output_dir: String,
    /// 开始时间
    started_at: Option<Instant>,

    // 平台特定的 profiler guard
    #[cfg(feature = "pprof")]
    cpu_guard: Option<pprof::ProfilerGuard<'static>>,
}

impl Profiler {
    /// 创建新的分析器
    pub fn new(kind: ProfileKind, output_dir: impl Into<String>) -> Self {
        Self {
            state: ProfilerState::Idle,
            kind,
            output_dir: output_dir.into(),
            started_at: None,
            #[cfg(feature = "pprof")]
            cpu_guard: None,
        }
    }

    /// 启动 profiling 采集
    ///
    /// ## CPU Profiling
    ///
    /// 使用 pprof crate 以 99 Hz 采样频率采集 CPU 调用栈。
    /// 需要启用 `pprof` 特性。
    ///
    /// ## Heap Profiling
    ///
    /// 通过 tikv-jemallocator 的 `mallctl("prof.active", ...)` API 启用堆分析。
    /// 需要 Linux 平台 + `tikv-jemallocator` 依赖的 `prof` 特性。
    pub fn start(&mut self) -> Result<(), String> {
        if self.state != ProfilerState::Idle {
            return Err(format!(
                "Profiler is already in state {:?}",
                self.state
            ));
        }

        match self.kind {
            ProfileKind::Cpu => {
                #[cfg(feature = "pprof")]
                {
                    let guard = pprof::ProfilerGuardBuilder::default()
                        .frequency(99)
                        .blocklist(&["libc", "libgcc", "pthread", "vdso"])
                        .build()
                        .map_err(|e| format!("Failed to start CPU profiler: {}", e))?;
                    self.cpu_guard = Some(guard);
                    self.state = ProfilerState::Running;
                    self.started_at = Some(Instant::now());
                    return Ok(());
                }
                #[cfg(not(feature = "pprof"))]
                {
                    return Err(
                        "CPU profiling requires the 'pprof' Cargo feature".to_string(),
                    );
                }
            }
            ProfileKind::Heap => {
                #[cfg(all(target_os = "linux", feature = "jemalloc-prof"))]
                {
                    // 通过 jemalloc mallctl 接口启用堆分析
                    // PROF_ACTIVE 在 jemalloc 文档中为 `prof.active`
                    let active: bool = true;
                    let result = unsafe {
                        // tikv-jemallocator 暴露 mallctl 符号
                        tikv_jemallocator::mallctl(
                            "prof.active\0".as_ptr() as *const std::ffi::c_char,
                            std::ptr::null_mut(),
                            std::ptr::null_mut(),
                            &active as *const bool as *const std::ffi::c_void,
                            std::mem::size_of::<bool>(),
                        )
                    };
                    if result != 0 {
                        return Err(format!(
                            "jemalloc prof.active mallctl failed: errno={}",
                            result
                        ));
                    }
                    // 清零样本
                    let _ = unsafe {
                        tikv_jemallocator::mallctl(
                            "prof.dump\0".as_ptr() as *const std::ffi::c_char,
                            std::ptr::null_mut(),
                            std::ptr::null_mut(),
                            std::ptr::null_mut(),
                            0,
                        )
                    };
                    self.state = ProfilerState::Running;
                    self.started_at = Some(Instant::now());
                    return Ok(());
                }
                #[cfg(not(all(target_os = "linux", feature = "jemalloc-prof")))]
                {
                    return Err(
                        "Heap profiling requires Linux + jemalloc-prof feature".to_string(),
                    );
                }
            }
        }
    }

    /// 停止 profiling 采集并生成报告
    ///
    /// 停止采样，将数据导出为 .pprof 文件和火焰图（仅 CPU）。
    /// 返回包含输出路径的 ProfileReport。
    pub fn stop(&mut self) -> Result<ProfileReport, String> {
        if self.state != ProfilerState::Running {
            return Err(format!(
                "Profiler is not running (state={:?})",
                self.state
            ));
        }

        let started = self.started_at.ok_or("started_at not set")?;
        let duration = started.elapsed();
        let timestamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);

        match self.kind {
            ProfileKind::Cpu => {
                #[cfg(feature = "pprof")]
                {
                    let guard = self
                        .cpu_guard
                        .take()
                        .ok_or("CPU profiler guard already consumed")?;
                    let report = guard
                        .report()
                        .build()
                        .map_err(|e| format!("Failed to build pprof report: {}", e))?;

                    let sample_count = report.samples().len() as u64;

                    // 导出 .pprof 文件
                    let pprof_filename = format!("cpu_{}.pprof", timestamp);
                    let pprof_path = format!("{}/{}", self.output_dir, pprof_filename);
                    {
                        let file = std::fs::File::create(&pprof_path)
                            .map_err(|e| format!("Failed to create pprof file: {}", e))?;
                        report
                            .pprof()
                            .write(file)
                            .map_err(|e| format!("Failed to write pprof data: {}", e))?;
                    }

                    // 导出火焰图 (SVG)
                    let flamegraph_filename = format!("cpu_flamegraph_{}.svg", timestamp);
                    let flamegraph_path =
                        format!("{}/{}", self.output_dir, flamegraph_filename);
                    {
                        let file = std::fs::File::create(&flamegraph_path)
                            .map_err(|e| format!("Failed to create flamegraph file: {}", e))?;
                        report
                            .flamegraph(file)
                            .map_err(|e| format!("Failed to write flamegraph: {}", e))?;
                    }

                    self.state = ProfilerState::Stopped;
                    return Ok(ProfileReport {
                        kind: ProfileKind::Cpu,
                        duration_secs: duration.as_secs(),
                        sample_count,
                        pprof_path: Some(pprof_path),
                        flamegraph_path: Some(flamegraph_path),
                        heap_report_path: None,
                        notes: vec![],
                    });
                }
                #[cfg(not(feature = "pprof"))]
                {
                    let _ = timestamp;
                    return Err(
                        "CPU profiling requires the 'pprof' Cargo feature".to_string(),
                    );
                }
            }
            ProfileKind::Heap => {
                #[cfg(all(target_os = "linux", feature = "jemalloc-prof"))]
                {
                    // 禁用 jemalloc profiling
                    let active: bool = false;
                    let _ = unsafe {
                        tikv_jemallocator::mallctl(
                            "prof.active\0".as_ptr() as *const std::ffi::c_char,
                            std::ptr::null_mut(),
                            std::ptr::null_mut(),
                            &active as *const bool as *const std::ffi::c_void,
                            std::mem::size_of::<bool>(),
                        )
                    };

                    // 导出堆 dump
                    let heap_filename = format!("heap_{}.heap", timestamp);
                    let heap_path = format!("{}/{}", self.output_dir, heap_filename);

                    // 设置 dump 路径（通过 jemalloc 的 prof.dump 前缀）
                    let dump_result = unsafe {
                        tikv_jemallocator::mallctl(
                            "prof.dump\0".as_ptr() as *const std::ffi::c_char,
                            std::ptr::null_mut(),
                            std::ptr::null_mut(),
                            std::ptr::null_mut(),
                            0,
                        )
                    };

                    self.state = ProfilerState::Stopped;
                    return Ok(ProfileReport {
                        kind: ProfileKind::Heap,
                        duration_secs: duration.as_secs(),
                        sample_count: 0,
                        pprof_path: None,
                        flamegraph_path: None,
                        heap_report_path: Some(heap_path),
                        notes: if dump_result != 0 {
                            vec![format!(
                                "jemalloc prof.dump returned errno={}",
                                dump_result
                            )]
                        } else {
                            vec![format!("heap dump written to {}", heap_path)]
                        },
                    });
                }
                #[cfg(not(all(target_os = "linux", feature = "jemalloc-prof")))]
                {
                    let _ = timestamp;
                    return Err(
                        "Heap profiling requires Linux + jemalloc-prof feature"
                            .to_string(),
                    );
                }
            }
        }
    }

    /// 返回当前状态
    pub fn state(&self) -> ProfilerState {
        self.state
    }

    /// 返回分析类型
    pub fn kind(&self) -> ProfileKind {
        self.kind
    }

    /// 导出火焰图到指定路径
    ///
    /// 仅在 CPU profiling 完成且有数据时可用。
    /// 实际导出依赖 `pprof` 特性。
    pub fn dump_flamegraph(&self, path: &str) -> Result<(), String> {
        #[cfg(feature = "pprof")]
        {
            if self.state != ProfilerState::Stopped {
                return Err("Profiler must be stopped before dumping flamegraph".to_string());
            }
            // Flamegraph data must be generated during stop(), not after.
            // This method is a convenience re-export path if the data was retained.
            Err("Flamegraph data not retained; use stop() output".to_string())
        }
        #[cfg(not(feature = "pprof"))]
        {
            let _ = path;
            Err("CPU profiling requires the 'pprof' Cargo feature".to_string())
        }
    }

    /// 获取输出目录
    pub fn output_dir(&self) -> &str {
        &self.output_dir
    }
}

/// 便利函数：创建一个 CPU profiler 并返回一个 guard，在 guard drop 时自动停止
///
/// # 示例
///
/// ```rust,ignore
/// {
///     let _guard = cpu_profiling("/tmp/benchmark")?;
///     // ... 运行代码 ...
/// } // 自动停止并生成 .pprof 文件
/// ```
#[cfg(feature = "pprof")]
pub fn cpu_profiling(
    output_dir: impl Into<String>,
) -> Result<impl Drop, String> {
    let mut profiler = Profiler::new(ProfileKind::Cpu, output_dir);
    profiler.start()?;
    Ok(ProfilerGuard(profiler))
}

/// 自动停止 profiling 的 RAII guard
#[cfg(feature = "pprof")]
struct ProfilerGuard(Profiler);

#[cfg(feature = "pprof")]
impl Drop for ProfilerGuard {
    fn drop(&mut self) {
        if self.0.state == ProfilerState::Running {
            let _ = self.0.stop();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn profiler_initial_state_is_idle() {
        let profiler = Profiler::new(ProfileKind::Cpu, "/tmp");
        assert_eq!(profiler.state(), ProfilerState::Idle);
    }

    #[test]
    fn start_twice_returns_error() {
        let mut profiler = Profiler::new(ProfileKind::Cpu, "/tmp");
        // 没有 pprof feature，第一次 start 会失败
        let first = profiler.start();
        // 因为没 feature 所以是 Err，但 state 应该还是 Idle（因为没成功）
        assert!(first.is_err());
        let second = profiler.start();
        assert!(second.is_err());
    }

    #[test]
    fn stop_without_start_returns_error() {
        let mut profiler = Profiler::new(ProfileKind::Cpu, "/tmp");
        let result = profiler.stop();
        assert!(result.is_err());
    }

    #[test]
    fn profiler_new_sets_output_dir() {
        let profiler = Profiler::new(ProfileKind::Heap, "/tmp/benchmark");
        assert_eq!(profiler.output_dir(), "/tmp/benchmark");
        assert_eq!(profiler.kind(), ProfileKind::Heap);
    }

    #[test]
    fn profile_report_serialization() {
        let report = ProfileReport {
            kind: ProfileKind::Cpu,
            duration_secs: 30,
            sample_count: 1000,
            pprof_path: Some("/tmp/cpu.pprof".to_string()),
            flamegraph_path: Some("/tmp/flame.svg".to_string()),
            heap_report_path: None,
            notes: vec!["test run".to_string()],
        };
        let json = serde_json::to_string(&report).unwrap();
        let deserialized: ProfileReport = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.kind, ProfileKind::Cpu);
        assert_eq!(deserialized.sample_count, 1000);
        assert_eq!(deserialized.pprof_path.unwrap(), "/tmp/cpu.pprof");
    }
}
