//! Phira-mp+ 服务器入口
//!
//! 增强的多人游戏服务端，支持 WASM 插件系统、CLI 管理控制台和扩展 API。
//! 配置加载顺序（后覆盖前）：YAML 配置文件 → 环境变量 → CLI 参数

use anyhow::Result;
use clap::Parser;
use phira_mp_plus_server::cli::CliHandler;
use phira_mp_plus_server::server::{PlusConfig, PlusConfigCli, PlusServer};
use std::io;
use std::path::Path;
use std::sync::Arc;
use tokio::sync::mpsc;
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

    /// YAML 配置文件路径
    #[arg(short = 'c', long = "config", default_value = "server_config.yml", help = "YAML 配置文件路径")]
    config: String,
}

// ── 统一的 tracing 终端 writer ──
// TUI 模式将日志发送到 mpsc 通道，普通模式输出到 stdout

enum OutputWriter {
    Stdout(Box<dyn io::Write + Send>),
    Chan(mpsc::UnboundedSender<String>, Vec<u8>),
}

impl io::Write for OutputWriter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        match self {
            OutputWriter::Stdout(w) => w.write(buf),
            OutputWriter::Chan(_, data) => {
                data.extend_from_slice(buf);
                Ok(buf.len())
            }
        }
    }
    fn flush(&mut self) -> io::Result<()> {
        match self {
            OutputWriter::Stdout(w) => w.flush(),
            OutputWriter::Chan(tx, data) => {
                if !data.is_empty() {
                    let s = String::from_utf8_lossy(data).to_string();
                    data.clear();
                    let _ = tx.send(s);
                }
                Ok(())
            }
        }
    }
}

impl Drop for OutputWriter {
    fn drop(&mut self) {
        let _ = <Self as io::Write>::flush(self);
    }
}

struct OutputWriterMaker(Option<mpsc::UnboundedSender<String>>);

impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for OutputWriterMaker {
    type Writer = OutputWriter;

    fn make_writer(&'a self) -> Self::Writer {
        match &self.0 {
            Some(tx) => OutputWriter::Chan(tx.clone(), Vec::new()),
            None => OutputWriter::Stdout(Box::new(io::sink())),
        }
    }
}

fn init_log(file: &str, log_tx: Option<mpsc::UnboundedSender<String>>) -> Result<WorkerGuard> {
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

    // 文件日志：所有 DEBUG 及以上级别，无 ANSI
    let file_layer = fmt::layer()
        .with_writer(non_blocking)
        .with_ansi(false)
        .with_filter(LevelFilter::DEBUG);

    // 终端过滤器
    let filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new("info"))
        .add_directive("hyper=info".parse().unwrap())
        .add_directive("rustls=info".parse().unwrap())
        .add_directive("isahc=info".parse().unwrap())
        .add_directive("h2=info".parse().unwrap())
        .add_directive("reqwest=info".parse().unwrap())
        .add_directive("wasmtime=info".parse().unwrap());

    // stdout 层
    let stdout_layer = fmt::layer()
        .with_writer(io::stdout)
        .with_ansi(false)
        .with_filter(filter.clone());

    // TUI 通道层（有通道时发到通道，否则也写 stdout）
    let tui_layer = fmt::layer()
        .with_writer(OutputWriterMaker(log_tx))
        .with_ansi(false)
        .with_filter(filter);

    // 同时注册所有层，tracing_subscriber 会自动去重 stdout
    tracing_subscriber::registry()
        .with(file_layer)
        .with(stdout_layer)
        .with(tui_layer)
        .init();

    Ok(guard)
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();
    let cli_enabled = !args.no_cli;

    // 通道始终创建（CLI 处理器需要，TUI 和 stdin 只是不同前端）
    let (cmd_tx, cmd_rx) = if cli_enabled {
        let (tx, rx) = mpsc::unbounded_channel();
        (Some(tx), Some(rx))
    } else {
        (None, None)
    };
    let (out_tx, out_rx) = if cli_enabled {
        let (tx, rx) = mpsc::unbounded_channel();
        (Some(tx), Some(rx))
    } else {
        (None, None)
    };
    let (log_tx, log_rx) = if cli_enabled {
        let (tx, rx) = mpsc::unbounded_channel();
        (Some(tx), Some(rx))
    } else {
        (None, None)
    };

    // ── 初始化日志（此后应使用 info! / warn!，避免直接 println!） ──
    let _guard = init_log(&args.log_file, log_tx)?;

    // ── 自动创建数据 & 插件目录 ──
    let data_dir = Path::new("data");
    if !data_dir.exists() {
        std::fs::create_dir_all(data_dir).expect("failed to create data directory");
    }
    let plugins_dir = Path::new(&args.plugins_dir);
    if !plugins_dir.exists() {
        std::fs::create_dir_all(plugins_dir).expect("failed to create plugins directory");
    }

    // ── 加载配置（三层覆盖：YAML 配置 < 环境变量 < CLI 参数） ──
    let config_path = &args.config;
    let mut config = if Path::new(config_path).exists() {
        info!("loading config from '{config_path}'");
        match PlusConfig::from_yaml(config_path) {
            Ok(cfg) => cfg,
            Err(e) => {
                warn!("failed to load config '{config_path}': {e}, using defaults");
                PlusConfig::default()
            }
        }
    } else {
        info!("config file '{config_path}' not found, using defaults");
        PlusConfig::default()
    };

    // CLI 参数覆盖
    let cli_overrides = PlusConfigCli {
        port: args.port,
        http_port: args.http_port,
        monitors: if args.monitors.is_empty() {
            config.monitors.clone()
        } else {
            args.monitors
        },
        plugins_dir: args.plugins_dir,
        extensions_file: Some(args.extensions_file),
        no_cli: args.no_cli,
        log_file: args.log_file,
    };
    config = config.merge_cli(cli_overrides);

    if config.monitors.is_empty() {
        config.monitors = vec![2];
    }

    // ── 创建服务器 ──
    let server = PlusServer::new(config).await?;

    // ── 启动 CLI 处理器（接收 TUI 或 stdin 发来的命令） ──
    if let (Some(cmd_rx), Some(out_tx)) = (cmd_rx, out_tx) {
        let state = Arc::clone(&server.state);
        tokio::spawn(async move {
            let cli = CliHandler::new(state, out_tx);
            cli.start(cmd_rx).await;
        });
        info!("CLI management console started");
    }

    // ── 启动 TUI（screen/tmux 下也使用 TUI，部分终端可正常显示） ──
    let tui_handle = if let (Some(cmd_tx), Some(out_rx), Some(log_rx)) = (cmd_tx, out_rx, log_rx) {
        Some(std::thread::spawn(move || {
            if let Err(e) = phira_mp_plus_server::cli_tui::run_tui(cmd_tx, out_rx, log_rx) {
                eprintln!("TUI error (try --no-cli): {e}");
            }
        }))
    } else {
        info!("CLI management console disabled, logs to stdout");
        None
    };

    info!(
        "Server started on port {} (http port {})",
        server.state.config.port,
        server.state.config.http_port,
    );

    // ── 主循环 - 接受连接（直到收到关闭信号） ──
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

    // ── 清理 ──
    info!("cleaning up resources...");
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
    server.state.plugin_manager.cleanup_all().await;
    if let Err(e) = server.state.extensions.persist().await {
        warn!("failed to persist extension data: {e}");
    }

    // 等待 TUI 线程结束
    if let Some(handle) = tui_handle {
        let _ = handle.join();
    }

    info!("Server shut down.");
    Ok(())
}
