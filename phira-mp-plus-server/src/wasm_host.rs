//! WASM plugin host runtime.
//!
//! Guest ABI:
//! - `phira_init() -> i32`
//! - `phira_get_info()` writes `[len:u32-le][metadata JSON]` at memory offset 0
//! - `phira_cleanup()`
//! - `phira_on_event(ptr: i32, len: i32) -> i32`
//! - `phira_on_api(method_ptr, method_len, args_ptr, args_len) -> i64`
//! - `phira_alloc(size: i32) -> i32`
//! - `phira_dealloc(ptr: i32, size: i32)`
//!
//! Host imports use module `phira` and names `host/log`, `host/uuid`,
//! `host/time`, and `host/api`. Data exchange uses UTF-8 and JSON-compatible
//! payloads in guest linear memory. Every guest call is fuel-metered when
//! configured and each plugin has an independent memory ceiling.

use crate::extensions::ExtensionManager;
use crate::plugin::{CliCommand, PluginEvent, PluginHost, PluginInfo, WasmRuntimeConfig};
use phira_mp_plus_server_api as api;
use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::{Arc, Mutex, Weak};
use tracing::{error, info, warn};
use wasmtime::AsContext;

const MAX_HOST_INPUT_BYTES: usize = 8 * 1024 * 1024;
const MAX_HOST_OUTPUT_BYTES: usize = 4 * 1024 * 1024;

// ── 共享宿主服务 ──

/// WASM 插件的共享服务（所有插件共用）
///
/// 内部使用 std::sync::RwLock（非异步），因为 WASM 宿主函数
/// 在同步上下文中被调用。
pub struct WasmPluginServices {
    pub extensions: Arc<ExtensionManager>,
    pub state_query: std::sync::RwLock<Option<api::ServerStateQuery>>,
    pub send_chat: std::sync::RwLock<Option<Arc<dyn Fn(i32, String) + Send + Sync>>>,
    pub cli_commands: Mutex<HashMap<String, CliCommand>>,
    pub plugin_configs: std::sync::RwLock<HashMap<String, HashMap<String, String>>>,
    pub http_handle: std::sync::RwLock<Option<api::HttpHandle>>,
    pub api_handlers: Mutex<HashMap<String, api::PluginApiHandler>>,
    pub runtime: WasmRuntimeConfig,
    pub http_client: reqwest::blocking::Client,
    capabilities: Mutex<HashMap<String, HashSet<String>>>,
    registered_apis: Mutex<HashMap<String, HashSet<String>>>,
    plugin_runtimes: Mutex<HashMap<String, Weak<Mutex<Box<dyn PluginHost>>>>>,
    /// Server state reference for WIT component model host implementations.
    /// Set after server initialization via `set_server_state()`.
    pub server_state: std::sync::RwLock<Option<std::sync::Weak<crate::server::PlusServerState>>>,
}

impl WasmPluginServices {
    pub fn new(extensions: Arc<ExtensionManager>, runtime: WasmRuntimeConfig) -> Self {
        let http_client = reqwest::blocking::Client::builder()
            .timeout(std::time::Duration::from_secs(
                runtime.http_timeout_secs.max(1),
            ))
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .unwrap_or_else(|_| reqwest::blocking::Client::new());
        Self {
            extensions,
            state_query: std::sync::RwLock::new(None),
            send_chat: std::sync::RwLock::new(None),
            cli_commands: Mutex::new(HashMap::new()),
            plugin_configs: std::sync::RwLock::new(HashMap::new()),
            http_handle: std::sync::RwLock::new(None),
            api_handlers: Mutex::new(HashMap::new()),
            runtime,
            http_client,
            capabilities: Mutex::new(HashMap::new()),
            registered_apis: Mutex::new(HashMap::new()),
            plugin_runtimes: Mutex::new(HashMap::new()),
            server_state: std::sync::RwLock::new(None),
        }
    }

    /// Set the server state reference (called after server initialization).
    pub fn set_server_state(&self, state: &Arc<crate::server::PlusServerState>) {
        if let Ok(mut guard) = self.server_state.write() {
            *guard = Some(Arc::downgrade(state));
        }
    }

    pub fn register_plugin_runtime(
        &self,
        plugin_id: String,
        runtime: Weak<Mutex<Box<dyn PluginHost>>>,
    ) {
        self.plugin_runtimes
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(plugin_id, runtime);
    }

    pub fn clear_dynamic_registrations(&self) {
        self.cli_commands
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clear();
        self.registered_apis
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clear();
        self.plugin_runtimes
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clear();
        self.capabilities
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clear();
        // api_handlers may contain native registrations and is intentionally kept.
    }

    fn set_capabilities(&self, plugin: &str, caps: HashSet<String>) {
        self.capabilities
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(plugin.to_string(), caps);
    }

}

