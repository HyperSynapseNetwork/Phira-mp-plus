//! Phira-mp+ 服务器入口
//!
//! 增强的多人游戏服务端，支持 WASM 插件系统、CLI 管理控制台和扩展 API。

use anyhow::Result;
use clap::Parser;
use phira_mp_plus_server::server::{PlusConfig, PlusServer};
use std::path::Path;
use tracing::{Level, level_filters::LevelFilter, warn};
use tracing_appender::non_blocking::WorkerGuard;
use tracing_subscriber::{EnvFilter, filter, fmt, prelude::*};

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
    #[arg(short, long, default_value = "plugins", help = "WASM 插件目录路径")]
    plugins_dir: String,

    /// 扩展数据持久化文件
    #[arg(short = 'e', long = "ext-file", help = "扩展数据持久化 JSON 文件路径")]
    extensions_file: Option<String>,

    /// 禁用 CLI 管理控制台
    #[arg(long = "no-cli", help = "禁用交互式 CLI 管理控制台")]
    no_cli: bool,

    /// 日志文件基础名称
    #[arg(short, long, default_value = "phira-mp-plus", help = "日志文件基础名称")]
    log_file: String,

    /// 允许旁观的管理员用户ID列表
    #[arg(short = 'm', long = "monitor", help = "允许旁观的用户ID（可多次指定）")]
    monitors: Vec<i32>,
}

fn init_log(file: &str) -> Result<WorkerGuard> {
    use tracing_log::LogTracer;

    let log_dir = Path::new("log");
    if log_dir.exists() {
        if !log_dir.is_dir() {
            panic!("log exists and is not a folder");
        }
    } else {
        std::fs::create_dir(log_dir).expect("failed to create log folder");
    }

    LogTracer::init()?;

    let (non_blocking, guard) =
        tracing_appender::non_blocking(tracing_appender::rolling::hourly(log_dir, file));

    let subscriber = tracing_subscriber::registry()
        .with(
            fmt::layer()
                .with_writer(non_blocking)
                .with_filter(LevelFilter::DEBUG),
        )
        .with(
            fmt::layer()
                .with_writer(std::io::stdout)
                .with_filter(EnvFilter::from_default_env()),
        )
        .with(
            filter::Targets::new()
                .with_target("hyper", Level::INFO)
                .with_target("rustls", Level::INFO)
                .with_target("isahc", Level::INFO)
                .with_default(Level::TRACE),
        );

    tracing::subscriber::set_global_default(subscriber)
        .expect("unable to set global subscriber");
    Ok(guard)
}

#[tokio::main]
async fn main() -> Result<()> {
    let _guard = init_log("phira-mp-plus")?;

    let args = Args::parse();

    let config = PlusConfig {
        port: args.port,
        monitors: if args.monitors.is_empty() {
            vec![2]
        } else {
            args.monitors
        },
        plugins_dir: args.plugins_dir,
        extensions_file: args.extensions_file,
        cli_enabled: !args.no_cli,
    };

    println!("╔══════════════════════════════════════════════════════════╗");
    println!("║                                                          ║");
    println!("║     Phira-mp+ v{}                                   ║", env!("CARGO_PKG_VERSION"));
    println!("║     增强版 Phira 多人游戏服务端                          ║");
    println!("║                                                          ║");
    println!("╚══════════════════════════════════════════════════════════╝");
    println!();

    // 创建服务器
    let server = PlusServer::new(config).await?;

    // 启动 CLI 管理控制台
    server.start_cli().await?;

    println!("服务器已启动，等待连接...");

    // 主循环 - 接收连接
    loop {
        if let Err(err) = server.accept().await {
            warn!("failed to accept: {err:?}");
        }
    }
}
