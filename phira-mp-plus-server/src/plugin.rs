//! Phira-mp+ WASM plugin manager.
//!
//! Guest execution is moved off Tokio worker threads and serialized per plugin.
//! The async plugin-list lock is never held while guest code is running.

use crate::extensions::ExtensionManager;
use phira_mp_plus_server_api as api;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, Mutex};
use tokio::sync::{OwnedSemaphorePermit, RwLock, Semaphore};
use tracing::{info, warn};

pub use api::{HttpHandle, JudgeEventItem, PluginEvent, PluginInfo, TouchEventPoint};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WasmRuntimeConfig {
    #[serde(default = "default_wasm_memory_mb")]
    pub max_memory_mb: usize,
    /// Fuel added before each guest call. Zero disables metering.
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
    #[serde(default = "default_wasm_event_concurrency")]
    pub max_event_concurrency: usize,
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

type PluginSlot = Arc<Mutex<Box<dyn PluginHost>>>;

#[cfg(feature = "plugin-system")]
pub mod wasm {
    use super::*;
    use crate::wasm_host;

    pub struct WasmPlugin {
        meta: PluginMeta,
        services: Arc<wasm_host::WasmPluginServices>,
        #[allow(dead_code)]
    runtime: WasmRuntimeConfig,
        component_instance: Option<wasm_host::WitPluginComponent>,
    }

    impl WasmPlugin {
        pub fn new(
            path: &str,
            info: PluginInfo,
            services: Arc<wasm_host::WasmPluginServices>,
            #[allow(dead_code)]
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
            self.meta.info = component.info.clone();
            component.call_init()?;
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

pub struct PluginManager {
    plugins: Arc<RwLock<Vec<PluginSlot>>>,
    cli_commands: Arc<Mutex<HashMap<String, CliCommand>>>,
    api_handlers: Arc<Mutex<HashMap<String, api::PluginApiHandler>>>,
    plugins_dir: String,
    #[allow(dead_code)]
    runtime: WasmRuntimeConfig,
    event_gate: Arc<Semaphore>,
    http_handle: Arc<RwLock<Option<api::HttpHandle>>>,
    #[cfg(feature = "plugin-system")]
    wasm_services: Arc<crate::wasm_host::WasmPluginServices>,
}

impl PluginManager {
    pub fn new(
        plugins_dir: &str,
        extensions: Arc<ExtensionManager>,
        #[allow(dead_code)]
    runtime: WasmRuntimeConfig,
    ) -> Self {
        let cli_commands = Arc::new(Mutex::new(HashMap::new()));
        let api_handlers = Arc::new(Mutex::new(HashMap::new()));
        let http_handle = Arc::new(RwLock::new(None));

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
            event_gate: Arc::new(Semaphore::new(runtime.max_event_concurrency.max(1))),
            runtime,
            http_handle,
            #[cfg(feature = "plugin-system")]
            wasm_services,
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
        let _ = query;
    }

    pub async fn set_send_chat(&self, _callback: Arc<dyn Fn(i32, String) + Send + Sync>) {
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
            let plugin = tokio::task::spawn_blocking(move || {
                plugin.init()?;
                Ok::<_, String>(plugin)
            })
            .await
            .map_err(|e| format!("plugin loader task failed: {e}"))??;
            let meta = plugin.meta().clone();

            let existing: HashSet<String> = self
                .list_plugins()
                .await
                .into_iter()
                .map(|item| item.info.name)
                .collect();
            if existing.contains(&meta.info.name) {
                return Err(format!(
                    "duplicate plugin display name '{}'",
                    meta.info.name
                ));
            }

            let slot = Arc::new(Mutex::new(plugin));
            self.wasm_services
                .register_plugin_runtime(&stable_id);
            self.plugins.write().await.push(slot);
            Ok(meta)
        }
        #[cfg(not(feature = "plugin-system"))]
        {
            let _ = path;
            Err("WASM plugin support not enabled".to_string())
        }
    }

    /// Quick check — avoids RwLock — returns true only if at least one plugin has been loaded.
    pub async fn has_plugins(&self) -> bool {
        !self.plugins.read().await.is_empty()
    }

    pub async fn trigger(&self, event: &PluginEvent) -> Vec<PluginEventResult> {
        let permit = match Arc::clone(&self.event_gate).acquire_owned().await {
            Ok(permit) => permit,
            Err(_) => return Vec::new(),
        };
        self.trigger_with_permit(event.clone(), permit).await
    }

    /// Dispatch a plugin event only when a concurrency slot is immediately available.
    ///
    /// High-frequency telemetry paths use this to avoid building an unbounded backlog of
    /// Tokio tasks while plugin WASM calls are already saturated.
    pub fn try_spawn_trigger(self: &Arc<Self>, event: PluginEvent) -> bool {
        let permit = match Arc::clone(&self.event_gate).try_acquire_owned() {
            Ok(permit) => permit,
            Err(_) => return false,
        };
        let manager = Arc::clone(self);
        tokio::spawn(async move {
            manager.trigger_with_permit(event, permit).await;
        });
        true
    }

    async fn trigger_with_permit(
        &self,
        event: PluginEvent,
        _permit: OwnedSemaphorePermit,
    ) -> Vec<PluginEventResult> {
        let slots = self.plugins.read().await.clone();
        match tokio::task::spawn_blocking(move || {
            let mut results = Vec::new();
            for slot in slots {
                let mut plugin = slot.lock().unwrap_or_else(|e| e.into_inner());
                if !plugin.meta().enabled {
                    continue;
                }
                let name = plugin.meta().info.name.clone();
                match plugin.trigger_event(&event) {
                    Ok(responses) if !responses.is_empty() => results.push(PluginEventResult {
                        plugin_name: name,
                        responses,
                    }),
                    Ok(_) => {}
                    Err(err) => {
                        plugin.meta_mut().enabled = false;
                        plugin.meta_mut().state = PluginState::Error(err.clone());
                        warn!("plugin '{}' failed and was disabled: {err}", name);
                    }
                }
            }
            results
        })
        .await
        {
            Ok(results) => results,
            Err(err) => {
                warn!("plugin event task failed: {err}");
                Vec::new()
            }
        }
    }

    pub async fn list_plugins(&self) -> Vec<PluginMeta> {
        self.plugins
            .read()
            .await
            .clone()
            .into_iter()
            .map(|slot| {
                slot.lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .meta()
                    .clone()
            })
            .collect()
    }

    pub async fn call_plugin_api(
        &self,
        plugin_id: &str,
        method: &str,
        args: Vec<serde_json::Value>,
    ) -> Result<serde_json::Value, String> {
        let slots = self.plugins.read().await.clone();
        let selected = slots.into_iter().find(|slot| {
            let plugin = slot.lock().unwrap_or_else(|e| e.into_inner());
            plugin_matches(plugin.meta(), plugin_id)
        });
        let slot = selected.ok_or_else(|| format!("plugin '{plugin_id}' not found"))?;
        let method = method.to_string();
        tokio::task::spawn_blocking(move || {
            slot.lock()
                .unwrap_or_else(|e| e.into_inner())
                .call_api(&method, &args)
        })
        .await
        .map_err(|e| format!("plugin API task failed: {e}"))?
    }

    pub async fn enable_plugin(&self, id: &str) -> Result<(), String> {
        self.set_enabled(id, true).await
    }

    pub async fn disable_plugin(&self, id: &str) -> Result<(), String> {
        self.set_enabled(id, false).await
    }

    async fn set_enabled(&self, id: &str, enabled: bool) -> Result<(), String> {
        for slot in self.plugins.read().await.clone() {
            let mut plugin = slot.lock().unwrap_or_else(|e| e.into_inner());
            if plugin_matches(plugin.meta(), id) {
                plugin.meta_mut().enabled = enabled;
                plugin.meta_mut().state = if enabled {
                    PluginState::Enabled
                } else {
                    PluginState::Disabled
                };
                return Ok(());
            }
        }
        Err(format!("plugin '{id}' not found"))
    }

    /// Remove a plugin: disable, delete its data directory, and remove from registry.
    pub async fn remove_plugin(&self, id: &str) -> Result<(), String> {
        let plugin_dir = {
            let slots = self.plugins.read().await;
            let slot = slots.iter().find(|slot| {
                let plugin = slot.lock().unwrap_or_else(|e| e.into_inner());
                plugin_matches(plugin.meta(), id)
            }).ok_or_else(|| format!("plugin '{id}' not found"))?;
            let plugin = slot.lock().unwrap_or_else(|e| e.into_inner());
            let meta = plugin.meta();
            std::path::Path::new(&meta.path).parent().map(|p| p.to_path_buf())
        };
        // Remove from plugin list (triggers cleanup via Drop)
        let mut slots = self.plugins.write().await;
        slots.retain(|slot| {
            let plugin = slot.lock().unwrap_or_else(|e| e.into_inner());
            !plugin_matches(plugin.meta(), id)
        });
        // Delete plugin data directory
        if let Some(dir) = plugin_dir {
            if dir.exists() {
                tokio::task::spawn_blocking(move || {
                    let _ = std::fs::remove_dir_all(&dir);
                }).await.ok();
            }
        }
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
                slot.lock().unwrap_or_else(|e| e.into_inner()).cleanup();
            }
        })
        .await
        {
            warn!("plugin cleanup task failed: {err}");
        }
        #[cfg(feature = "plugin-system")]
        self.wasm_services.clear_dynamic_registrations();
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