struct HostState {
    limits: wasmtime::StoreLimits,
}

/// Store data for WIT component model plugin instances.
/// Wraps the WIT host trait implementations alongside resource limits.
struct WitHostState {
    host: crate::wit_host::WitPluginHost,
    #[allow(dead_code)]
    limits: wasmtime::StoreLimits,
}

#[cfg(feature = "wit-bindgen")]
mod wit_host_state_impls {
    use super::WitHostState;
    use crate::plugin_abi::wit_abi as wit;
    use wit::phira::plugin::*;

    // ── phira-types (data-only, no methods) ──
    impl phira_types::Host for WitHostState {}

    // ── phira-events (data-only, no methods) ──
    impl phira_events::Host for WitHostState {}

    // ── phira-host ──
    impl phira_host::Host for WitHostState {
        fn log(&mut self, level: String, message: String) { self.host.log(level, message) }
        fn generate_uuid(&mut self) -> String { self.host.generate_uuid() }
        fn current_time_ms(&mut self) -> u64 { self.host.current_time_ms() }
        fn api_call(&mut self, method: String, args: Vec<phira_types::JsonValue>) -> phira_types::ApiResult { self.host.api_call(method, args) }
        fn send_chat(&mut self, user_id: u32, message: String) { self.host.send_chat(user_id, message) }
        fn http_request(&mut self, url: String, method: String, headers: Vec<(String, String)>, body: Vec<u8>) -> Result<phira_types::HttpResponse, String> { self.host.http_request(url, method, headers, body) }
    }

    // ── phira-query ──
    impl phira_query::Host for WitHostState {
        fn get_user(&mut self, user_id: u32) -> phira_types::ApiResult { self.host.get_user(user_id) }
        fn get_user_extra(&mut self, user_id: u32, key: String) -> phira_types::ApiResult { self.host.get_user_extra(user_id, key) }
        fn set_user_extra(&mut self, user_id: u32, key: String, value: String) -> phira_types::ApiResult { self.host.set_user_extra(user_id, key, value) }
        fn get_room(&mut self, room_id: String) -> phira_types::ApiResult { self.host.get_room(room_id) }
        fn get_room_extra(&mut self, room_id: String, key: String) -> phira_types::ApiResult { self.host.get_room_extra(room_id, key) }
        fn list_rooms(&mut self) -> phira_types::ApiResult { self.host.list_rooms() }
        fn list_online_users(&mut self) -> phira_types::ApiResult { self.host.list_online_users() }
        fn is_user_online(&mut self, user_id: u32) -> bool { self.host.is_user_online(user_id) }
    }

    // ── phira-room-mgmt ──
    impl phira_room_mgmt::Host for WitHostState {
        fn create_empty_room(&mut self, room_id: String, endpoint: Option<String>) -> phira_types::ApiResult { self.host.create_empty_room(room_id, endpoint) }
        fn kick_from_room(&mut self, room_id: String, target_id: u32) -> phira_types::ApiResult { self.host.kick_from_room(room_id, target_id) }
        fn transfer_host(&mut self, room_id: String, target_id: u32) -> phira_types::ApiResult { self.host.transfer_host(room_id, target_id) }
        fn set_host(&mut self, room_id: String, target_id: Option<u32>) -> phira_types::ApiResult { self.host.set_host(room_id, target_id) }
        fn set_room_lock(&mut self, room_id: String, locked: bool) -> phira_types::ApiResult { self.host.set_room_lock(room_id, locked) }
        fn set_room_hidden(&mut self, room_id: String, hidden: bool) -> phira_types::ApiResult { self.host.set_room_hidden(room_id, hidden) }
        fn close_room(&mut self, room_id: String) -> phira_types::ApiResult { self.host.close_room(room_id) }
        fn set_room_phira_api_endpoint(&mut self, room_id: String, endpoint: Option<String>) -> phira_types::ApiResult { self.host.set_room_phira_api_endpoint(room_id, endpoint) }
    }

    // ── phira-user-mgmt ──
    impl phira_user_mgmt::Host for WitHostState {
        fn kick_user(&mut self, user_id: u32, reason: String) -> phira_types::ApiResult { self.host.kick_user(user_id, reason) }
        fn ban_user(&mut self, user_id: u32, reason: String) -> phira_types::ApiResult { self.host.ban_user(user_id, reason) }
        fn unban_user(&mut self, user_id: u32) -> phira_types::ApiResult { self.host.unban_user(user_id) }
        fn get_ban_list(&mut self) -> phira_types::ApiResult { self.host.get_ban_list() }
        fn is_banned(&mut self, user_id: u32) -> bool { self.host.is_banned(user_id) }
    }

