//! Phira-mp+ WASM plugin manager.
//!
//! Guest execution is moved off Tokio worker threads and serialized per plugin.
//! The async plugin-list lock is never held while guest code is running.

use crate::extensions::ExtensionManager;
use phira_mp_plus_server_api as api;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
#[cfg(feature = "plugin-system")]
use std::collections::HashSet;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, RwLock as StdRwLock};
use tokio::sync::{mpsc, oneshot, Mutex as AsyncMutex, RwLock};
use tracing::{info, warn};

pub use api::{HttpHandle, JudgeEventItem, PluginEvent, PluginInfo, TouchEventPoint};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WasmRuntimeConfig {
    #[serde(default = "default_wasm_memory_mb")]
    pub max_memory_mb: usize,
    /// Fuel added before each guest call. PMP configuration rejects zero.
    #[serde(default = "default_wasm_fuel")]
    pub fuel_per_call: u64,
    #[serde(default = "default_wasm_stack_bytes")]
    pub max_stack_bytes: usize,
    #[serde(default = "default_wasm_http_timeout")]
    pub http_timeout_secs: u64,
    #[serde(default = "default_wasm_http_response_bytes")]
    pub max_http_response_bytes: usize,
    #[serde(default = "default_wasm_file_bytes")]
    pub max_file_bytes: usize,
    /// Disabled by default to reduce SSRF exposure.
    #[serde(default)]
    pub allow_private_network: bool,
    /// Maximum number of plugins executed in parallel for one ordered event.
    #[serde(default = "default_wasm_event_concurrency")]
    pub max_event_concurrency: usize,
    /// Bounded queue for reliable low/medium-frequency plugin events.
    #[serde(default = "default_wasm_event_queue_capacity")]
    pub event_queue_capacity: usize,
    /// Wall-clock observation deadline around plugin init/event/API tasks. Fuel
    /// limits guest CPU. A timed-out blocking host call cannot be force-killed
    /// in-process, so the per-plugin execution gate remains occupied until it exits.
    #[serde(default = "default_wasm_call_timeout_ms")]
    pub call_timeout_ms: u64,
}

const fn default_wasm_memory_mb() -> usize {
    64
}
const fn default_wasm_fuel() -> u64 {
    10_000_000
}
const fn default_wasm_stack_bytes() -> usize {
    2 * 1024 * 1024
}
const fn default_wasm_http_timeout() -> u64 {
    10
}
const fn default_wasm_http_response_bytes() -> usize {
    2 * 1024 * 1024
}
const fn default_wasm_file_bytes() -> usize {
    4 * 1024 * 1024
}
const fn default_wasm_event_concurrency() -> usize {
    8
}
const fn default_wasm_event_queue_capacity() -> usize {
    2048
}
const fn default_wasm_call_timeout_ms() -> u64 {
    2_000
}

