#[cfg(target_os = "linux")]
#[global_allocator]
static ALLOC: tikv_jemallocator::Jemalloc = tikv_jemallocator::Jemalloc;

use anyhow::Result;
use clap::Parser;
use phira_mp_plus_server::cli::CliHandler;
use phira_mp_plus_server::server::{PlusConfig, PlusConfigCli, PlusServer};
use phira_mp_plus_server::terminal::{ConsoleMode, TerminalProfile};
use std::path::Path;
use std::sync::Arc;
use tokio::sync::mpsc;
use tracing::{info, warn};

#[derive(Parser, Debug)]
#[command(
    name = "phira-mp-plus-server",
    author,
    version,
    about = "Enhanced Phira multiplayer server",
    long_about = "Phira multiplayer server with WASM plugins, an administrative console, and extension APIs."
)]
struct Args {
    #[arg(short, long, default_value_t = 12346, help = "TCP listen port")]
    port: u16,

    #[arg(
        short = 'd',
        long = "plugins-dir",
        default_value = "plugins",
        help = "WASM plugin directory"
    )]
    plugins_dir: String,

    #[arg(
        short = 'e',
        long = "ext-file",
        default_value = "data/extensions.json",
        help = "Extension data file"
    )]
    extensions_file: String,

    #[arg(long = "no-cli", help = "Disable the administrative console")]
    no_cli: bool,

    #[arg(short, long, default_value = "phira-mp-plus", help = "Log file prefix")]
    log_file: String,

    #[arg(
        short = 'm',
        long = "monitor",
        help = "User ID allowed to spectate; may be repeated"
    )]
    monitors: Vec<i32>,

    #[arg(
        long = "http-port",
        default_value_t = 12347,
        help = "HTTP and SSE listen port"
    )]
    http_port: u16,

    #[arg(
        long = "proxy-port",
        default_value_t = 0,
        help = "PROXY protocol HTTP port (0 = disabled). Set to 12344 for reverse proxy support."
    )]
    proxy_protocol_port: u16,

    #[arg(
        short = 'c',
        long = "config",
        default_value = "server_config.yml",
        help = "YAML configuration file"
    )]
    config: String,
}