    // ── phira-messaging ──
    impl phira_messaging::Host for WitHostState {
        fn send_to_user(&mut self, user_id: u32, message: String) -> phira_types::ApiResult { self.host.send_to_user(user_id, message) }
        fn send_to_room(&mut self, room_id: String, message: String) -> phira_types::ApiResult { self.host.send_to_room(room_id, message) }
        fn send_to_all(&mut self, message: String) -> phira_types::ApiResult { self.host.send_to_all(message) }
    }

    // ── phira-persistence ──
    impl phira_persistence::Host for WitHostState {
        fn query_events(&mut self, since: u64, limit: u32, kind: Option<String>, room: Option<String>, user: Option<u32>) -> phira_types::ApiResult { self.host.query_events(since, limit, kind, room, user) }
        fn query_room_snapshots(&mut self, since: u64, limit: u32) -> phira_types::ApiResult { self.host.query_room_snapshots(since, limit) }
        fn query_touches(&mut self, since: u64, limit: u32, round: Option<String>, player: Option<u32>) -> phira_types::ApiResult { self.host.query_touches(since, limit, round, player) }
        fn query_judges(&mut self, since: u64, limit: u32, round: Option<String>, player: Option<u32>) -> phira_types::ApiResult { self.host.query_judges(since, limit, round, player) }
        fn get_playtime(&mut self, user_id: u32) -> phira_types::ApiResult { self.host.get_playtime(user_id) }
        fn top_playtime(&mut self, limit: u32) -> phira_types::ApiResult { self.host.top_playtime(limit) }
    }

    // ── phira-admin ──
    impl phira_admin::Host for WitHostState {
        fn list_admin_ids(&mut self) -> phira_types::ApiResult { self.host.list_admin_ids() }
        fn is_admin(&mut self, user_id: u32) -> bool { self.host.is_admin(user_id) }
        fn add_admin_id(&mut self, user_id: u32) -> phira_types::ApiResult { self.host.add_admin_id(user_id) }
        fn remove_admin_id(&mut self, user_id: u32) -> phira_types::ApiResult { self.host.remove_admin_id(user_id) }
        fn set_admin_ids(&mut self, ids: Vec<u32>) -> phira_types::ApiResult { self.host.set_admin_ids(ids) }
    }

    // ── phira-config ──
    impl phira_config::Host for WitHostState {
        fn get_config(&mut self, key: String) -> phira_types::ApiResult { self.host.get_config(key) }
        fn set_config(&mut self, key: String, value: String) -> phira_types::ApiResult { self.host.set_config(key, value) }
        fn list_config(&mut self, prefix: String) -> phira_types::ApiResult { self.host.list_config(prefix) }
        fn reload_config(&mut self) -> phira_types::ApiResult { self.host.reload_config() }
        fn poll_config_changes(&mut self, since: u64) -> phira_types::ApiResult { self.host.poll_config_changes(since) }
    }

    // ── phira-simulation ──
    impl phira_simulation::Host for WitHostState {
        fn status(&mut self) -> phira_types::ApiResult { self.host.status() }
        fn run(&mut self, preset: String, users: Option<u32>, rooms: Option<u32>, duration: Option<u32>) -> phira_types::ApiResult { self.host.run(preset, users, rooms, duration) }
        fn stop(&mut self) -> phira_types::ApiResult { self.host.stop() }
        fn cleanup(&mut self) -> phira_types::ApiResult { self.host.cleanup() }
    }

    // ── phira-runtime ──
    impl phira_runtime::Host for WitHostState {
        fn status(&mut self) -> phira_types::ApiResult { self.host.status() }
        fn events(&mut self, limit: Option<u32>) -> phira_types::ApiResult { self.host.events(limit) }
        fn commands(&mut self) -> phira_types::ApiResult { self.host.commands() }
    }
}

// ── WASM 插件实例 ──

/// WASM 插件在 wasmtime 中的导出函数表
pub struct WasmPluginInstance {
    instance: wasmtime::Instance,
    store: wasmtime::Store<HostState>,

    // 导出的函数
    func_init: Option<wasmtime::TypedFunc<(), i32>>,
    func_get_info: Option<wasmtime::TypedFunc<(), ()>>,
    func_cleanup: Option<wasmtime::TypedFunc<(), ()>>,
    func_on_event: Option<wasmtime::TypedFunc<(i32, i32), i32>>,
    func_on_api: Option<wasmtime::TypedFunc<(i32, i32, i32, i32), i64>>,
    func_alloc: Option<wasmtime::TypedFunc<i32, i32>>,
    func_dealloc: Option<wasmtime::TypedFunc<(i32, i32), ()>>,

    // 插件元数据
    pub info: PluginInfo,
    pub plugin_name: String,
    pub plugin_path: String,
    pub initialized: bool,
    runtime: WasmRuntimeConfig,
}