impl Default for WasmRuntimeConfig {
    fn default() -> Self {
        Self {
            max_memory_mb: default_wasm_memory_mb(),
            fuel_per_call: default_wasm_fuel(),
            max_stack_bytes: default_wasm_stack_bytes(),
            http_timeout_secs: default_wasm_http_timeout(),
            max_http_response_bytes: default_wasm_http_response_bytes(),
            max_file_bytes: default_wasm_file_bytes(),
            allow_private_network: false,
            max_event_concurrency: default_wasm_event_concurrency(),
            event_queue_capacity: default_wasm_event_queue_capacity(),
            call_timeout_ms: default_wasm_call_timeout_ms(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum PluginState {
    Loaded,
    Enabled,
    Disabled,
    Error(String),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PluginMeta {
    pub info: PluginInfo,
    pub path: String,
    pub state: PluginState,
    pub enabled: bool,
}

pub trait PluginHost: Send {
    fn meta(&self) -> &PluginMeta;
    fn meta_mut(&mut self) -> &mut PluginMeta;
    fn init(&mut self) -> Result<(), String>;
    fn cleanup(&mut self);
    fn trigger_event(&mut self, event: &PluginEvent) -> Result<Vec<String>, String>;
    fn call_api(
        &mut self,
        _method: &str,
        _args: &[serde_json::Value],
    ) -> Result<serde_json::Value, String> {
        Err("plugin does not export phira_on_api".to_string())
    }
}

struct PluginSlotInner {
    host: Mutex<Box<dyn PluginHost>>,
    meta_cache: StdRwLock<PluginMeta>,
    executing: AtomicBool,
    quarantined: AtomicBool,
}

type PluginSlot = Arc<PluginSlotInner>;

struct PluginExecutionPermit<'a> {
    slot: &'a PluginSlotInner,
}

impl Drop for PluginExecutionPermit<'_> {
    fn drop(&mut self) {
        self.slot.executing.store(false, Ordering::Release);
    }
}

impl PluginSlotInner {
    fn new(host: Box<dyn PluginHost>) -> PluginSlot {
        let meta = host.meta().clone();
        Arc::new(Self {
            host: Mutex::new(host),
            meta_cache: StdRwLock::new(meta),
            executing: AtomicBool::new(false),
            quarantined: AtomicBool::new(false),
        })
    }

    fn lock(&self) -> std::sync::LockResult<std::sync::MutexGuard<'_, Box<dyn PluginHost>>> {
        self.host.lock()
    }

    fn try_execution(&self) -> Result<PluginExecutionPermit<'_>, &'static str> {
        if self.quarantined.load(Ordering::Acquire) {
            return Err("plugin is quarantined after a timed-out call");
        }
        self.executing
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .map_err(|_| "plugin already has an in-flight call")?;
        Ok(PluginExecutionPermit { slot: self })
    }

    fn wait_for_execution_until(
        &self,
        deadline: std::time::Instant,
    ) -> Result<PluginExecutionPermit<'_>, &'static str> {
        loop {
            match self.try_execution() {
                Ok(permit) => return Ok(permit),
                Err("plugin already has an in-flight call") => {
                    if std::time::Instant::now() >= deadline {
                        return Err("plugin execution slot wait timed out");
                    }
                    std::thread::sleep(std::time::Duration::from_millis(2));
                }
                Err(reason) => return Err(reason),
            }
        }
    }

    fn quarantine(&self) {
        self.quarantined.store(true, Ordering::Release);
    }

    fn clear_quarantine(&self) {
        self.quarantined.store(false, Ordering::Release);
    }

    fn is_quarantined(&self) -> bool {
        self.quarantined.load(Ordering::Acquire)
    }

    fn update_meta(&self, meta: PluginMeta) {
        *self.meta_cache.write().unwrap_or_else(|e| e.into_inner()) = meta;
    }

    fn meta_snapshot(&self) -> PluginMeta {
        let mut meta = self
            .meta_cache
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        if self.is_quarantined() {
            meta.enabled = false;
            meta.state =
                PluginState::Error("quarantined after a timed-out in-process call".to_string());
        }
        meta
    }

    fn matches(&self, id: &str) -> bool {
        let meta = self.meta_cache.read().unwrap_or_else(|e| e.into_inner());
        plugin_matches(&meta, id)
    }
}

#[cfg(feature = "plugin-system")]
pub mod wasm {
    use super::*;
    use crate::wasm_host;

    pub struct WasmPlugin {
        meta: PluginMeta,
        services: Arc<wasm_host::WasmPluginServices>,
        runtime: WasmRuntimeConfig,
        component_instance: Option<wasm_host::WitPluginComponent>,
    }

    impl WasmPlugin {
        pub fn new(
            path: &str,
            info: PluginInfo,
            services: Arc<wasm_host::WasmPluginServices>,
            runtime: WasmRuntimeConfig,
        ) -> Self {
            Self {
                meta: PluginMeta {
                    info,
                    path: path.to_string(),
                    state: PluginState::Loaded,
                    enabled: true,
                },
                services,
                runtime,
                component_instance: None,
            }
        }
    }

    impl WasmPlugin {
        /// Initialize as a WIT component. JSON bridge (init_module) has been removed.
        fn init_component(&mut self, bytes: &[u8]) -> Result<(), String> {
            let plugin_name = std::path::Path::new(&self.meta.path)
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or("unknown")
                .to_string();
            let mut component = wasm_host::WitPluginComponent::new(
                bytes,
                plugin_name,
                Arc::clone(&self.services),
                self.runtime.clone(),
            )?;
            component.call_init()?;
            self.meta.info = component.info.clone();
            self.component_instance = Some(component);
            self.meta.state = PluginState::Enabled;
            info!("WIT component '{}' loaded", self.meta.info.name);
            Ok(())
        }
    }

    impl PluginHost for WasmPlugin {
        fn meta(&self) -> &PluginMeta {
            &self.meta
        }
        fn meta_mut(&mut self) -> &mut PluginMeta {
            &mut self.meta
        }

        fn init(&mut self) -> Result<(), String> {
            let bytes = std::fs::read(&self.meta.path)
                .map_err(|e| format!("read WASM '{}': {e}", self.meta.path))?;
            self.init_component(&bytes)
        }

        fn cleanup(&mut self) {
            if let Some(component) = self.component_instance.as_mut() {
                component.call_cleanup();
            }
            self.component_instance = None;
            self.meta.state = PluginState::Disabled;
        }

        fn trigger_event(&mut self, event: &PluginEvent) -> Result<Vec<String>, String> {
            if !self.meta.enabled {
                return Ok(Vec::new());
            }
            // Component path (WIT, requires wit-bindgen)
            #[cfg(feature = "wit-bindgen")]
            if let Some(component) = self.component_instance.as_mut() {
                let code = component.call_on_event(event)?;
                if code < 0 {
                    return Err(format!("component returned event error {code}"));
                }
                return Ok(Vec::new());
            }
            #[cfg(not(feature = "wit-bindgen"))]
            let _ = event;
            Err("plugin is not initialized".to_string())
        }

        fn call_api(
            &mut self,
            method: &str,
            args: &[serde_json::Value],
        ) -> Result<serde_json::Value, String> {
            if !self.meta.enabled {
                return Err("plugin is disabled".to_string());
            }
            if let Some(component) = self.component_instance.as_mut() {
                return component.call_api(method, args);
            }
            Err("plugin is not initialized".to_string())
        }
    }
}

enum PluginDispatchMessage {
    Event(PluginEvent),
    Flush(oneshot::Sender<()>),
    Shutdown(oneshot::Sender<()>),
}

pub struct PluginManager {
    plugins: Arc<RwLock<Vec<PluginSlot>>>,
    cli_commands: Arc<Mutex<HashMap<String, CliCommand>>>,
    api_handlers: Arc<Mutex<HashMap<String, api::PluginApiHandler>>>,
    plugins_dir: String,
    #[allow(dead_code)]
    runtime: WasmRuntimeConfig,
    event_tx: mpsc::Sender<PluginDispatchMessage>,
    event_rx: AsyncMutex<Option<mpsc::Receiver<PluginDispatchMessage>>>,
    event_send_gate: AsyncMutex<()>,
    dispatcher_started: AtomicBool,
    dispatcher_closed: AtomicBool,
    http_handle: Arc<RwLock<Option<api::HttpHandle>>>,
    /// Idle hint used only by best-effort high-frequency dispatch. Reliable
    /// queued events are still drained so lifecycle callbacks are not lost.
    suspended: AtomicBool,
    #[cfg(feature = "plugin-system")]
    wasm_services: Arc<crate::wasm_host::WasmPluginServices>,
}

impl PluginManager {
    pub fn new(
        plugins_dir: &str,
        extensions: Arc<ExtensionManager>,
        runtime: WasmRuntimeConfig,
    ) -> Self {
        let cli_commands = Arc::new(Mutex::new(HashMap::new()));
        let api_handlers = Arc::new(Mutex::new(HashMap::new()));
        let http_handle = Arc::new(RwLock::new(None));
        let (event_tx, event_rx) = mpsc::channel(runtime.event_queue_capacity.max(16));

        #[cfg(feature = "plugin-system")]
        let wasm_services = Arc::new(crate::wasm_host::WasmPluginServices::new(
            extensions,
            Arc::clone(&cli_commands),
            Arc::clone(&api_handlers),
        ));
        #[cfg(not(feature = "plugin-system"))]
        let _ = extensions;

        Self {
            plugins: Arc::new(RwLock::new(Vec::new())),
            cli_commands,
            api_handlers,
            plugins_dir: plugins_dir.to_string(),
            event_tx,
            event_rx: AsyncMutex::new(Some(event_rx)),
            event_send_gate: AsyncMutex::new(()),
            dispatcher_started: AtomicBool::new(false),
            dispatcher_closed: AtomicBool::new(false),
            runtime,
            http_handle,
            suspended: AtomicBool::new(false),
            #[cfg(feature = "plugin-system")]
            wasm_services,
        }
    }

    /// Start the bounded plugin-event dispatcher exactly once.
    ///
    /// Events are consumed in queue order. Up to `max_event_concurrency`
    /// plugins execute in parallel for one event, but lifecycle events themselves
    /// never overlap merely to increase throughput.
    pub async fn start_event_dispatcher(self: &Arc<Self>) {
        if self.dispatcher_started.swap(true, Ordering::AcqRel) {
            return;
        }
        let Some(mut rx) = self.event_rx.lock().await.take() else {
            return;
        };
        let manager = Arc::clone(self);
        crate::supervisor_actor::spawn_critical("plugin-event-dispatcher", async move {
            while let Some(message) = rx.recv().await {
                match message {
                    PluginDispatchMessage::Event(event) => {
                        let _ = manager.trigger(&event).await;
                    }
                    PluginDispatchMessage::Flush(reply) => {
                        // All earlier events have completed because the consumer
                        // processes messages serially.
                        let _ = reply.send(());
                    }
                    PluginDispatchMessage::Shutdown(reply) => {
                        let _ = reply.send(());
                        break;
                    }
                }
            }
        });
    }

    /// Queue a plugin event. Before the dispatcher starts (mainly unit tests),
    /// execute synchronously so callers never enqueue into an unconsumed queue.
    pub async fn dispatch_event(&self, event: PluginEvent) {
        if !self.dispatcher_started.load(Ordering::Acquire) {
            let _ = self.trigger(&event).await;
            return;
        }
        let _send_guard = self.event_send_gate.lock().await;
        if self.dispatcher_closed.load(Ordering::Acquire) {
            warn!(
                kind = event.kind(),
                "plugin event rejected after dispatcher shutdown"
            );
            return;
        }
        if let Err(error) = self
            .event_tx
            .send(PluginDispatchMessage::Event(event))
            .await
        {
            if let PluginDispatchMessage::Event(event) = error.0 {
                warn!(kind = event.kind(), "plugin event dispatcher is closed");
            }
        }
    }

    /// Best-effort non-blocking path for high-frequency telemetry.
    pub fn try_dispatch_event(&self, event: PluginEvent) -> bool {
        if !self.dispatcher_started.load(Ordering::Acquire)
            || self.dispatcher_closed.load(Ordering::Acquire)
            || self.is_suspended()
        {
            return false;
        }
        let Ok(_send_guard) = self.event_send_gate.try_lock() else {
            return false;
        };
        if self.dispatcher_closed.load(Ordering::Acquire) {
            return false;
        }
        self.event_tx
            .try_send(PluginDispatchMessage::Event(event))
            .is_ok()
    }

    pub async fn flush_events(&self, timeout: std::time::Duration) -> Result<(), String> {
        if !self.dispatcher_started.load(Ordering::Acquire) {
            return Ok(());
        }
        let (reply, rx) = oneshot::channel();
        {
            let _send_guard = self.event_send_gate.lock().await;
            if self.dispatcher_closed.load(Ordering::Acquire) {
                return Err("plugin event dispatcher is shutting down".to_string());
            }
            self.event_tx
                .send(PluginDispatchMessage::Flush(reply))
                .await
                .map_err(|_| "plugin event dispatcher is closed".to_string())?;
        }
        tokio::time::timeout(timeout, rx)
            .await
            .map_err(|_| "plugin event flush timed out".to_string())?
            .map_err(|_| "plugin event flush acknowledgement dropped".to_string())
    }

    pub async fn shutdown_event_dispatcher(
        &self,
        timeout: std::time::Duration,
    ) -> Result<(), String> {
        if !self.dispatcher_started.load(Ordering::Acquire) {
            return Ok(());
        }
        let (reply, rx) = oneshot::channel();
        {
            let _send_guard = self.event_send_gate.lock().await;
            if self.dispatcher_closed.swap(true, Ordering::AcqRel) {
                return Ok(());
            }
            if self
                .event_tx
                .send(PluginDispatchMessage::Shutdown(reply))
                .await
                .is_err()
            {
                self.dispatcher_closed.store(false, Ordering::Release);
                return Err("plugin event dispatcher is closed".to_string());
            }
        }
        tokio::time::timeout(timeout, rx)
            .await
            .map_err(|_| "plugin event dispatcher shutdown timed out".to_string())?
            .map_err(|_| "plugin event dispatcher shutdown acknowledgement dropped".to_string())
    }

    /// Check whether the plugin manager is suspended (idle mode).
    pub fn is_suspended(&self) -> bool {
        self.suspended.load(Ordering::Relaxed)
    }

    /// Set the idle hint. Only best-effort high-frequency dispatch is dropped;
    /// reliable queued events continue to drain.
    pub async fn set_suspended(&self, suspended: bool) {
        self.suspended.store(suspended, Ordering::Release);
        if suspended {
            info!("plugin event processing suspended");
        } else {
            info!("plugin event processing resumed");
        }
    }

    pub async fn set_default_state(&self, query: api::ServerStateQuery) {
        #[cfg(feature = "plugin-system")]
        {
            *self
                .wasm_services
                .state_query
                .lock()
                .unwrap_or_else(|e| e.into_inner()) = Some(query);
        }
        #[cfg(not(feature = "plugin-system"))]
        let _ = query;
    }

    pub async fn set_send_chat(
        &self,
        #[allow(unused_variables)] callback: Arc<dyn Fn(i32, String) + Send + Sync>,
    ) {
        #[cfg(feature = "plugin-system")]
        {
            *self
                .wasm_services
                .send_chat
                .lock()
                .unwrap_or_else(|e| e.into_inner()) = Some(callback);
        }
    }

    pub fn http_handle(&self) -> Option<api::HttpHandle> {
        self.http_handle.try_read().ok().and_then(|g| g.clone())
    }

    pub async fn set_http_handle(&self, handle: api::HttpHandle) {
        *self.http_handle.write().await = Some(handle);
    }

    /// Set the server state reference on WASM services (for WIT host impls).
    pub async fn set_server_state(&self, state: std::sync::Arc<crate::server::PlusServerState>) {
        #[cfg(feature = "plugin-system")]
        self.wasm_services
            .set_server_state(&std::sync::Arc::downgrade(&state));
        let _ = state;
    }

    pub async fn register_plugin_api(&self, name: &str, handler: api::PluginApiHandler) {
        self.api_handlers
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(name.to_string(), Arc::clone(&handler));
        // wasm_services shares the same Arc<Mutex<HashMap>> — no separate write needed.
    }

    pub async fn load_plugins(&self) -> Result<usize, String> {
        let dir = Path::new(&self.plugins_dir);
        if !dir.exists() {
            std::fs::create_dir_all(dir).map_err(|e| format!("create plugins dir: {e}"))?;
            return Ok(0);
        }
        let mut paths = std::fs::read_dir(dir)
            .map_err(|e| format!("read plugins dir: {e}"))?
            .filter_map(Result::ok)
            .map(|entry| entry.path())
            .filter(|path| path.extension().and_then(|ext| ext.to_str()) == Some("wasm"))
            .collect::<Vec<_>>();
        paths.sort();

        let mut loaded = 0;
        for path in paths {
            match self.load_plugin(&path).await {
                Ok(meta) => {
                    info!("loaded plugin: {} v{}", meta.info.name, meta.info.version);
                    loaded += 1;
                }
                Err(err) => warn!("failed to load plugin '{}': {err}", path.display()),
            }
        }
        Ok(loaded)
    }

    async fn load_plugin(&self, path: &Path) -> Result<PluginMeta, String> {
        if path.extension().and_then(|ext| ext.to_str()) != Some("wasm") {
            return Err("only .wasm plugins are supported".to_string());
        }
        #[cfg(feature = "plugin-system")]
        {
            let stable_id = path
                .file_stem()
                .and_then(|value| value.to_str())
                .ok_or_else(|| "plugin filename is not UTF-8".to_string())?
                .to_string();
            let capabilities = crate::wasm_host_helpers::load_manifest_capabilities(
                path.to_str()
                    .ok_or_else(|| "plugin path is not UTF-8".to_string())?,
            )?;
            self.wasm_services
                .set_capabilities(&stable_id, capabilities.into_iter().collect());
            let info = PluginInfo {
                name: stable_id.clone(),
                version: "0.1.0".to_string(),
                author: "unknown".to_string(),
                description: format!("WASM plugin from {}", path.display()),
            };
            let mut plugin: Box<dyn PluginHost> = Box::new(wasm::WasmPlugin::new(
                path.to_str()
                    .ok_or_else(|| "plugin path is not UTF-8".to_string())?,
                info,
                Arc::clone(&self.wasm_services),
                self.runtime.clone(),
            ));
            let init_timeout =
                std::time::Duration::from_millis(self.runtime.call_timeout_ms.max(1));
            let plugin = match tokio::time::timeout(
                init_timeout,
                tokio::task::spawn_blocking(move || {
                    plugin.init()?;
                    Ok::<_, String>(plugin)
                }),
            )
            .await
            {
                Ok(Ok(Ok(plugin))) => plugin,
                Ok(Ok(Err(error))) => {
                    self.wasm_services.remove_capabilities(&stable_id);
                    return Err(error);
                }
                Ok(Err(error)) => {
                    self.wasm_services.remove_capabilities(&stable_id);
                    return Err(format!("plugin loader task failed: {error}"));
                }
                Err(_) => {
                    self.wasm_services.remove_capabilities(&stable_id);
                    return Err(format!(
                        "plugin init exceeded {} ms",
                        self.runtime.call_timeout_ms
                    ));
                }
            };
            let meta = plugin.meta().clone();

            let existing: HashSet<String> = self
                .list_plugins()
                .await
                .into_iter()
                .map(|item| item.info.name)
                .collect();
            if existing.contains(&meta.info.name) {
                self.wasm_services.remove_capabilities(&stable_id);
                return Err(format!(
                    "duplicate plugin display name '{}'",
                    meta.info.name
                ));
            }

            let slot = PluginSlotInner::new(plugin);
            self.wasm_services.register_plugin_runtime(&stable_id);
            self.plugins.write().await.push(slot);
            Ok(meta)
        }
        #[cfg(not(feature = "plugin-system"))]
        {
            let _ = path;
            Err("WASM plugin support not enabled".to_string())
        }
    }

    /// Return true only when at least one plugin is currently enabled and not quarantined.
    pub async fn has_plugins(&self) -> bool {
        self.plugins
            .read()
            .await
            .iter()
            .any(|slot| slot.meta_snapshot().enabled && !slot.is_quarantined())
    }

    pub async fn trigger(&self, event: &PluginEvent) -> Vec<PluginEventResult> {
        let slots = self.plugins.read().await.clone();
        if slots.is_empty() {
            return Vec::new();
        }

        let deadline = tokio::time::Instant::now()
            + std::time::Duration::from_millis(self.runtime.call_timeout_ms.max(1));
        let concurrency = self.runtime.max_event_concurrency.max(1);
        let mut pending = slots.iter().cloned();
        let mut tasks = tokio::task::JoinSet::new();

        let spawn_one = |tasks: &mut tokio::task::JoinSet<Option<PluginEventResult>>,
                         slot: PluginSlot| {
            let event = (*event).clone();
            let call_timeout_ms = self.runtime.call_timeout_ms;
            tasks.spawn_blocking(move || {
                let execution_deadline = std::time::Instant::now()
                    + std::time::Duration::from_millis(call_timeout_ms.max(1));
                let _execution = match slot.wait_for_execution_until(execution_deadline) {
                    Ok(permit) => permit,
                    Err(reason) => {
                        warn!(%reason, "plugin event could not acquire its execution slot");
                        return None;
                    }
                };
                let mut plugin = slot.lock().unwrap_or_else(|e| e.into_inner());
                if !plugin.meta().enabled {
                    return None;
                }
                let name = plugin.meta().info.name.clone();
                match plugin.trigger_event(&event) {
                    Ok(responses) if !responses.is_empty() => Some(PluginEventResult {
                        plugin_name: name,
                        responses,
                    }),
                    Ok(_) => None,
                    Err(error) => {
                        plugin.meta_mut().enabled = false;
                        plugin.meta_mut().state = PluginState::Error(error.clone());
                        slot.update_meta(plugin.meta().clone());
                        warn!(plugin = %name, %error, "plugin failed and was disabled");
                        None
                    }
                }
            });
        };

        for _ in 0..concurrency {
            let Some(slot) = pending.next() else {
                break;
            };
            spawn_one(&mut tasks, slot);
        }

        let mut results = Vec::new();
        while !tasks.is_empty() {
            let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
            if remaining.is_zero() {
                warn!(
                    timeout_ms = self.runtime.call_timeout_ms,
                    "plugin event deadline exceeded"
                );
                for slot in &slots {
                    if slot.executing.load(Ordering::Acquire) {
                        slot.quarantine();
                    }
                }
                tasks.abort_all();
                break;
            }

            match tokio::time::timeout(remaining, tasks.join_next()).await {
                Ok(Some(Ok(Some(result)))) => results.push(result),
                Ok(Some(Ok(None))) => {}
                Ok(Some(Err(error))) => warn!(%error, "plugin event task failed"),
                Ok(None) => break,
                Err(_) => {
                    warn!(
                        timeout_ms = self.runtime.call_timeout_ms,
                        "plugin event deadline exceeded"
                    );
                    for slot in &slots {
                        if slot.executing.load(Ordering::Acquire) {
                            slot.quarantine();
                        }
                    }
                    tasks.abort_all();
                    break;
                }
            }

            if let Some(slot) = pending.next() {
                spawn_one(&mut tasks, slot);
            }
        }
        results
    }

    pub async fn list_plugins(&self) -> Vec<PluginMeta> {
        self.plugins
            .read()
            .await
            .iter()
            .map(|slot| slot.meta_snapshot())
            .collect()
    }

    pub async fn call_plugin_api(
        &self,
        plugin_id: &str,
        method: &str,
        args: Vec<serde_json::Value>,
    ) -> Result<serde_json::Value, String> {
        let slots = self.plugins.read().await.clone();
        let selected = slots.into_iter().find(|slot| slot.matches(plugin_id));
        let slot = selected.ok_or_else(|| format!("plugin '{plugin_id}' not found"))?;
        let method = method.to_string();
        let timeout = std::time::Duration::from_millis(self.runtime.call_timeout_ms.max(1));
        let call_slot = Arc::clone(&slot);
        let execution_timeout = timeout;
        let task = tokio::task::spawn_blocking(move || {
            let execution_deadline = std::time::Instant::now() + execution_timeout;
            let _execution = call_slot
                .wait_for_execution_until(execution_deadline)
                .map_err(str::to_string)?;
            call_slot
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .call_api(&method, &args)
        });
        match tokio::time::timeout(timeout, task).await {
            Ok(Ok(result)) => result,
            Ok(Err(error)) => Err(format!("plugin API task failed: {error}")),
            Err(_) => {
                slot.quarantine();
                Err(format!(
                    "plugin API exceeded {} ms and the plugin was quarantined",
                    self.runtime.call_timeout_ms
                ))
            }
        }
    }

    pub async fn enable_plugin(&self, id: &str) -> Result<(), String> {
        self.set_enabled(id, true).await
    }

    pub async fn disable_plugin(&self, id: &str) -> Result<(), String> {
        self.set_enabled(id, false).await
    }

    async fn set_enabled(&self, id: &str, enabled: bool) -> Result<(), String> {
        for slot in self.plugins.read().await.clone() {
            if !slot.matches(id) {
                continue;
            }
            let mut plugin = slot.host.try_lock().map_err(|_| {
                format!("plugin '{id}' is busy; retry after the current call exits")
            })?;
            if enabled {
                slot.clear_quarantine();
            }
            plugin.meta_mut().enabled = enabled;
            plugin.meta_mut().state = if enabled {
                PluginState::Enabled
            } else {
                PluginState::Disabled
            };
            slot.update_meta(plugin.meta().clone());
            return Ok(());
        }
        Err(format!("plugin '{id}' not found"))
    }

    /// Remove a plugin from the registry. The plugin is disabled and unloaded,
    /// but its .wasm file, capability sidecar, and data directory are NOT deleted.
    /// Use `purge_plugin_data` to clean up files separately.
    pub async fn remove_plugin(&self, id: &str) -> Result<(), String> {
        let (slot, plugin_name, stable_id) = {
            let slots = self.plugins.read().await;
            let slot = slots
                .iter()
                .find(|slot| slot.matches(id))
                .cloned()
                .ok_or_else(|| format!("plugin '{id}' not found"))?;
            let meta = slot.meta_snapshot();
            let path = std::path::PathBuf::from(&meta.path);
            let stable_id = path
                .file_stem()
                .and_then(|value| value.to_str())
                .ok_or_else(|| "plugin filename is not UTF-8".to_string())?
                .to_string();
            (slot.clone(), meta.info.name.clone(), stable_id)
        };

        {
            let mut meta = slot.meta_snapshot();
            meta.enabled = false;
            meta.state = PluginState::Disabled;
            slot.update_meta(meta);
            match slot.host.try_lock() {
                Ok(mut plugin) => {
                    plugin.meta_mut().enabled = false;
                    plugin.meta_mut().state = PluginState::Disabled;
                    plugin.cleanup();
                }
                Err(_) => {
                    slot.quarantine();
                    warn!(plugin = %plugin_name, "plugin removed while a call is still running; cleanup is deferred to instance drop");
                }
            }
        }
        self.plugins
            .write()
            .await
            .retain(|candidate| !Arc::ptr_eq(candidate, &slot));

        #[cfg(feature = "plugin-system")]
        {
            self.wasm_services.remove_capabilities(&stable_id);
            let removed = self
                .wasm_services
                .extensions
                .remove_fields_by_plugin(&plugin_name)
                .await;
            if !removed.is_empty() {
                tracing::info!(plugin = %plugin_name, fields = ?removed, "removed extension fields");
            }
        }
        #[cfg(not(feature = "plugin-system"))]
        let _ = (&stable_id, &plugin_name);

        info!(plugin = %plugin_name, stable_id = %stable_id,
            "plugin removed from registry; files and data retained. Use purge_plugin_data to delete them."
        );
        Ok(())
    }

    /// Permanently delete a plugin's .wasm file, capability sidecar, and data directory.
    /// The plugin must already be removed via `remove_plugin`.
    pub async fn purge_plugin_data(&self, id: &str) -> Result<(), String> {
        // Resolve the plugin path from the extension store — the plugin may no longer
        // be in the live registry.
        let plugin_path = std::path::Path::new(&self.plugins_dir).join(format!("{id}.wasm"));
        let sidecar = plugin_path.with_extension("capabilities.json");
        let data_dir = std::path::Path::new("data/plugins").join(id);

        tokio::task::spawn_blocking(move || -> Result<(), String> {
            let mut removed_any = false;
            if plugin_path.exists() {
                std::fs::remove_file(&plugin_path)
                    .map_err(|e| format!("remove plugin file '{}': {e}", plugin_path.display()))?;
                removed_any = true;
            }
            if sidecar.exists() {
                std::fs::remove_file(&sidecar)
                    .map_err(|e| format!("remove capability sidecar '{}': {e}", sidecar.display()))?;
                removed_any = true;
            }
            if data_dir.exists() {
                std::fs::remove_dir_all(&data_dir)
                    .map_err(|e| format!("remove plugin data '{}': {e}", data_dir.display()))?;
                removed_any = true;
            }
            if !removed_any {
                return Err(format!("no files found for plugin '{id}'"));
            }
            Ok(())
        })
        .await
        .map_err(|e| format!("plugin purge task failed: {e}"))??;
        Ok(())
    }

    pub async fn reload_plugins(&self) -> Result<usize, String> {
        self.cleanup_all().await;
        self.load_plugins().await
    }

    pub async fn cleanup_all(&self) {
        let slots = {
            let mut guard = self.plugins.write().await;
            std::mem::take(&mut *guard)
        };
        if let Err(err) = tokio::task::spawn_blocking(move || {
            for slot in slots {
                slot.quarantine();
                match slot.host.try_lock() {
                    Ok(mut plugin) => plugin.cleanup(),
                    Err(_) => {
                        warn!("plugin cleanup skipped because an in-process call is still running")
                    }
                }
            }
        })
        .await
        {
            warn!("plugin cleanup task failed: {err}");
        }
        #[cfg(feature = "plugin-system")]
        {
            self.wasm_services.clear_dynamic_registrations();
            if let Ok(mut capabilities) = self.wasm_services.capabilities.lock() {
                capabilities.clear();
            }
        }
    }

    pub async fn register_cli_command(&self, command: CliCommand) -> Result<(), String> {
        let mut commands = self.cli_commands.lock().unwrap_or_else(|e| e.into_inner());
        if commands.contains_key(&command.name) {
            return Err(format!("CLI command '{}' already registered", command.name));
        }
        commands.insert(command.name.clone(), command);
        Ok(())
    }

    pub async fn execute_cli_command(&self, name: &str, args: &[&str]) -> Option<Vec<String>> {
        let handler = self
            .cli_commands
            .lock()
            .ok()?
            .get(name)
            .map(|command| Arc::clone(&command.handler))?;
        Some(handler(args))
    }

    pub async fn list_cli_commands(&self) -> Vec<CliCommand> {
        self.cli_commands
            .lock()
            .map(|commands| commands.values().cloned().collect())
            .unwrap_or_default()
    }
}

fn plugin_matches(meta: &PluginMeta, selector: &str) -> bool {
    Path::new(&meta.path)
        .file_stem()
        .and_then(|value| value.to_str())
        == Some(selector)
        || meta.info.name == selector
}

pub struct CliCommand {
    pub name: String,
    pub description: String,
    pub usage: String,
    pub handler: Arc<dyn Fn(&[&str]) -> Vec<String> + Send + Sync>,
}

impl std::fmt::Debug for CliCommand {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CliCommand")
            .field("name", &self.name)
            .field("description", &self.description)
            .field("usage", &self.usage)
            .finish()
    }
}

impl Clone for CliCommand {
    fn clone(&self) -> Self {
        Self {
            name: self.name.clone(),
            description: self.description.clone(),
            usage: self.usage.clone(),
            handler: Arc::clone(&self.handler),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PluginEventResult {
    pub plugin_name: String,
    pub responses: Vec<String>,
}