#[tokio::main]
async fn main() -> Result<()> {
    let terminal = TerminalProfile::detect();
    terminal.apply_environment();
    let args = Args::parse();

    let (base_config, config_load) = load_config(&args.config);
    let mut config = base_config.merge_cli(PlusConfigCli {
        port: args.port,
        http_port: args.http_port,
        proxy_protocol_port: args.proxy_protocol_port,
        monitors: args.monitors.clone(),
        plugins_dir: args.plugins_dir.clone(),
        extensions_file: Some(args.extensions_file.clone()),
        no_cli: args.no_cli,
        log_file: args.log_file.clone(),
    });
    if config.monitors.is_empty() {
        config.monitors.push(2);
    }

    std::fs::create_dir_all("data")?;
    std::fs::create_dir_all(&config.plugins_dir)?;

    let cli_enabled = config.cli_enabled;
    let (cmd_tx, cmd_rx) = optional_channel(cli_enabled);
    let (out_tx, out_rx) = optional_channel(cli_enabled);
    let (log_tx, log_rx) = optional_channel(cli_enabled);
    let _log_guard = phira_mp_plus_server::logging::init(&args.log_file, log_tx)?;
    config_load.report(&args.config);

    let server = PlusServer::new(config).await?;

    if let (Some(cmd_rx), Some(out_tx)) = (cmd_rx, out_tx) {
        let state = Arc::clone(&server.state);
        tokio::spawn(async move {
            CliHandler::new(state, out_tx).start(cmd_rx).await;
        });
        info!("CLI management console started");
    }

    let console_handle = match (cmd_tx, out_rx, log_rx) {
        (Some(cmd_tx), Some(out_rx), Some(log_rx)) => {
            let mode = terminal.console_mode();
            let screen_compat = terminal.is_screen();
            if screen_compat {
                info!("GNU Screen detected; using conservative TUI capabilities with Ctrl+H backspace compatibility");
            }
            Some(std::thread::spawn(move || {
                // 在非 tmux 的低兼容性终端下，提示安装 tmux 获得更好的 TUI 体验
                let has_tmux = std::env::var_os("TMUX").is_some();
                let term = std::env::var("TERM").unwrap_or_default();
                let is_low_compat = term.is_empty() || term == "dumb"
                    || term.starts_with("screen") || term == "linux"
                    || term == "ansi" || term == "cons25";
                if is_low_compat && !has_tmux {
                    eprintln!("\n  ⚠ 当前终端兼容性较低，管理控制台将以降级模式运行。");
                    eprintln!("  💡 建议安装 tmux 以获得完整的 TUI 体验：");
                    if std::fs::metadata("/etc/debian_version").is_ok() {
                        eprintln!("     apt install tmux");
                    } else if std::fs::metadata("/etc/redhat-release").is_ok() {
                        eprintln!("     yum install tmux");
                    } else if std::fs::metadata("/etc/arch-release").is_ok() {
                        eprintln!("     pacman -S tmux");
                    } else if std::path::Path::new("/usr/local/bin/brew").exists() {
                        eprintln!("     brew install tmux");
                    } else {
                        eprintln!("     # 请使用系统包管理器安装 tmux");
                    }
                    eprint!("\n  输入 y 继续启动 [y/N]: ");
                    use std::io::Write;
                    let _ = std::io::stdout().flush();
                    let mut input = String::new();
                    let proceed = std::io::stdin().read_line(&mut input)
                        .map(|_| input.trim().to_lowercase() == "y")
                        .unwrap_or(false);
                    if !proceed {
                        eprintln!("  已取消启动。");
                        return;
                    }
                }
                match mode {
                    ConsoleMode::Tui(capabilities) => {
                        if let Err(err) =
                            phira_mp_plus_server::cli_tui::run_tui(cmd_tx, out_rx, log_rx, capabilities)
                        {
                            eprintln!("TUI error: {err}");
                        }
                    }
                    ConsoleMode::Line => {
                        phira_mp_plus_server::cli_tui::run_stdin_cli_with_logs(
                            cmd_tx, out_rx, log_rx, screen_compat,
                        );
                    }
                }
            }))
            }))
        }
        _ => {
            info!("CLI management console disabled; logs are written to stdout");
            None
        }
    };

    info!(
        tcp_port = server.state.config.port,
        http_port = server.state.config.http_port,
        "server started"
    );

    loop {
        tokio::select! {
            result = server.accept() => {
                if let Err(err) = result {
                    warn!(?err, "accept failed");
                }
            }
            _ = server.state.shutdown.notified() => {
                info!("shutdown requested");
                break;
            }
        }
    }

    for session in server.state.sessions.read().await.values() {
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
    server.state.plugin_manager.cleanup_all().await;
    if let Err(err) = server.state.extensions.persist().await {
        warn!(?err, "failed to persist extension data");
    }

    drop(console_handle);
    info!("server stopped");
    Ok(())
}

fn optional_channel<T>(
    enabled: bool,
) -> (
    Option<mpsc::UnboundedSender<T>>,
    Option<mpsc::UnboundedReceiver<T>>,
) {
    if enabled {
        let (tx, rx) = mpsc::unbounded_channel();
        (Some(tx), Some(rx))
    } else {
        (None, None)
    }
}

enum ConfigLoad {
    Loaded,
    Missing,
    Invalid(String),
}

impl ConfigLoad {
    fn report(&self, path: &str) {
        match self {
            Self::Loaded => info!(path, "configuration loaded"),
            Self::Missing => info!(path, "configuration file not found; using defaults"),
            Self::Invalid(error) => {
                warn!(path, %error, "failed to load configuration; using defaults")
            }
        }
    }
}

fn load_config(path: &str) -> (PlusConfig, ConfigLoad) {
    if !Path::new(path).exists() {
        return (PlusConfig::default(), ConfigLoad::Missing);
    }

    match PlusConfig::from_yaml(path) {
        Ok(config) => (config, ConfigLoad::Loaded),
        Err(error) => (
            PlusConfig::default(),
            ConfigLoad::Invalid(error.to_string()),
        ),
    }
}