impl WasmPluginInstance {
    pub fn new(
        wasm_bytes: &[u8],
        plugin_path: &str,
        services: Arc<WasmPluginServices>,
        runtime: WasmRuntimeConfig,
    ) -> Result<Self, String> {
        let plugin_name = Path::new(plugin_path)
            .file_stem()
            .and_then(|s| s.to_str())
            .ok_or_else(|| "plugin filename is not valid UTF-8".to_string())?
            .to_string();
        helpers::validate_identifier(&plugin_name)?;
        services.set_capabilities(&plugin_name, helpers::load_manifest_capabilities(plugin_path)?);

        // Configure deterministic resource ceilings before compiling guest code.
        let mut engine_config = wasmtime::Config::new();
        engine_config.consume_fuel(runtime.fuel_per_call > 0);
        engine_config.max_wasm_stack(runtime.max_stack_bytes.max(64 * 1024));
        let engine =
            wasmtime::Engine::new(&engine_config).map_err(|e| format!("engine creation: {}", e))?;

        // Dual-ABI: try component first, fall back to module
        let is_component = wasm_bytes.starts_with(b"\x00asm")
            && wasm_bytes.len() > 8
            && wasm_bytes[8..12] == [0x01, 0x00, 0x00, 0x00];

        if is_component {
            return Self::new_component(engine, wasm_bytes, plugin_name, services, runtime);
        }

        // Fallback: traditional module (JSON bridge ABI)

        // Fallback: traditional module (JSON bridge ABI)
        let module = wasmtime::Module::new(&engine, wasm_bytes)
            .map_err(|e| format!("module compile error: {}", e))?;

        let mut linker: wasmtime::Linker<HostState> = wasmtime::Linker::new(&engine);

        let svc = Arc::clone(&services);

        // 注册日志宿主函数
        linker
            .func_wrap(
                "phira",
                "host/log",
                |mut caller: wasmtime::Caller<'_, HostState>,
                 level_ptr: i32,
                 level_len: i32,
                 msg_ptr: i32,
                 msg_len: i32| {
                    let memory = match caller.get_export("memory").and_then(|e| e.into_memory()) {
                        Some(m) => m,
                        None => {
                            warn!("WASM plugin has no memory export");
                            return;
                        }
                    };
                    let level =
                        read_str_from_memory(&memory, caller.as_context(), level_ptr, level_len);
                    let msg = read_str_from_memory(&memory, caller.as_context(), msg_ptr, msg_len);
                    match level.as_deref().unwrap_or("info") {
                        "error" => error!("[WASM] {}", msg.unwrap_or_default()),
                        "warn" => warn!("[WASM] {}", msg.unwrap_or_default()),
                        _ => info!("[WASM] {}", msg.unwrap_or_default()),
                    }
                },
            )
            .map_err(|e| format!("link log: {}", e))?;

        // 注册 uuid 宿主函数
        linker
            .func_wrap(
                "phira",
                "host/uuid",
                |mut caller: wasmtime::Caller<'_, HostState>, out_ptr: i32, out_len: i32| {
                    let memory = match caller.get_export("memory").and_then(|e| e.into_memory()) {
                        Some(m) => m,
                        None => return,
                    };
                    if out_ptr < 0 || out_len <= 0 {
                        return;
                    }
                    let uuid = uuid::Uuid::new_v4().to_string();
                    let bytes = uuid.as_bytes();
                    let len = bytes.len().min(out_len as usize);
                    if let Err(e) = memory.write(&mut caller, out_ptr as usize, &bytes[..len]) {
                        warn!("WASM uuid write error: {}", e);
                    }
                },
            )
            .map_err(|e| format!("link uuid: {}", e))?;

