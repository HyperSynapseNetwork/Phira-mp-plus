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
use std::time::{Duration, Instant};
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
    #[arg(short, long, help = "TCP listen port (overrides YAML only when provided)")]
    port: Option<u16>,

    #[arg(
        short = 'd',
        long = "plugins-dir",
        help = "WASM plugin directory (overrides YAML only when provided)"
    )]
    plugins_dir: Option<String>,

    #[arg(
        short = 'e',
        long = "ext-file",
        help = "Extension data file (overrides YAML only when provided)"
    )]
    extensions_file: Option<String>,

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
        help = "HTTP and SSE listen port (overrides YAML only when provided)"
    )]
    http_port: Option<u16>,

    #[arg(
        long = "proxy-port",
        help = "Forwarded-header compatibility HTTP port (0 = disabled; overrides YAML only when provided)"
    )]
    proxy_protocol_port: Option<u16>,

    #[arg(long = "no-cli", help = "Disable the interactive management console")]
    no_cli: bool,

    #[arg(
        short = 'c',
        long = "config",
        default_value = "server_config.yml",
        help = "YAML configuration file"
    )]
    config: String,
}

/// 在 Linux 下提前配置 jemalloc 以更快地归还空闲内存给 OS。
/// 可通过环境变量 `MALLOC_CONF` 覆盖（例如 `MALLOC_CONF=background_thread:false`）。
fn configure_jemalloc() {
    #[cfg(target_os = "linux")]
    {
        // 仅在用户未显式设置 MALLOC_CONF 时使用优化的默认值
        if std::env::var("MALLOC_CONF").is_err() {
            std::env::set_var(
                "MALLOC_CONF",
                "background_thread:true,dirty_decay_ms:5000,muzzy_decay_ms:5000",
            );
        }
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    configure_jemalloc();
    let terminal = TerminalProfile::detect();
    terminal.apply_environment();
    let args = Args::parse();

    let (mut base_config, config_load) = load_config(&args.config)?;
    base_config.config_path = args.config.clone();
    let mut config = base_config.merge_cli(PlusConfigCli {
        port: args.port,
        http_port: args.http_port,
        proxy_protocol_port: args.proxy_protocol_port,
        monitors: args.monitors.clone(),
        plugins_dir: args.plugins_dir.clone(),
        extensions_file: args.extensions_file.clone(),
        disable_cli: args.no_cli,
    });
    config.normalize()?;
    std::fs::create_dir_all("data")?;
    std::fs::create_dir_all(&config.plugins_dir)?;
    config.validate()?;

    let cli_enabled = config.cli_enabled;
    let (cmd_tx, cmd_rx) = optional_channel(cli_enabled);
    let (out_tx, out_rx) = optional_channel(cli_enabled);
    let (log_tx, log_rx) = optional_channel(cli_enabled);
    let _log_guard = phira_mp_plus_server::logging::init(&args.log_file, log_tx)?;
    config_load.report(&args.config);

    let server = PlusServer::new(config).await?;

    if let (Some(cmd_rx), Some(out_tx)) = (cmd_rx, out_tx) {
        let state = Arc::clone(&server.state);
        phira_mp_plus_server::supervisor_actor::spawn_named("cli-handler", async move {
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

    server
        .state
        .shutting_down
        .store(true, std::sync::atomic::Ordering::Release);
    // Prevent new permit acquisition immediately. Existing pre-auth tasks check
    // `shutting_down` before publishing a Session into the authoritative map.
    server.state.pre_auth_gate.close();
    server.state.session_gate.close();

    let shutdown_timeout = Duration::from_secs(
        server.state.config.graceful_shutdown_timeout_secs.max(1),
    );
    let shutdown_deadline = Instant::now() + shutdown_timeout;
    let remaining = || shutdown_deadline.saturating_duration_since(Instant::now());

    // Remove transports from the authoritative session map first. Their later
    // socket callbacks then find no session and cannot repeat shutdown effects.
    let sessions = {
        let mut sessions = server.state.sessions.write().await;
        std::mem::take(&mut *sessions)
            .into_values()
            .collect::<Vec<_>>()
    };
    let mut disconnect_users = std::collections::HashMap::<i32, String>::new();
    for session in &sessions {
        *session.user.session.write().await = None;
        if session.user.id >= 0 {
            disconnect_users
                .entry(session.user.id)
                .or_insert_with(|| session.user.name.clone());
        }
        session
            .try_send(phira_mp_common::ServerCommand::Message(
                phira_mp_common::Message::Chat {
                    user: 0,
                    content: "服务器正在关闭...".to_string(),
                },
            ))
            .await;
        session.stream.close();
    }

    // Emit one canonical disconnect and one offline write per user. This avoids
    // the ordinary reconnect grace period leaving stale online rows on process exit.
    let lifecycle_budget = remaining();
    if !lifecycle_budget.is_zero() {
        let lifecycle = async {
            for (user_id, user_name) in disconnect_users {
                server
                    .state
                    .publish_user_disconnected(user_id, user_name.clone())
                    .await;
                let _ = server
                    .state
                    .persistence_worker
                    .enqueue(
                        phira_mp_plus_server::persistence::message::PersistenceEvent::UserDisconnect {
                            user_id,
                            user_name,
                        },
                    )
                    .await;
                let _ = server
                    .state
                    .persistence_worker
                    .enqueue(
                        phira_mp_plus_server::persistence::message::PersistenceEvent::UserOffline {
                            user_id,
                        },
                    )
                    .await;
            }
        };
        if tokio::time::timeout(lifecycle_budget, lifecycle).await.is_err() {
            warn!("session lifecycle shutdown exceeded the shared deadline");
        }
    }

    let budget = remaining();
    if !budget.is_zero() {
        if let Err(error) = server.state.plugin_manager.flush_events(budget).await {
            warn!(%error, "plugin event flush failed during shutdown");
        }
    }
    let budget = remaining();
    if !budget.is_zero() {
        if let Err(error) = server
            .state
            .plugin_manager
            .shutdown_event_dispatcher(budget)
            .await
        {
            warn!(%error, "plugin event dispatcher shutdown failed");
        }
    }

    let budget = remaining();
    if !budget.is_zero() {
        if tokio::time::timeout(budget, server.state.plugin_manager.cleanup_all())
            .await
            .is_err()
        {
            warn!("plugin cleanup exceeded the shared shutdown deadline");
        }
    }
    let budget = remaining();
    if !budget.is_zero() {
        match tokio::time::timeout(budget, server.state.extensions.persist()).await {
            Ok(Ok(())) => {}
            Ok(Err(err)) => warn!(?err, "failed to persist extension data"),
            Err(_) => warn!("extension persistence exceeded the shared shutdown deadline"),
        }
    }

    let budget = remaining();
    if !budget.is_zero() {
        if let Err(error) = server.state.persistence_worker.flush(budget).await {
            warn!(%error, "persistence flush failed during shutdown");
        }
    }
    let budget = remaining();
    if !budget.is_zero() {
        if let Err(error) = server.state.persistence_worker.shutdown(budget).await {
            warn!(%error, "persistence shutdown failed");
        }
    }

    let budget = remaining();
    let stopped = if budget.is_zero() {
        0
    } else {
        phira_mp_plus_server::supervisor_actor::shutdown_all(budget).await
    };
    info!(stopped_tasks = stopped, "background tasks stopped");

    drop(console_handle);
    info!("server stopped");
    Ok(())
}

fn optional_channel<T>(
    enabled: bool,
) -> (
    Option<mpsc::Sender<T>>,
    Option<mpsc::Receiver<T>>,
) {
    if enabled {
        let (tx, rx) = mpsc::channel(1024);
        (Some(tx), Some(rx))
    } else {
        (None, None)
    }
}

enum ConfigLoad {
    Loaded,
    Missing,
}

impl ConfigLoad {
    fn report(&self, path: &str) {
        match self {
            Self::Loaded => info!(path, "configuration loaded"),
            Self::Missing => info!(path, "configuration file not found; using defaults"),
        }
    }
}

fn load_config(path: &str) -> Result<(PlusConfig, ConfigLoad)> {
    if !Path::new(path).exists() {
        return Ok((PlusConfig::default(), ConfigLoad::Missing));
    }

    let config = PlusConfig::from_yaml(path)?;
    Ok((config, ConfigLoad::Loaded))
}
