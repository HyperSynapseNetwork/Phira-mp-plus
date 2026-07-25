//! Local Mock Phira HTTP server
//!
//! 本地 Mock Phira HTTP 服务器，在 Real 模式下替代真实的 Phira API。
//! 支持可配置的延迟、抖动、错误率和响应大小，用于测试 PMP 在
//! 各种 Phira API 响应行为下的表现。

use serde::{Deserialize, Serialize};

/// Mock Phira 服务器配置
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MockPhiraConfig {
    /// 模拟的 API 响应延迟（毫秒）
    pub delay_ms: u64,
    /// 随机的延迟抖动范围（毫秒）
    pub jitter_ms: u64,
    /// 模拟的错误率（0.0 ~ 1.0），0.01 表示 1% 的请求会返回错误
    pub error_rate: f64,
    /// 模拟的请求超时时间
    pub timeout_ms: u64,
    /// 随机种子，用于确定性回放
    pub seed: u64,
    /// 模拟的响应体大小（字节）
    pub response_size: usize,
    /// 监听的地址
    pub listen_addr: String,
    /// 是否记录所有请求日志
    pub verbose: bool,
}

impl Default for MockPhiraConfig {
    fn default() -> Self {
        Self {
            delay_ms: 5,
            jitter_ms: 2,
            error_rate: 0.0,
            timeout_ms: 30_000,
            seed: 114_514,
            response_size: 1024,
            listen_addr: "127.0.0.1:9877".to_string(),
            verbose: false,
        }
    }
}

/// Mock Phira 服务器
///
/// 在后台启动一个 Axum HTTP 服务器，模拟 Phira API 端点。
/// 提供 start / stop 方法控制生命周期。
pub struct MockPhiraServer {
    config: MockPhiraConfig,
    // TODO: 添加 shutdown 信号发送端
    // shutdown_tx: tokio::sync::oneshot::Sender<()>,
}

impl MockPhiraServer {
    /// 使用给定配置创建 Mock Phira 服务器（尚未启动）
    pub fn new(config: MockPhiraConfig) -> Self {
        Self { config }
    }

    /// 启动 Mock Phira HTTP 服务器
    ///
    /// TODO: 实现 Axum 路由、延迟模拟、错误注入、请求日志。
    pub async fn start(&self) -> Result<(), String> {
        // TODO: 实现 Mock Phira 服务器启动

        // 模拟端点：
        // - POST /api/auth/login → 返回模拟 token
        // - GET /api/chart/<id> → 返回模拟谱面数据
        // - GET /api/record/<id> → 返回模拟记录数据
        // - POST /api/record/upload → 返回成功

        Err("MockPhiraServer::start not yet implemented".to_string())
    }

    /// 停止 Mock Phira HTTP 服务器
    pub async fn stop(&self) -> Result<(), String> {
        // TODO: 发送 shutdown 信号并等待服务器退出
        Err("MockPhiraServer::stop not yet implemented".to_string())
    }

    /// 返回服务器当前监听地址
    pub fn listen_addr(&self) -> &str {
        &self.config.listen_addr
    }

    /// 返回配置引用
    pub fn config(&self) -> &MockPhiraConfig {
        &self.config
    }
}