        // 注册时间宿主函数
        linker
            .func_wrap("phira", "host/time", || -> i64 {
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_millis() as i64)
                    .unwrap_or(0)
            })
            .map_err(|e| format!("link time: {}", e))?;

        // 注册通用 API 调用宿主函数：
        //   phira:host/api(method_ptr, method_len, args_ptr, args_len, out_ptr, out_len) -> i32
        //   method: JSON string — 方法名（如 "state.query", "send.to_user", "ext.get_user", "http.get"）
        //   args:   JSON string — 参数
        //   out:    输出缓冲区指针（由插件分配）
        //   returns: 0 = 成功，数据写入 out；非0 = 错误码
        //   输出格式: 写入内存为 [len:i32][json_bytes]
        linker
            .func_wrap("phira", "host/api", {
                let svc = Arc::clone(&svc);
                let pn = plugin_name.clone();
                move |mut caller: wasmtime::Caller<'_, HostState>,
                      method_ptr: i32,
                      method_len: i32,
                      args_ptr: i32,
                      args_len: i32,
                      out_ptr: i32,
                      out_len: i32|
                      -> i32 {
                    let memory = match caller.get_export("memory").and_then(|e| e.into_memory()) {
                        Some(m) => m,
                        None => {
                            warn!("WASM plugin has no memory");
                            return -1;
                        }
                    };
                    let method = match read_str_from_memory(
                        &memory,
                        caller.as_context(),
                        method_ptr,
                        method_len,
                    ) {
                        Some(s) => s,
                        None => return -2,
                    };
                    let args_str =
                        read_str_from_memory(&memory, caller.as_context(), args_ptr, args_len)
                            .unwrap_or_default();

                    if out_ptr < 0 || out_len < 4 || out_len as usize > MAX_HOST_OUTPUT_BYTES {
                        return -3;
                    }
                    let result = dispatch_api(&svc, &pn, &method, &args_str);

                    match result {
                        Ok(json) => {
                            let bytes = json.as_bytes();
                            let data_len = bytes.len();
                            let total_len = data_len + 4;
                            if total_len as i32 > out_len {
                                warn!(
                                    "WASM api output buffer too small: need {}, have {}",
                                    total_len, out_len
                                );
                                return -3;
                            }
                            let len_prefix = (data_len as i32).to_le_bytes();
                            if memory
                                .write(&mut caller, out_ptr as usize, &len_prefix)
                                .is_err()
                            {
                                return -4;
                            }
                            let Some(data_ptr) = out_ptr.checked_add(4) else {
                                return -5;
                            };
                            if memory.write(&mut caller, data_ptr as usize, bytes).is_err() {
                                return -5;
                            }
                            0
                        }
                        Err(e) => {
                            // 写入错误信息
                            let err_bytes = e.as_bytes();
                            let total_len = err_bytes.len() + 4;
                            if total_len as i32 <= out_len {
                                let len_prefix = (err_bytes.len() as i32).to_le_bytes();
                                let _ = memory.write(&mut caller, out_ptr as usize, &len_prefix);
                                if let Some(data_ptr) = out_ptr.checked_add(4) {
                                    let _ = memory.write(&mut caller, data_ptr as usize, err_bytes);
                                }
                            }
                            -1
                        }
                    }
                }
            })
            .map_err(|e| format!("link api: {}", e))?;

        // Create one limited store per plugin.
        let memory_limit = runtime.max_memory_mb.max(1).saturating_mul(1024 * 1024);
        let limits = wasmtime::StoreLimitsBuilder::new()
            .memory_size(memory_limit)
            .build();
        let mut store = wasmtime::Store::new(&engine, HostState { limits });
        store.limiter(|state| &mut state.limits);
        if runtime.fuel_per_call > 0 {
            store
                .set_fuel(runtime.fuel_per_call)
                .map_err(|e| format!("set initial fuel: {e}"))?;
        }
        let instance = linker
            .instantiate(&mut store, &module)
            .map_err(|e| format!("instantiate '{}': {}", plugin_name, e))?;

        // 获取导出函数
        let func_init = instance
            .get_typed_func::<(), i32>(&mut store, "phira_init")
            .ok();
        let func_get_info = instance
            .get_typed_func::<(), ()>(&mut store, "phira_get_info")
            .ok();
        let func_cleanup = instance
            .get_typed_func::<(), ()>(&mut store, "phira_cleanup")
            .ok();
        let func_on_event = instance
            .get_typed_func::<(i32, i32), i32>(&mut store, "phira_on_event")
            .ok();
        let func_on_api = instance
            .get_typed_func::<(i32, i32, i32, i32), i64>(&mut store, "phira_on_api")
            .ok();
        let func_alloc = instance
            .get_typed_func::<i32, i32>(&mut store, "phira_alloc")
            .ok();
        let func_dealloc = instance
            .get_typed_func::<(i32, i32), ()>(&mut store, "phira_dealloc")
            .ok();

        let pn = plugin_name.clone();
        let mut plugin = Self {
            instance,
            store,
            func_init,
            func_get_info,
            func_cleanup,
            func_on_event,
            func_on_api,
            func_alloc,
            func_dealloc,
            info: PluginInfo {
                name: pn.clone(),
                version: "0.1.0".to_string(),
                author: "unknown".to_string(),
                description: format!("WASM plugin from {}", plugin_path),
            },
            plugin_name: pn.clone(),
            plugin_path: plugin_path.to_string(),
            initialized: false,
            runtime,
        };

        // 尝试从 WASM 内存读取插件信息
        plugin.read_plugin_info();

        Ok(plugin)
    }

    fn reset_fuel(&mut self) -> Result<(), String> {
        if self.runtime.fuel_per_call > 0 {
            self.store
                .set_fuel(self.runtime.fuel_per_call)
                .map_err(|e| format!("set fuel: {e}"))?;
        }
        Ok(())
    }

    /// Read metadata written at memory offset zero as `[len:u32][json]`.
    fn read_plugin_info(&mut self) {
        if self.reset_fuel().is_err() {
            return;
        }
        let called_get_info = self
            .func_get_info
            .as_ref()
            .is_some_and(|get_info| get_info.call(&mut self.store, ()).is_ok());
        let Some(memory) = self.instance.get_memory(&mut self.store, "memory") else {
            return;
        };
        let mut header = [0u8; 4];
        if memory.read(&self.store, 0, &mut header).is_err() {
            return;
        }
        let len = u32::from_le_bytes(header) as usize;
        if len == 0 || len > 64 * 1024 {
            return;
        }
        let mut buf = vec![0u8; len];
        if memory.read(&self.store, 4, &mut buf).is_err() {
            return;
        }
        if let Ok(info) = serde_json::from_slice::<serde_json::Value>(&buf) {
            if let Some(name) = info.get("name").and_then(|v| v.as_str()) {
                if helpers::validate_display_name(name).is_ok() {
                    self.info.name = name.to_string();
                }
            }
            if let Some(version) = info.get("version").and_then(|v| v.as_str()) {
                self.info.version = helpers::truncate_string(version, 128);
            }
            if let Some(author) = info.get("author").and_then(|v| v.as_str()) {
                self.info.author = helpers::truncate_string(author, 256);
            }
            if let Some(description) = info.get("description").and_then(|v| v.as_str()) {
                self.info.description = helpers::truncate_string(description, 2048);
            }
        }
        if called_get_info {
            info!("WASM plugin '{}' metadata loaded", self.plugin_name);
        }
    }

    pub fn call_init(&mut self) -> Result<(), String> {
        self.reset_fuel()?;
        let code = match self.func_init.as_ref() {
            Some(init) => init
                .call(&mut self.store, ())
                .map_err(|e| format!("plugin '{}' init trap: {e}", self.plugin_name))?,
            None => 0,
        };
        if code != 0 {
            return Err(format!(
                "plugin '{}' init returned error code {code}",
                self.plugin_name
            ));
        }
        self.initialized = true;
        info!("WASM plugin '{}' initialized", self.plugin_name);
        Ok(())
    }

    pub fn call_cleanup(&mut self) {
        if !self.initialized {
            return;
        }
        let _ = self.reset_fuel();
        if let Some(cleanup) = self.func_cleanup.as_ref() {
            if let Err(err) = cleanup.call(&mut self.store, ()) {
                warn!("plugin '{}' cleanup error: {err}", self.plugin_name);
            }
        }
        self.initialized = false;
    }

    pub fn call_on_event(&mut self, event: &PluginEvent) -> Result<i32, String> {
        let Some(on_event) = self.func_on_event.clone() else {
            return Ok(1);
        };
        self.reset_fuel()?;
        let json = crate::plugin_abi::encode_plugin_event_json(event);
        let (ptr, len) = self.write_to_wasm(json.as_bytes())?;
        let result = on_event
            .call(&mut self.store, (ptr, len))
            .map_err(|e| format!("plugin '{}' event trap: {e}", self.plugin_name));
        self.dealloc(ptr, len);
        result
    }

    /// Call the optional guest-to-guest API export.
    /// Return value packs `(len << 32) | ptr` as unsigned 32-bit fields.
    pub fn call_api(
        &mut self,
        method: &str,
        args: &[serde_json::Value],
    ) -> Result<serde_json::Value, String> {
        let on_api = self
            .func_on_api
            .clone()
            .ok_or_else(|| "plugin does not export phira_on_api".to_string())?;
        self.reset_fuel()?;
        let args_json = crate::plugin_abi::encode_plugin_api_args_json(args)?;
        let (method_ptr, method_len) = self.write_to_wasm(method.as_bytes())?;
        let (args_ptr, args_len) = match self.write_to_wasm(&args_json) {
            Ok(value) => value,
            Err(err) => {
                self.dealloc(method_ptr, method_len);
                return Err(err);
            }
        };
        let packed = on_api
            .call(
                &mut self.store,
                (method_ptr, method_len, args_ptr, args_len),
            )
            .map_err(|e| format!("plugin '{}' API trap: {e}", self.plugin_name));
        self.dealloc(method_ptr, method_len);
        self.dealloc(args_ptr, args_len);
        let packed = packed? as u64;
        let ptr = (packed & 0xffff_ffff) as u32 as usize;
        let len = (packed >> 32) as u32 as usize;
        if len == 0 || len > MAX_HOST_OUTPUT_BYTES {
            return Err(format!("invalid plugin API result length {len}"));
        }
        let memory = self
            .instance
            .get_memory(&mut self.store, "memory")
            .ok_or_else(|| "plugin has no exported memory".to_string())?;
        let mut bytes = vec![0u8; len];
        memory
            .read(&self.store, ptr, &mut bytes)
            .map_err(|e| format!("read plugin API result: {e}"))?;
        self.dealloc(ptr as i32, len as i32);
        crate::plugin_abi::decode_plugin_api_result_json(&bytes)
    }

    fn write_to_wasm(&mut self, data: &[u8]) -> Result<(i32, i32), String> {
        if data.len() > MAX_HOST_INPUT_BYTES || data.len() > i32::MAX as usize {
            return Err("guest input exceeds host limit".to_string());
        }
        let len = data.len() as i32;
        let ptr = self
            .func_alloc
            .as_ref()
            .ok_or_else(|| "plugin does not export phira_alloc".to_string())?
            .call(&mut self.store, len)
            .map_err(|e| format!("plugin allocation failed: {e}"))?;
        if ptr < 0 {
            return Err("plugin returned an invalid allocation pointer".to_string());
        }
        let memory = self
            .instance
            .get_memory(&mut self.store, "memory")
            .ok_or_else(|| "plugin has no exported memory".to_string())?;
        memory
            .write(&mut self.store, ptr as usize, data)
            .map_err(|e| format!("write guest memory: {e}"))?;
        Ok((ptr, len))
    }

    fn dealloc(&mut self, ptr: i32, len: i32) {
        if ptr < 0 || len < 0 {
            return;
        }
        if let Some(dealloc) = self.func_dealloc.as_ref() {
            let _ = dealloc.call(&mut self.store, (ptr, len));
        }
    }

    /// Load a WIT component via the component model.
    #[cfg(feature = "wit-bindgen")]
    fn new_component(
        engine: wasmtime::Engine,
        wasm_bytes: &[u8],
        plugin_name: String,
        services: Arc<WasmPluginServices>,
        runtime: WasmRuntimeConfig,
    ) -> Result<Self, String> {
        use crate::plugin_abi::wit_abi;
        let component = wasmtime::component::Component::new(&engine, wasm_bytes)
            .map_err(|e| format!("component compile: {e}"))?;
        let mut linker = wasmtime::component::Linker::<WitHostState>::new(&engine);
        wit_abi::PhiraPluginV2::add_to_linker(&mut linker, |state| state)
            .map_err(|e| format!("linker setup: {e}"))?;
        let state_ref = services
            .server_state
            .read()
            .unwrap_or_else(|e| e.into_inner());
        let host = state_ref
            .as_ref()
            .and_then(|w| w.upgrade())
            .map(|s| crate::wit_host::WitPluginHost::new(s, plugin_name.clone()))
            .ok_or_else(|| "server state not available — set via PluginManager::set_server_state".to_string())?;
        let store_limits = {
            let mut builder = wasmtime::StoreLimitsBuilder::new();
            if runtime.max_memory_mb > 0 {
                builder = builder.memory_size(runtime.max_memory_mb as usize * 1024 * 1024);
            }
            builder.build()
        };
        if runtime.max_stack_bytes > 0 {
            // Stack limit is set at engine config level already.
        }
        let host_state = WitHostState { host, limits: store_limits };
        let mut store = wasmtime::Store::new(&engine, host_state);
        let _instance = futures::executor::block_on(
            linker.instantiate_async(&mut store, &component),
        )
        .map_err(|e| format!("instantiate component: {e}"))?;
        Err(format!(
            "WIT component '{plugin_name}' compiled. TODO: wrap component exports in PluginHost impl."
        ))
    }

    #[cfg(not(feature = "wit-bindgen"))]
    fn new_component(
        _engine: wasmtime::Engine,
        _wasm_bytes: &[u8],
        plugin_name: String,
        _services: Arc<WasmPluginServices>,
        _runtime: WasmRuntimeConfig,
    ) -> Result<Self, String> {
        Err(format!(
            "WIT component model requires the wit-bindgen feature. \
             Enable it in Cargo.toml to load component '{}'.",
            plugin_name
        ))
    }
}

