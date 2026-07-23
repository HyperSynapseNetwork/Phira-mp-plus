//! PlusServer::new() — full server initialization.
//!
//! Extracted from orig.rs.

use crate::ban::BanManager;
use crate::extensions::ExtensionManager;
use crate::plugin::PluginManager;
use crate::plugin_http::{PluginHttpServer, SseHub};
use anyhow::Result;
use phira_mp_plus_server_api as api;
use serde_json::Value;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;
use tokio::net::TcpListener;
use tokio::sync::{mpsc, Mutex, Notify, RwLock, Semaphore};
use tracing::{error, info, warn};

use super::benchmark::{
    load_benchmark_tokens, BenchRequest, BenchRequestKind,
};
use super::config::{IdMap, LiveConfig, PlusConfig, SafeMap};
use super::events::{spawn_event_subscribers, spawn_runtime_event_observer};
use super::state::{
    PlusServer, PlusServerState, BENCHMARK_QUEUE_CAPACITY, CONNECTION_LIMITER_CLEANUP_SECS,
    ROOM_METADATA_REFRESH_CONCURRENCY,
};

impl PlusServer {
    /// 创建新的 Phira-mp+ 服务器
    pub async fn new(config: PlusConfig) -> Result<Self> {
        // Windows 下 IPV6_V6ONLY 默认 true，[::] 不收 IPv4 连接。
        // Linux 下 IPV6_V6ONLY 默认 false，绑 [::] 即可收 IPv4。
        // 统一使用 0.0.0.0 确保两平台局域网 IP 都能连。
        let addr =
            std::net::SocketAddr::new(std::net::Ipv4Addr::UNSPECIFIED.into(), config.port);
        let listener = TcpListener::bind(addr).await?;
        info!("Phira-mp+ listening on tcp://{}", addr);

        // 初始化 Supervisor Actor（接受子任务注册与健康检查）
        crate::supervisor_actor::init();

        let (lost_con_tx, mut lost_con_rx) = mpsc::channel(1024);

        // 初始化扩展管理器
        let extensions = Arc::new(ExtensionManager::new(config.extensions_file.clone()));

        // 初始化插件管理器
        let plugin_manager = Arc::new(PluginManager::new(
            &config.plugins_dir,
            Arc::clone(&extensions),
            config.wasm_runtime.clone(),
        ));

        // 初始化黑名单管理器
        let ban_manager = Arc::new(BanManager::new(Arc::clone(&extensions)));

        let http_port = config.http_port;
        let rate_limit = config.connection_rate_limit;
        let rate_window = config.connection_rate_window;
        let retention_days = config.round_data_retention_days;
        let bench_tokens = load_benchmark_tokens(&config);
        let mut admin_ids: std::collections::HashSet<i32> =
            config.admin_phira_ids.iter().copied().collect();
        if admin_ids.is_empty() {
            if let Ok(raw) = std::fs::read_to_string("data/admin-phira-ids.json") {
                if let Ok(ids) = serde_json::from_str::<Vec<i32>>(&raw) {
                    admin_ids.extend(ids.into_iter().filter(|id| *id > 0));
                }
            }
        }
        let (bench_tx, bench_rx) =
            tokio::sync::mpsc::channel::<BenchRequest>(BENCHMARK_QUEUE_CAPACITY);

        let runtime = config.runtime.clone();
        // PostgreSQL is required infrastructure — the server refuses to start if
        // the connection or migration fails.
        let db_url = config.database_url.clone().unwrap_or_default();
        let db_manager = crate::db::DbManager::new(&db_url).await.map_err(|e| {
            crate::error::AppError::Database(format!(
                "PostgreSQL init failed: {e}; PMP requires PostgreSQL and will not start without it."
            ))
        })?;
        // Register DB globally BEFORE PersistenceWorker spawns, so that
        // WAL replay and telemetry batcher can access the database from the start.
        let _ = crate::internal_hooks::DB.set(db_manager.clone());
        let command_registry = Arc::new(crate::command_registry::runtime_registry());
        let event_bus = Arc::new(crate::event_bus::EventBus::new_with_trace(
            crate::runtime_diagnostics::EVENT_BUS_CHANNEL_CAPACITY,
            crate::runtime_diagnostics::EVENT_TRACE_WINDOW,
        ));
        spawn_runtime_event_observer(Arc::clone(&event_bus));
        let benchmark_reports =
            Arc::new(crate::benchmark_snapshot::BenchmarkReportStore::new(
                crate::runtime_diagnostics::BENCHMARK_REPORT_HISTORY,
            ));
        let simulation = Arc::new(crate::simulation::SimulationManager::new());
        let persistence_worker =
            crate::persistence_worker::PersistenceWorker::spawn_with_policy_and_journals(
                runtime.persistence_queue_capacity,
                runtime.telemetry.clone(),
                runtime.persistence_dead_letter_path.clone(),
                runtime.persistence_wal_path.clone(),
            );
        let high_frequency_writer = Arc::new(
            crate::persistence::high_frequency::HighFrequencyWriter::spawn(
                crate::persistence::high_frequency::HighFrequencyConfig::default(),
                Arc::new(db_manager.clone()),
            ),
        );
        let actor_runtime = Arc::new(crate::actor_runtime::ActorRuntime::new_blueprint());
        let room_commands = Arc::new(crate::room_actor::RoomCommandGateway::new());
        let phira_client = Arc::new(crate::phira_client::PhiraRetryClient::new(
            runtime.phira_http.clone().into_policy(),
        )?);
        let events = Arc::new(SseHub::new());
        // Capture config fields before config is consumed by state
        let proxy_protocol_port = config.proxy_protocol_port;
        let http_bind_address = config.http_bind_address.clone();
        let idle_config = config.idle.clone();
        let max_pending_auth = config.max_pending_auth;
        let max_sessions = config.max_sessions;
        let room_monitor_key = phira_mp_common::generate_secret_key("room_monitor", 64)?;
        let live_config = Arc::new(RwLock::new(LiveConfig::from_full(&config)));
        let state = Arc::new(PlusServerState {
            config,
            live_config,
            sessions: IdMap::default(),
            users: SafeMap::default(),
            user_registration_gate: Mutex::new(()),
            rooms: SafeMap::default(),
            lost_con_tx,
            plugin_manager,
            extensions,
            ban_manager,
            shutdown: Notify::new(),
            shutting_down: AtomicBool::new(false),
            connection_limiter: crate::rate_limiter::ConnectionRateLimiter::new(
                rate_limit,
                rate_window,
            ),
            round_store: Arc::new(crate::round_store::RoundStore::new(
                "data",
                retention_days,
            )),
            user_room_history: SafeMap::default(),
            bench_tx: bench_tx.clone(),
            pre_auth_gate: Arc::new(Semaphore::new(max_pending_auth)),
            session_gate: Arc::new(Semaphore::new(max_sessions)),
            room_metadata_refresh_gate: Arc::new(Semaphore::new(
                ROOM_METADATA_REFRESH_CONCURRENCY,
            )),
            command_registry,
            event_bus,
            benchmark_reports,
            simulation,
            persistence_worker,
            high_frequency_writer,
            actor_runtime,
            room_commands,
            phira_client,
            bench_tokens: RwLock::new(bench_tokens),
            admin_ids: RwLock::new(admin_ids),
            room_monitor_key,
            room_monitor: RwLock::new(None),
            game_monitors: SafeMap::default(),
            events,
            idle_monitor: crate::idle::IdleMonitor::new(idle_config),
            db_manager,
        });
        // Wire PersistenceWorker into ExtensionManager for mirrored writes
        state
            .extensions
            .set_persistence_worker(&state.persistence_worker)
            .await;
        state.plugin_manager.start_event_dispatcher().await;
        spawn_event_subscribers(&state);
        state.room_commands.start_mailbox(Arc::clone(&state), 1024);
        state
            .actor_runtime
            .mark_status(
                "room-actor",
                crate::actor_runtime::ActorBoundaryStatus::WriteRouted,
                "nine room management commands cross a bounded per-room mailbox; uncertain post-enqueue outcomes are not replayed; full RoomState ownership remains pending",
            )
            .await;
        // 启动 IdleMonitor 主循环（定期检查空闲条件，挂起/恢复重服务）
        state.idle_monitor.start_loop(&state);
        let bench_state = Arc::clone(&state);
        crate::supervisor_actor::spawn_named("benchmark-worker", async move {
            let mut bench_rx = bench_rx;
            while let Some(request) = bench_rx.recv().await {
                let bs = Arc::clone(&bench_state);
                let output = match request.kind {
                    BenchRequestKind::Real {
                        duration_secs,
                        target_rooms,
                    } => bs.run_benchmark_network(duration_secs, target_rooms).await,
                    BenchRequestKind::Hybrid(config) => bs.run_benchmark_hybrid(config).await,
                };
                let _ = request.result_tx.send(output);
            }
        });

        let lost_con_state = Arc::clone(&state);
        crate::supervisor_actor::spawn_critical("lost-connection-worker", async move {
            while let Some(id) = lost_con_rx.recv().await {
                warn!("lost connection with {id}");
                let session_opt = lost_con_state.sessions.write().await.remove(&id);
                if let Some(session) = session_opt {
                    session.stream.close();
                    let user_ref = {
                        let session_guard = session.user.session.read().await;
                        session_guard
                            .as_ref()
                            .is_some_and(|it| it.ptr_eq(&Arc::downgrade(&session)))
                    };
                    if user_ref {
                        Arc::clone(&session.user).dangle(id).await;
                    }
                }
            }
        });

        // 初始化黑名单扩展字段
        state.ban_manager.register_fields().await;

        // 设置发送聊天消息能力（供插件使用）
        let s = Arc::clone(&state);
        state
            .plugin_manager
            .set_send_chat(Arc::new(move |uid, msg| {
                let s = Arc::clone(&s);
                tokio::spawn(async move {
                    let cmd =
                        phira_mp_common::ServerCommand::Message(phira_mp_common::Message::Chat {
                            user: 0,
                            content: msg,
                        });

                    // WASM `send.to_all` uses uid = 0.  Older code only looked up a
                    // concrete user id, so `send.to_all` could silently send to no
                    // one.  Clone user Arcs before awaiting to avoid holding the
                    // global users lock across network sends.
                    if uid == 0 {
                        let recipients = {
                            let users = s.users.read().await;
                            users.values().cloned().collect::<Vec<_>>()
                        };
                        for user in recipients {
                            user.try_send(cmd.clone()).await;
                        }
                        return;
                    }

                    let user = {
                        let users = s.users.read().await;
                        users.get(&uid).cloned()
                    };
                    if let Some(user) = user {
                        user.try_send(cmd).await;
                    }
                });
            }))
            .await;

        // Legacy/default state query. WIT components receive a stricter per-plugin wrapper.
        let state_query_all = api::ServerStateQuery::new({
            let s = Arc::clone(&state);
            move |method: &str, args: &[Value]| -> Result<Value, String> {
                super::query::server_state_query_for_host(&s, method, args)
            }
        });
        state
            .plugin_manager
            .set_default_state(state_query_all)
            .await;

        // http_port>0 时才启动 HTTP 服务
        let http_server = if http_port > 0 {
            let srv = Arc::new(PluginHttpServer::new(
                http_port,
                &http_bind_address,
                proxy_protocol_port,
                Arc::clone(&state.events),
            ));
            let http_handle =
                api::HttpHandle::new(crate::plugin_http::HttpHandleBridge(Arc::clone(&srv)));
            state.plugin_manager.set_http_handle(http_handle).await;
            Some(srv)
        } else {
            None
        };
        // 设置 WIT 组件模型所需的服务端状态引用
        state
            .plugin_manager
            .set_server_state(Arc::clone(&state))
            .await;
        // 设置命令注册表的 Room ID 补全引用
        state.command_registry.install_room_completer(&state);

        // Session actor mailbox 在每连接认证后独立初始化，不再需要全局 init。

        // 加载插件
        let plugin_count = state.plugin_manager.load_plugins().await.unwrap_or(0);
        info!("loaded {} plugin(s)", plugin_count);

        // 初始化内置功能（欢迎语/追踪/排行等）
        let http_server_ref = http_server.as_ref().map(|s| Arc::clone(s));
        crate::internal_hooks::init_internal_hooks(
            &state,
            &http_server_ref,
            &state.plugin_manager,
        )
        .await;

        // 启动中央 HTTP 服务器（所有路由已注册完毕）
        // The `start()` method binds the listener; failure is reported to Supervisor.
        if let Some(srv) = http_server {
            let http_state = Arc::clone(&state);
            crate::supervisor_actor::spawn_named("http-server", async move {
                if let Err(err) = srv.start(http_state).await {
                    error!("HTTP server failed to start: {err}");
                    crate::supervisor_actor::report_critical_failure("http-server", err).await;
                }
            });
        }

        // 定期持久化 auth 缓存（避免每次认证都写盘）
        let persist_state = Arc::clone(&state);
        crate::supervisor_actor::spawn_named("auth-cache-persist", async move {
            loop {
                tokio::time::sleep(std::time::Duration::from_secs(60)).await;
                if let Err(e) = persist_state.extensions.persist().await {
                    warn!("auth cache persist: {e}");
                }
            }
        });

        let limiter_cleanup_state = Arc::clone(&state);
        crate::supervisor_actor::spawn_named("rate-limiter-cleanup", async move {
            loop {
                tokio::time::sleep(std::time::Duration::from_secs(
                    CONNECTION_LIMITER_CLEANUP_SECS,
                ))
                .await;
                limiter_cleanup_state.connection_limiter.cleanup().await;
            }
        });

        // 轮次文件与统一 PostgreSQL 持久化定期清理（每小时检查一次）
        let telemetry_retention_days = state
            .config
            .touch_judge_retention_days
            .unwrap_or(state.config.persistence_retention_days);
        if retention_days > 0
            || state.config.persistence_retention_days > 0
            || telemetry_retention_days > 0
        {
            let cleanup_state = Arc::clone(&state);
            crate::supervisor_actor::spawn_named("retention-cleanup", async move {
                loop {
                    tokio::time::sleep(std::time::Duration::from_secs(3600)).await;
                    cleanup_state.round_store.cleanup_expired().await;
                    if let Some(db) = crate::internal_hooks::DB.get() {
                        let telemetry_retention_days = cleanup_state
                            .config
                            .touch_judge_retention_days
                            .unwrap_or(cleanup_state.config.persistence_retention_days);
                        db.cleanup_expired(
                            cleanup_state.config.persistence_retention_days,
                            telemetry_retention_days,
                        )
                        .await;
                    }
                }
            });
        }

        Ok(Self { state, listener })
    }
}
