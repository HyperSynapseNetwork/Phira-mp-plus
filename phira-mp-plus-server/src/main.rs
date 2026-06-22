//! Phira-mp+ 服务器入口
//!
//! 增强的多人游戏服务端，支持 WASM 插件系统、CLI 管理控制台和扩展 API。

use anyhow::Result;
use clap::Parser;
use phira_mp_plus_server::server::{PlusConfig, PlusServer};
use std::path::Path;
use tracing::level_filters::LevelFilter;
use tracing::{info, warn};
use tracing_appender::non_blocking::WorkerGuard;
use tracing_subscriber::{EnvFilter, fmt, prelude::*};

/// Phira-mp+ 命令行参数
#[derive(Parser, Debug)]
#[clap(
    name = "phira-mp-plus-server",
    author,
    version,
    about = "Phira-mp+ - 增强版 Phira 多人游戏服务端",
    long_about = "基于 Phira-mp 二次开发的增强版多人游戏服务端。
支持 WASM 插件系统、CLI 管理控制台和扩展 API。"
)]
struct Args {
    /// 服务器端口
    #[arg(short, long, default_value_t = 12346, help = "服务器监听端口")]
    port: u16,

    /// 插件目录
    #[arg(short = 'd', long = "plugins-dir", default_value = "plugins", help = "WASM 插件目录路径")]
    plugins_dir: String,

    /// 扩展数据持久化文件
    #[arg(short = 'e', long = "ext-file", default_value = "data/extensions.json", help = "扩展数据持久化 JSON 文件路径")]
    extensions_file: String,

    /// 禁用 CLI 管理控制台
    #[arg(long = "no-cli", help = "禁用交互式 CLI 管理控制台")]
    no_cli: bool,

    /// 日志文件基础名称
    #[arg(short, long, default_value = "phira-mp-plus", help = "日志文件基础名称")]
    log_file: String,

    /// 允许旁观的管理员用户ID列表
    #[arg(short = 'm', long = "monitor", help = "允许旁观的用户ID（可多次指定）")]
    monitors: Vec<i32>,

    /// HTTP/SSE 服务端口
    #[arg(long = "http-port", default_value_t = 12347, help = "中央 HTTP/SSE 服务端口")]
    http_port: u16,
}

fn init_log(file: &str) -> Result<WorkerGuard> {
    let log_dir = Path::new("log");
    if log_dir.exists() {
        if !log_dir.is_dir() {
            panic!("log exists and is not a folder");
        }
    } else {
        std::fs::create_dir(log_dir).expect("failed to create log folder");
    }

    let (non_blocking, guard) =
        tracing_appender::non_blocking(tracing_appender::rolling::hourly(log_dir, file));

    // 文件日志：记录所有 DEBUG 及以上级别
    let file_layer = fmt::layer()
        .with_writer(non_blocking)
        .with_filter(LevelFilter::DEBUG);

    // 终端日志：无 RUST_LOG 时默认 INFO 级别，并压制嘈杂的第三方库
    let stdout_layer = fmt::layer()
        .with_writer(std::io::stdout)
        .with_filter(
            EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| EnvFilter::new("info"))
                .add_directive("hyper=info".parse().unwrap())
                .add_directive("rustls=info".parse().unwrap())
                .add_directive("isahc=info".parse().unwrap())
                .add_directive("h2=info".parse().unwrap())
                .add_directive("reqwest=info".parse().unwrap())
                .add_directive("wasmtime=info".parse().unwrap()),
        );

    tracing_subscriber::registry()
        .with(file_layer)
        .with(stdout_layer)
        .init();

    Ok(guard)
}

#[tokio::main]
async fn main() -> Result<()> {
    let _guard = init_log("phira-mp-plus")?;

    let args = Args::parse();

    // 自动创建数据目录
    let data_dir = Path::new("data");
    if !data_dir.exists() {
        std::fs::create_dir_all(data_dir).expect("failed to create data directory");
    }
    // 自动创建插件目录
    let plugins_dir = Path::new(&args.plugins_dir);
    if !plugins_dir.exists() {
        std::fs::create_dir_all(plugins_dir).expect("failed to create plugins directory");
    }

    let config = PlusConfig {
        port: args.port,
        http_port: args.http_port,
        monitors: if args.monitors.is_empty() {
            vec![2]
        } else {
            args.monitors
        },
        plugins_dir: args.plugins_dir,
        extensions_file: Some(args.extensions_file),
        cli_enabled: !args.no_cli,
    };

    println!();
    println!("  Phira-mp+ v{}", env!("CARGO_PKG_VERSION"));
    println!();

    // 创建服务器
    let server = PlusServer::new(config).await?;

    // 启动 CLI 管理控制台
    server.start_cli().await?;

    println!("  ● 服务器已启动，监听端口 {} ── 输入 exit 关闭", server.state.config.port);

    // 主循环 - 接收连接（直到收到关闭信号）
    loop {
        tokio::select! {
            result = server.accept() => {
                if let Err(err) = result {
                    warn!("accept error: {err:?}");
                }
            }
            _ = server.state.shutdown.notified() => {
                info!("received shutdown signal, stopping server...");
                break;
            }
        }
    }

    // 清理
    println!("正在清理资源...");
    // 清理所有会话
    {
        let sessions = server.state.sessions.read().await;
        for session in sessions.values() {
            let _ = session
                .stream
                .send(phira_mp_common::ServerCommand::Message(
                    phira_mp_common::Message::Chat {
                        user: 0,
                        content: "服务器正在关闭...".to_string(),
                    },
                ))
                .await;
        }
    }
    // 清理插件
    server.state.plugin_manager.cleanup_all().await;
    // 持久化扩展数据
    if let Err(e) = server.state.extensions.persist().await {
        warn!("failed to persist extension data: {e}");
    }

    println!("服务器已关闭。");
    Ok(())
}