/// WIT component model plugin instance.
/// Wraps a compiled component and its store, providing the same lifecycle
/// API as WasmPluginInstance but through the component model.
#[cfg(feature = "wit-bindgen")]
pub struct WitPluginComponent {
    store: wasmtime::Store<WitHostState>,
    component: crate::plugin_abi::wit_abi::PhiraPluginV2,
    pub info: PluginInfo,
    pub plugin_name: String,
    pub initialized: bool,
}

#[cfg(feature = "wit-bindgen")]
impl WitPluginComponent {
    pub fn new(
        wasm_bytes: &[u8],
        plugin_name: String,
        services: Arc<WasmPluginServices>,
        runtime: WasmRuntimeConfig,
    ) -> Result<Self, String> {
        use crate::plugin_abi::wit_abi;
        let mut engine_config = wasmtime::Config::new();
        engine_config.consume_fuel(runtime.fuel_per_call > 0);
        engine_config.max_wasm_stack(runtime.max_stack_bytes.max(64 * 1024));
        let engine = wasmtime::Engine::new(&engine_config)
            .map_err(|e| format!("engine creation: {e}"))?;
        let component = wasmtime::component::Component::new(&engine, wasm_bytes)
            .map_err(|e| format!("component compile: {e}"))?;
        let mut linker = wasmtime::component::Linker::<WitHostState>::new(&engine);
        wit_abi::PhiraPluginV2::add_to_linker(&mut linker, |state| state)
            .map_err(|e| format!("linker setup: {e}"))?;
        let state_ref = services
            .server_state
            .read()
            .unwrap_or_else(|e| e.into_inner());
        let host = state_ref
            .as_ref()
            .and_then(|w| w.upgrade())
            .map(|s| crate::wit_host::WitPluginHost::new(s, plugin_name.clone()))
            .ok_or_else(|| "server state not available".to_string())?;
        let store_limits = {
            let mut builder = wasmtime::StoreLimitsBuilder::new();
            if runtime.max_memory_mb > 0 {
                builder = builder.memory_size(runtime.max_memory_mb as usize * 1024 * 1024);
            }
            builder.build()
        };
        let host_state = WitHostState { host, limits: store_limits };
        let mut store = wasmtime::Store::new(&engine, host_state);
        let _instance: wasmtime::component::Instance = futures::executor::block_on(
            linker.instantiate_async(&mut store, &component),
        )
        .map_err(|e| format!("instantiate component: {e}"))?;
        // TODO: get PhiraPluginV2 handle from instance.  The generated bindings
        // provide a new() or from_handle() constructor.  Until then, component
        // lifecycle methods are stubs.
        let component_handle = crate::plugin_abi::wit_abi::PhiraPluginV2::new(&mut store, &_instance)
            .map_err(|e| format!("get component handle: {e}"))?;
        let info = PluginInfo {
            name: plugin_name.clone(),
            version: "0.1.0".to_string(),
            author: "unknown".to_string(),
            description: "WIT component plugin".to_string(),
        };
        Ok(Self {
            store,
            component: component_handle,
            info,
            plugin_name,
            initialized: false,
        })
    }

