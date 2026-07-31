//! PlusServer::new() — full server initialization.

use crate::ban::BanManager;
use crate::extensions::ExtensionManager;
use crate::plugin::PluginManager;
use std::collections::HashMap;
use crate::plugin_http::{PluginHttpServer, SseHub};
use anyhow::Result;
use phira_mp_plus_server_api as api;
use serde_json::Value;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;
use tokio::net::TcpListener;
use tokio::sync::{mpsc, Mutex, Notify, RwLock, Semaphore};
use tracing::{error, info, warn};

use super::config::{IdMap, LiveConfig, PlusConfig, SafeMap};
use super::events::{spawn_event_subscribers, spawn_runtime_event_observer};
use super::state::{
    PlusServer, PlusServerState, CONNECTION_LIMITER_CLEANUP_SECS,
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

        // 初始化 Server Instance ID（用于区分 crash 后重启的 playtime session）
        let instance_id = crate::server_instance::init();
        info!("server instance ID: {instance_id}");

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
        let mut admin_ids: std::collections::HashSet<i32> =
            config.admin_phira_ids.iter().copied().collect();
        if admin_ids.is_empty() {
            if let Ok(raw) = std::fs::read_to_string("data/admin-phira-ids.json") {
                if let Ok(ids) = serde_json::from_str::<Vec<i32>>(&raw) {
                    admin_ids.extend(ids.into_iter().filter(|id| *id > 0));
                }
            }
        }
        let runtime = config.runtime.clone();
        // PostgreSQL is required infrastructure — the server refuses to start if
        // the connection or migration fails.
        let db_url = config.database_url.clone();
        let db_manager = crate::db::DbManager::new(&db_url).await.map_err(|e| {
            crate::error::AppError::Database(format!(
                "PostgreSQL init failed: {e}; PMP requires PostgreSQL and will not start without it."
            ))
        })?;
        // Register DB globally BEFORE PersistenceWorker spawns, so that
        // WAL replay and telemetry batcher can access the database from the start.
        let _ = crate::internal_hooks::DB.set(db_manager.clone());
        // Register this server instance in mp_server_instances so crash
        // recovery can accrue playtime only up to this instance's last known
        // alive time (P0-H).
        {
            let now = crate::db::now_ms();
            if let Err(e) = db_manager
                .register_server_instance(instance_id, now)
                .await
            {
                warn!("failed to register server instance: {e}");
            }
            // Heartbeat: keep last_alive_at fresh so a crash is not counted
            // as playtime beyond the last heartbeat.  After several consecutive
            // failures report degraded — the crash-recovery playtime accuracy
            // depends on this row staying fresh.
            let hb_db = db_manager.clone();
            let hb_id = instance_id.to_string();
            crate::supervisor_actor::spawn_named("server-instance-heartbeat", async move {
                let mut interval = tokio::time::interval(std::time::Duration::from_secs(30));
                interval.tick().await; // skip first immediate tick
                let mut consecutive_failures: u32 = 0;
                loop {
                    interval.tick().await;
                    let now = crate::db::now_ms();
                    match hb_db.heartbeat_server_instance(&hb_id, now).await {
                        Ok(()) => {
                            consecutive_failures = 0;
                        }
                        Err(e) => {
                            consecutive_failures += 1;
                            if consecutive_failures >= 3 {
                                crate::supervisor_actor::report_critical_failure(
                                    "server-instance-heartbeat",
                                    format!(
                                        "server instance heartbeat failed {consecutive_failures} \
                                         times consecutively: {e}"
                                    ),
                                )
                                .await;
                            } else {
                                tracing::warn!(error = %e, "server instance heartbeat failed");
                            }
                        }
                    }
                }
            });
        }
        let command_registry = Arc::new(crate::command_registry::runtime_registry());
        let event_bus = Arc::new(crate::event_bus::EventBus::new_with_trace(
            crate::runtime_diagnostics::EVENT_BUS_CHANNEL_CAPACITY,
            crate::runtime_diagnostics::EVENT_TRACE_WINDOW,
        ));
        spawn_runtime_event_observer(Arc::clone(&event_bus));
        let persistence_worker =
            crate::persistence::PersistenceWorker::spawn_with_journals(
                runtime.persistence_queue_capacity,
                runtime.persistence_dead_letter_path.clone(),
                runtime.persistence_wal_path.clone(),
            );
        let high_frequency_writer = Arc::new(
            crate::persistence::high_frequency::HighFrequencyWriter::spawn(
                runtime.high_frequency.clone(),
                Arc::new(db_manager.clone()),
            ),
        );
        let room_commands = Arc::new(crate::room_actor::RoomCommandGateway::new());
        // PMP25: Start PluginTcpActor for plugin TCP capabilities
        let (plugin_tcp_tx, plugin_tcp_rx) = mpsc::channel::<crate::plugin_tcp::PluginTcpCommand>(256);
        {
            let mut actor = crate::plugin_tcp::PluginTcpActor::new(plugin_tcp_rx);

            // Wire the TCP event callback to dispatch tcp:accept/receive/disconnect/error
            // events to the owning plugin via call_plugin_api.
            // The worker awaits this future directly — no inner tokio::spawn.
            use std::pin::Pin;
            use std::future::Future;
            let pm_for_tcp = Arc::clone(&plugin_manager);
            let tcp_event_cb: Arc<
                dyn Fn(String, serde_json::Value) -> Pin<Box<dyn Future<Output = ()> + Send>>
                    + Send
                    + Sync,
            > = Arc::new(move |event_type, payload| {
                let pm = Arc::clone(&pm_for_tcp);
                let payload_clone = payload.clone();
                Box::pin(async move {
                    // Extract plugin_id from the event payload to dispatch
                    // to the correct plugin.
                    if let Some(plugin_id) = payload_clone.get("plugin_id").and_then(|v| v.as_str()) {
                        let _ = pm
                            .call_plugin_api(
                                plugin_id,
                                "tcp:event",
                                vec![
                                    serde_json::json!(event_type),
                                    payload,
                                ],
                            )
                            .await;
                    }
                })
            });

            actor.set_event_callback(Arc::clone(&tcp_event_cb));
            plugin_manager.set_tcp_callback(tcp_event_cb);

            crate::supervisor_actor::spawn_critical("plugin-tcp-actor", async move {
                actor.run().await;
            });
        }
        plugin_manager.set_plugin_tcp_tx(plugin_tcp_tx.clone());
        let phira_client = Arc::new(crate::phira_client::PhiraRetryClient::new(
            runtime.phira_http.clone().into_policy(),
        )?);
        let events = Arc::new(SseHub::new());
        // Capture config fields before config is consumed by state
        let trusted_forwarded_http_port = config.trusted_forwarded_http_port;
        let http_bind_address = config.http_bind_address.clone();
        let max_pending_auth = config.max_pending_auth;
        let max_sessions = config.max_sessions;
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
            // Trusted proxy peers get a dedicated, higher-capacity rate limiter
            // so that traffic from HAProxy/LB is not throttled by the normal
            // client limit (e.g. 30/10s) before the forwarded IP is checked.
            proxy_connection_limiter: crate::rate_limiter::ConnectionRateLimiter::new(
                1000,
                10,
            ),
            round_store: Arc::new(crate::round_store::RoundStore::new()),
            user_room_history: SafeMap::default(),
            pre_auth_gate: Arc::new(Semaphore::new(max_pending_auth)),
            session_gate: Arc::new(Semaphore::new(max_sessions)),
            room_metadata_refresh_gate: Arc::new(Semaphore::new(
                ROOM_METADATA_REFRESH_CONCURRENCY,
            )),
            command_registry,
            event_bus,
            persistence_worker,
            high_frequency_writer,
            room_commands,
            plugin_tcp_tx: Some(plugin_tcp_tx),
            phira_client,
            admin_ids: RwLock::new(admin_ids),
            room_monitor: RwLock::new(None),
            game_monitors: SafeMap::default(),
            chart_duration_cache: RwLock::new(HashMap::new()),
            events,
            db_manager,
        });
        // Wire PersistenceWorker into ExtensionManager for persistence
        state
            .extensions
            .set_persistence_worker(&state.persistence_worker)
            .await;
        state.plugin_manager.start_event_dispatcher().await;
        spawn_event_subscribers(&state);

        // ── Startup state recovery ─────────────────────────────────────────
        // After DB connection is verified and migrations are applied, but before
        // plugins are loaded or network connections are accepted, recover any
        // state from the previous server session (unfinished rounds, etc.).
        //
        // The RoomCommandGateway mailbox MUST be started BEFORE recovery so
        // that restore_persistent_rooms → init_empty_room → room_mailbox_sender
        // has the self_ref/state_ref weak references available.  Previously
        // start_mailbox ran after recover_state, so persistent room
        // restoration silently failed (room_mailbox_sender = None).
        state.room_commands.start_mailbox(Arc::clone(&state), 1024);
        info!("startup recovery: running postgres state recovery");
        super::recovery::recover_state(&state, &state.db_manager).await?;
        info!("startup recovery: complete");
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
                trusted_forwarded_http_port,
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
                limiter_cleanup_state.proxy_connection_limiter.cleanup().await;
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

        // OpenUDS: start Unix Domain Socket API server if enabled
        if state.config.openuds.enabled {
            let uds_state = Arc::clone(&state);
            let uds_config = state.config.openuds.clone();
            crate::supervisor_actor::spawn_named("openuds-server", async move {
                crate::openuds::server::start(uds_state, &uds_config).await;
            });
        }

        Ok(Self { state, listener })
    }
}