    pub fn call_init(&mut self) -> Result<(), String> {
        let result = self.component.call_init(&mut self.store)
            .map_err(|e| format!("component init: {e}"))?;
        result.map_err(|e| format!("component init returned error: {e}"))?;
        self.initialized = true;
        Ok(())
    }

    pub fn call_cleanup(&mut self) {
        if !self.initialized {
            return;
        }
        let _ = self.component.call_cleanup(&mut self.store);
        self.initialized = false;
    }

    pub fn call_on_event(&mut self, _event: &PluginEvent) -> Result<i32, String> {
        // TODO: convert PluginEvent to WIT plugin-event and call component.on_event.
        // For now, return success without dispatching.
        let _ = &self.component;
        Ok(0)
    }

    pub fn call_api(&mut self, _method: &str, _args: &[serde_json::Value]) -> Result<serde_json::Value, String> {
        // TODO: call component.on_api with method and args.
        Err("component call_api not yet wired".to_string())
    }
}

#[cfg(feature = "wit-bindgen")]
impl Drop for WitPluginComponent {
    fn drop(&mut self) {
        self.call_cleanup();
    }
}

impl Drop for WasmPluginInstance {
    fn drop(&mut self) {
        if self.initialized {
            self.call_cleanup();
        }
    }
}

/// JSON bridge ABI has been removed in favour of the WIT component model.
/// See `wit_host.rs` for WIT host trait implementations.
fn dispatch_api(
    _svc: &WasmPluginServices,
    plugin_name: &str,
    method: &str,
    _args: &str,
) -> Result<String, String> {
    Err(format!(
        "JSON bridge ABI removed — WIT component model required. \
         Plugin '{plugin_name}' called '{method}'. Recompile plugin for the \
         phira-plugin-v2 WIT world. See wit/phira-plugin.wit."
    ))
}
// ── 辅助函数 ──

use crate::wasm_host_helpers as helpers;

fn read_str_from_memory(
    memory: &wasmtime::Memory,
    ctx: impl wasmtime::AsContext,
    ptr: i32,
    len: i32,
) -> Option<String> {
    if len <= 0 || ptr < 0 || len as usize > MAX_HOST_INPUT_BYTES {
        return None;
    }
    let mut bytes = vec![0u8; len as usize];
    memory.read(ctx, ptr as usize, &mut bytes).ok()?;
    String::from_utf8(bytes).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identifiers_are_restricted() {
        assert!(helpers::validate_identifier("plugin.api-v1").is_ok());
        assert!(helpers::validate_identifier("../escape").is_err());
        assert!(helpers::validate_identifier("has space").is_err());
    }

    #[test]
    fn default_capabilities_are_not_privileged() {
        let caps = helpers::default_capabilities();
        assert!(caps.contains("state.read"));
        assert!(!caps.contains("admin"));
        assert!(!caps.contains("room.manage"));
        assert!(!caps.contains("http"));
    }
}
