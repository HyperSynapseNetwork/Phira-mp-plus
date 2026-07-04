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
use std::io::Read;
use std::net::IpAddr;
use std::path::{Component, Path, PathBuf};
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

    fn has_capability(&self, plugin: &str, capability: &str) -> bool {
        self.capabilities
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(plugin)
            .is_some_and(|caps| caps.contains("*") || caps.contains(capability))
    }
}

struct HostState {
    limits: wasmtime::StoreLimits,
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

        // 创建模块
        let module = wasmtime::Module::new(&engine, wasm_bytes)
            .map_err(|e| format!("module compile error: {}", e))?;

        // 创建链接器并注册宿主函数
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
                    let result = Self::dispatch_api(&svc, &pn, &method, &args_str);

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

    /// 通用 API 分发：处理 WASM 插件的 `phira:host/api` 调用
    ///
    /// 支持的 method:
    /// ── 状态查询 ──
    /// - `state.query`    → 调用 ServerStateQuery (rooms.list, rooms.by_user, user_name, send_chat, send_room_chat, rooms.by_user)
    /// - `player.touches` → 查询指定用户的最近触控数据
    /// - `player.judges`  → 查询指定用户的最近判定数据
    /// - `round.data`     → 查询指定轮次+玩家的完整 Touches/Judges
    /// - `round.list`     → 列出所有已记录的轮次
    /// ── 消息发送 ──
    /// - `send.to_user`   → 发送聊天消息给指定用户
    /// - `send.to_room`   → 向房间广播消息
    /// - `send.to_all`    → 向所有用户广播
    /// ── 扩展数据 ──
    /// - `ext.get_user`   → 获取用户扩展数据
    /// - `ext.set_user`   → 设置用户扩展数据
    /// - `ext.get_room`   → 获取房间扩展数据
    /// - `ext.set_room`   → 设置房间扩展数据
    /// ── 房间管理（room-management） ──
    /// - `room.create_empty` → 创建无人持久空房间
    /// - `room.kick`         → 从房间踢出用户
    /// - `room.set_host`     → 设置房主，target_id 为 null/`?` 表示系统房主
    /// - `room.set_lock`     → 锁定/解锁房间
    /// - `room.force_move`   → 强制迁移用户到房间
    /// - `room.set_hidden`   → 设置房间隐藏状态
    /// - `room.is_hidden`    → 查询房间隐藏状态
    /// - `room.set_persistent_empty` → 设置房间无人保留
    /// - `room.set_phira_api_endpoint` → 设置/清除房间 Phira API 覆盖
    /// - `room.get_phira_api_endpoint` → 查询房间 Phira API 生效地址
    /// - `room.close`        → 解散房间
    /// ── 用户管理（user-management） ──
    /// - `admin.kick_user`  → 从服务器踢出用户
    /// - `admin.ban_user`   → 封禁用户
    /// - `admin.unban_user` → 解封用户
    /// - `admin.is_banned`  → 检查用户是否被封禁
    /// - `admin.ban_list`   → 获取封禁列表
    /// - `admin.list_users` → 列出所有在线用户
    /// ── 工具 ──
    /// - `config.get/set`  → 插件配置读写
    /// - `http.get/post`   → HTTP 请求
    /// - `file.read/write` → 文件读写
    /// - `uuid.v4`         → 生成 UUID v4
    /// - `time.now`        → 获取 Unix 时间戳
    /// ── 插件互调用（WASM 插件注册/调用 API） ──
    /// - `plugin.api_call`    → 调用其他 WASM 插件的 API
    /// - `plugin.api_register`→ 注册本插件的 API 供其他 WASM 插件调用
    fn dispatch_api(
        svc: &WasmPluginServices,
        plugin_name: &str,
        method: &str,
        args: &str,
    ) -> Result<String, String> {
        if args.len() > MAX_HOST_INPUT_BYTES {
            return Err("API arguments exceed host limit".to_string());
        }
        if let Some(capability) = helpers::required_capability(method) {
            if !svc.has_capability(plugin_name, capability) {
                return Err(format!(
                    "capability '{capability}' is required for {method}"
                ));
            }
        }
        let value: serde_json::Value = if args.trim().is_empty() {
            serde_json::json!({})
        } else {
            serde_json::from_str(args).map_err(|e| format!("invalid args: {e}"))?
        };
        let get_str = |name: &str| {
            value
                .get(name)
                .and_then(|v| v.as_str())
                .ok_or_else(|| format!("missing {name}"))
        };
        let get_i32 = |name: &str| {
            value
                .get(name)
                .and_then(|v| v.as_i64())
                .and_then(|v| i32::try_from(v).ok())
                .ok_or_else(|| format!("missing or invalid {name}"))
        };

        match method {
            "state.query" => {
                let query = get_str("method")?;
                let params = value
                    .get("params")
                    .and_then(|v| v.as_array())
                    .cloned()
                    .unwrap_or_default();
                state_call(svc, query, &params)
            }
            "send.to_user" => {
                let uid = get_i32("user_id")?;
                let message = get_str("message")?.to_string();
                let callback = svc
                    .send_chat
                    .read()
                    .map_err(|e| format!("lock error: {e}"))?;
                callback.as_ref().ok_or("send_chat not available")?(uid, message);
                Ok("ok".to_string())
            }
            "send.to_room" => state_call(
                svc,
                "send_room_chat",
                &[
                    serde_json::json!(get_str("room_id")?),
                    serde_json::json!(get_str("message")?),
                ],
            ),
            "send.to_all" => {
                let message = get_str("message")?.to_string();
                let callback = svc
                    .send_chat
                    .read()
                    .map_err(|e| format!("lock error: {e}"))?;
                callback.as_ref().ok_or("send_chat not available")?(0, message);
                Ok("ok".to_string())
            }
            "ext.get_user" => {
                let uid = get_i32("user_id")?;
                let key = get_str("key")?;
                let store = svc.extensions.store();
                let guard = store
                    .try_read()
                    .map_err(|_| "extension store busy".to_string())?;
                guard
                    .get_user_extra(uid, key)
                    .cloned()
                    .ok_or_else(|| format!("field '{key}' not found for user {uid}"))
            }
            "ext.set_user" => {
                let uid = get_i32("user_id")?;
                let key = get_str("key")?;
                let content = get_str("value")?;
                let store = svc.extensions.store();
                store
                    .try_write()
                    .map_err(|_| "extension store busy".to_string())?
                    .set_user_extra(uid, key, content.to_string())
                    .map_err(|e| format!("set_user_extra: {e}"))?;
                if let Some(db) = crate::internal_hooks::DB.get() {
                    db.record_room_event_sync(
                        "extensions.user.set",
                        None,
                        Some(uid),
                        serde_json::json!({
                            "user_id": uid,
                            "key": key,
                            "value": content,
                            "source": "wasm_host",
                        }),
                    );
                }
                Ok("ok".to_string())
            }
            "ext.get_room" => {
                let room_id = get_str("room_id")?;
                let key = get_str("key")?;
                let store = svc.extensions.store();
                let guard = store
                    .try_read()
                    .map_err(|_| "extension store busy".to_string())?;
                guard
                    .get_room_extra(room_id, key)
                    .cloned()
                    .ok_or_else(|| format!("field '{key}' not found for room {room_id}"))
            }
            "ext.set_room" => {
                let room_id = get_str("room_id")?;
                let key = get_str("key")?;
                let content = get_str("value")?;
                let store = svc.extensions.store();
                store
                    .try_write()
                    .map_err(|_| "extension store busy".to_string())?
                    .set_room_extra(room_id, key, content.to_string())
                    .map_err(|e| format!("set_room_extra: {e}"))?;
                if let Some(db) = crate::internal_hooks::DB.get() {
                    db.record_room_event_sync(
                        "extensions.room.set",
                        Some(room_id.to_string()),
                        None,
                        serde_json::json!({
                            "room_id": room_id,
                            "key": key,
                            "value": content,
                            "source": "wasm_host",
                        }),
                    );
                }
                Ok("ok".to_string())
            }
            "config.get" => {
                let key = get_str("key")?;
                helpers::validate_config_key(key)?;
                ensure_config_loaded(svc, plugin_name)?;
                svc.plugin_configs
                    .read()
                    .map_err(|e| format!("lock error: {e}"))?
                    .get(plugin_name)
                    .and_then(|cfg| cfg.get(key).cloned())
                    .ok_or_else(|| format!("config '{key}' not found"))
            }
            "config.set" => {
                let key = get_str("key")?;
                let content = get_str("value")?;
                helpers::validate_config_key(key)?;
                if content.len() > svc.runtime.max_file_bytes {
                    return Err("config value exceeds plugin limit".to_string());
                }
                ensure_config_loaded(svc, plugin_name)?;
                let snapshot = {
                    let mut configs = svc
                        .plugin_configs
                        .write()
                        .map_err(|e| format!("lock error: {e}"))?;
                    let config = configs.entry(plugin_name.to_string()).or_default();
                    config.insert(key.to_string(), content.to_string());
                    config.clone()
                };
                persist_plugin_config(plugin_name, &snapshot)?;
                if let Some(db) = crate::internal_hooks::DB.get() {
                    db.record_room_event_sync(
                        "plugin.config.set",
                        None,
                        None,
                        serde_json::json!({
                            "plugin": plugin_name,
                            "key": key,
                            "value": content,
                        }),
                    );
                }
                Ok("ok".to_string())
            }
            "http.get" => {
                let url = get_str("url")?;
                helpers::validate_http_url(url, svc.runtime.allow_private_network)?;
                let response = svc
                    .http_client
                    .get(url)
                    .send()
                    .map_err(|e| format!("HTTP GET failed: {e}"))?;
                read_limited_response(response, svc.runtime.max_http_response_bytes)
            }
            "http.post" => {
                let url = get_str("url")?;
                let body = value.get("body").and_then(|v| v.as_str()).unwrap_or("");
                let content_type = value
                    .get("content_type")
                    .and_then(|v| v.as_str())
                    .unwrap_or("application/json");
                if body.len() > MAX_HOST_INPUT_BYTES {
                    return Err("HTTP request body exceeds host limit".to_string());
                }
                helpers::validate_http_url(url, svc.runtime.allow_private_network)?;
                let response = svc
                    .http_client
                    .post(url)
                    .header("Content-Type", content_type)
                    .body(body.to_string())
                    .send()
                    .map_err(|e| format!("HTTP POST failed: {e}"))?;
                read_limited_response(response, svc.runtime.max_http_response_bytes)
            }
            "file.read" => {
                let path = plugin_data_path(plugin_name, get_str("path")?, false)?;
                let metadata =
                    std::fs::metadata(&path).map_err(|e| format!("file metadata failed: {e}"))?;
                if metadata.len() > svc.runtime.max_file_bytes as u64 {
                    return Err("file exceeds plugin read limit".to_string());
                }
                let bytes = std::fs::read(path).map_err(|e| format!("file read failed: {e}"))?;
                String::from_utf8(bytes).map_err(|e| format!("file is not UTF-8: {e}"))
            }
            "file.write" => {
                let content = get_str("content")?;
                if content.len() > svc.runtime.max_file_bytes {
                    return Err("file exceeds plugin write limit".to_string());
                }
                let path = plugin_data_path(plugin_name, get_str("path")?, true)?;
                helpers::atomic_write(&path, content.as_bytes())?;
                Ok("ok".to_string())
            }
            "uuid.v4" => Ok(uuid::Uuid::new_v4().to_string()),
            "time.now" => Ok(std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs_f64())
                .unwrap_or(0.0)
                .to_string()),
            "player.touches" => state_call(
                svc,
                "player.touches",
                &[serde_json::json!(get_i32("user_id")?)],
            ),
            "player.judges" => state_call(
                svc,
                "player.judges",
                &[serde_json::json!(get_i32("user_id")?)],
            ),
            "round.data" => state_call(
                svc,
                "round.data",
                &[
                    serde_json::json!(get_str("round_uuid")?),
                    serde_json::json!(get_i32("player_id")?),
                ],
            ),
            "round.list" => state_call(svc, "round.list", &[]),
            "room.create_empty" => {
                let endpoint = value
                    .get("endpoint")
                    .or_else(|| value.get("phira_api_endpoint"))
                    .cloned()
                    .unwrap_or(serde_json::Value::Null);
                state_call(
                    svc,
                    "room.create_empty",
                    &[serde_json::json!(get_str("room_id")?), endpoint],
                )
            }
            "room.kick" => state_call(
                svc,
                "room.kick",
                &[
                    serde_json::json!(get_str("room_id")?),
                    serde_json::json!(get_i32("target_id")?),
                ],
            ),
            "room.set_host" => {
                let target = value
                    .get("target_id")
                    .or_else(|| value.get("host_id"))
                    .or_else(|| value.get("target"))
                    .cloned()
                    .unwrap_or(serde_json::Value::Null);
                state_call(
                    svc,
                    "room.set_host",
                    &[serde_json::json!(get_str("room_id")?), target],
                )
            }
            "room.clear_host" => state_call(
                svc,
                "room.set_host",
                &[
                    serde_json::json!(get_str("room_id")?),
                    serde_json::Value::Null,
                ],
            ),
            "room.set_lock" => state_call(
                svc,
                "room.set_lock",
                &[
                    serde_json::json!(get_str("room_id")?),
                    serde_json::json!(value
                        .get("locked")
                        .and_then(|v| v.as_bool())
                        .ok_or("missing locked")?),
                ],
            ),
            "room.force_move" => state_call(
                svc,
                "room.force_move",
                &[
                    serde_json::json!(get_str("room_id")?),
                    serde_json::json!(get_i32("target_id")?),
                    serde_json::json!(value
                        .get("monitor")
                        .and_then(|v| v.as_bool())
                        .unwrap_or(false)),
                ],
            ),
            "room.set_hidden" => state_call(
                svc,
                "room.set_hidden",
                &[
                    serde_json::json!(get_str("room_id")?),
                    serde_json::json!(value
                        .get("hidden")
                        .and_then(|v| v.as_bool())
                        .ok_or("missing hidden")?),
                ],
            ),
            "room.is_hidden" => state_call(
                svc,
                "room.is_hidden",
                &[serde_json::json!(get_str("room_id")?)],
            ),
            "room.set_persistent_empty" => state_call(
                svc,
                "room.set_persistent_empty",
                &[
                    serde_json::json!(get_str("room_id")?),
                    serde_json::json!(value
                        .get("persistent")
                        .or_else(|| value.get("persistent_empty"))
                        .and_then(|v| v.as_bool())
                        .ok_or("missing persistent")?),
                ],
            ),
            "room.get_phira_api_endpoint" => state_call(
                svc,
                "room.get_phira_api_endpoint",
                &[serde_json::json!(get_str("room_id")?)],
            ),
            "room.set_phira_api_endpoint" => {
                let endpoint = value
                    .get("endpoint")
                    .or_else(|| value.get("phira_api_endpoint"))
                    .cloned()
                    .unwrap_or(serde_json::Value::Null);
                state_call(
                    svc,
                    "room.set_phira_api_endpoint",
                    &[serde_json::json!(get_str("room_id")?), endpoint],
                )
            }
            "room.clear_phira_api_endpoint" => state_call(
                svc,
                "room.set_phira_api_endpoint",
                &[
                    serde_json::json!(get_str("room_id")?),
                    serde_json::Value::Null,
                ],
            ),
            "room.close" => {
                state_call(svc, "room.close", &[serde_json::json!(get_str("room_id")?)])
            }
            "room.uuid" => state_call(svc, "room.uuid", &[serde_json::json!(get_str("room_id")?)]),
            "room.history" => state_call(
                svc,
                "room.history",
                &[serde_json::json!(get_str("room_id")?)],
            ),
            "room.round_info" => state_call(
                svc,
                "room.round_info",
                &[serde_json::json!(get_str("round_uuid")?)],
            ),
            "room.list_since" => state_call(
                svc,
                "room.list_since",
                &[serde_json::json!(value
                    .get("since_ms")
                    .and_then(|v| v.as_i64())
                    .unwrap_or(0))],
            ),
            "admin.kick_user" => state_call(
                svc,
                "admin.kick_user",
                &[
                    serde_json::json!(get_i32("user_id")?),
                    serde_json::json!(value
                        .get("reason")
                        .and_then(|v| v.as_str())
                        .unwrap_or("kicked by admin")),
                ],
            ),
            "admin.ban_user" => state_call(
                svc,
                "admin.ban_user",
                &[
                    serde_json::json!(get_i32("user_id")?),
                    serde_json::json!(value
                        .get("reason")
                        .and_then(|v| v.as_str())
                        .unwrap_or("banned")),
                ],
            ),
            "admin.unban_user" => state_call(
                svc,
                "admin.unban_user",
                &[serde_json::json!(get_i32("user_id")?)],
            ),
            "admin.is_banned" => state_call(
                svc,
                "admin.is_banned",
                &[serde_json::json!(get_i32("user_id")?)],
            ),
            "admin.ban_list" => state_call(svc, "admin.ban_list", &[]),
            "admin.list_users" => state_call(svc, "admin.list_users", &[]),
            "user.room_history" => state_call(
                svc,
                "user.room_history",
                &[serde_json::json!(get_i32("user_id")?)],
            ),

            "persist.events" => state_call(
                svc,
                "persist.events",
                &[
                    serde_json::json!(value
                        .get("since_sequence")
                        .or_else(|| value.get("since"))
                        .and_then(|v| v.as_i64())
                        .unwrap_or(0)),
                    serde_json::json!(value.get("limit").and_then(|v| v.as_i64()).unwrap_or(100)),
                    value
                        .get("kind")
                        .cloned()
                        .unwrap_or(serde_json::Value::Null),
                    value
                        .get("room_id")
                        .cloned()
                        .unwrap_or(serde_json::Value::Null),
                    value
                        .get("user_id")
                        .cloned()
                        .unwrap_or(serde_json::Value::Null),
                ],
            ),
            "persist.rooms" => state_call(
                svc,
                "persist.rooms",
                &[
                    serde_json::json!(value
                        .get("since_sequence")
                        .or_else(|| value.get("since"))
                        .and_then(|v| v.as_i64())
                        .unwrap_or(0)),
                    serde_json::json!(value.get("limit").and_then(|v| v.as_i64()).unwrap_or(100)),
                ],
            ),
            "persist.touches" => state_call(
                svc,
                "persist.touches",
                &[
                    serde_json::json!(value
                        .get("since_sequence")
                        .or_else(|| value.get("since"))
                        .and_then(|v| v.as_i64())
                        .unwrap_or(0)),
                    serde_json::json!(value.get("limit").and_then(|v| v.as_i64()).unwrap_or(100)),
                    value
                        .get("round_uuid")
                        .cloned()
                        .unwrap_or(serde_json::Value::Null),
                    value
                        .get("player_id")
                        .or_else(|| value.get("user_id"))
                        .cloned()
                        .unwrap_or(serde_json::Value::Null),
                ],
            ),
            "persist.judges" => state_call(
                svc,
                "persist.judges",
                &[
                    serde_json::json!(value
                        .get("since_sequence")
                        .or_else(|| value.get("since"))
                        .and_then(|v| v.as_i64())
                        .unwrap_or(0)),
                    serde_json::json!(value.get("limit").and_then(|v| v.as_i64()).unwrap_or(100)),
                    value
                        .get("round_uuid")
                        .cloned()
                        .unwrap_or(serde_json::Value::Null),
                    value
                        .get("player_id")
                        .or_else(|| value.get("user_id"))
                        .cloned()
                        .unwrap_or(serde_json::Value::Null),
                ],
            ),
            "persist.playtime" => state_call(
                svc,
                "persist.playtime",
                &[serde_json::json!(get_i32("user_id")?)],
            ),
            "persist.top_playtime" => state_call(
                svc,
                "persist.top_playtime",
                &[serde_json::json!(value
                    .get("limit")
                    .and_then(|v| v.as_i64())
                    .unwrap_or(10))],
            ),
            "admin.ids" => state_call(svc, "admin.ids", &[]),
            "admin.is_admin" => state_call(
                svc,
                "admin.is_admin",
                &[serde_json::json!(get_i32("user_id")?)],
            ),
            "admin.add_id" => state_call(
                svc,
                "admin.add_id",
                &[serde_json::json!(get_i32("user_id")?)],
            ),
            "admin.remove_id" => state_call(
                svc,
                "admin.remove_id",
                &[serde_json::json!(get_i32("user_id")?)],
            ),
            "admin.set_ids" => {
                let ids = value
                    .get("ids")
                    .or_else(|| value.get("admin_phira_ids"))
                    .cloned()
                    .unwrap_or(serde_json::Value::Array(Vec::new()));
                state_call(svc, "admin.set_ids", &[ids])
            }
            "plugin.api_register" => {
                let api_method = get_str("method")?;
                helpers::validate_identifier(api_method)?;
                let mut registrations = svc
                    .registered_apis
                    .lock()
                    .map_err(|e| format!("lock: {e}"))?;
                if !registrations
                    .entry(plugin_name.to_string())
                    .or_default()
                    .insert(api_method.to_string())
                {
                    return Err(format!("API '{api_method}' is already registered"));
                }
                Ok(format!("registered plugin API: {plugin_name}.{api_method}"))
            }
            "plugin.api_call" => {
                let target = get_str("plugin")?;
                let api_method = get_str("method")?;
                let api_args = value
                    .get("args")
                    .and_then(|v| v.as_array())
                    .cloned()
                    .unwrap_or_default();
                if target == plugin_name {
                    return Err("a plugin cannot synchronously call itself".to_string());
                }
                let key = format!("{target}:{api_method}");
                let native = {
                    let handlers = svc.api_handlers.lock().map_err(|e| format!("lock: {e}"))?;
                    handlers.get(&key).or_else(|| handlers.get(target)).cloned()
                };
                if let Some(handler) = native {
                    return handler(api_method, &api_args).map(|v| v.to_string());
                }
                let is_registered = svc
                    .registered_apis
                    .lock()
                    .map_err(|e| format!("lock: {e}"))?
                    .get(target)
                    .is_some_and(|methods| methods.contains(api_method));
                if !is_registered {
                    return Err(format!(
                        "plugin '{target}' has no registered API '{api_method}'"
                    ));
                }
                let runtime = svc
                    .plugin_runtimes
                    .lock()
                    .map_err(|e| format!("lock: {e}"))?
                    .get(target)
                    .and_then(Weak::upgrade)
                    .ok_or_else(|| format!("plugin '{target}' is unavailable"))?;
                // Retry try_lock in a short spin-loop so a briefly-busy target
                // does not immediately fail the caller.  Each iteration yields
                // the OS quantum so another thread holding the lock can advance.
                let mut plugin = None;
                for attempt in 0..5 {
                    if let Ok(guard) = runtime.try_lock() {
                        plugin = Some(guard);
                        break;
                    }
                    if attempt < 4 {
                        std::thread::sleep(std::time::Duration::from_millis(5));
                    }
                }
                let mut plugin = plugin.ok_or_else(|| {
                    format!(
                        "plugin '{target}' is busy (waited ~20ms for plugin.api_call from '{plugin_name}')"
                    )
                })?;
                plugin
                    .call_api(api_method, &api_args)
                    .map(|v| v.to_string())
            }
            _ => Err(format!("unknown API method: {method}")),
        }
    }
}

impl Drop for WasmPluginInstance {
    fn drop(&mut self) {
        if self.initialized {
            self.call_cleanup();
        }
    }
}

// ── 辅助函数 ──

use crate::wasm_host_helpers as helpers;

fn state_call(
    svc: &WasmPluginServices,
    method: &str,
    args: &[serde_json::Value],
) -> Result<String, String> {
    let guard = svc
        .state_query
        .read()
        .map_err(|e| format!("lock error: {e}"))?;
    guard
        .as_ref()
        .ok_or_else(|| "state query not available".to_string())?
        .call(method, args)
        .map(|value| value.to_string())
}

fn ensure_config_loaded(svc: &WasmPluginServices, plugin: &str) -> Result<(), String> {
    if svc
        .plugin_configs
        .read()
        .map_err(|e| format!("lock error: {e}"))?
        .contains_key(plugin)
    {
        return Ok(());
    }
    let path = helpers::config_path(plugin);
    let config = if path.exists() {
        let bytes = std::fs::read(&path).map_err(|e| format!("read config: {e}"))?;
        if bytes.len() > svc.runtime.max_file_bytes {
            return Err("config file exceeds plugin limit".to_string());
        }
        serde_json::from_slice::<HashMap<String, String>>(&bytes)
            .map_err(|e| format!("invalid config file: {e}"))?
    } else {
        HashMap::new()
    };
    svc.plugin_configs
        .write()
        .map_err(|e| format!("lock error: {e}"))?
        .entry(plugin.to_string())
        .or_insert(config);
    Ok(())
}

fn persist_plugin_config(plugin: &str, config: &HashMap<String, String>) -> Result<(), String> {
    let bytes = serde_json::to_vec_pretty(config).map_err(|e| format!("encode config: {e}"))?;
    helpers::atomic_write(&helpers::config_path(plugin), &bytes)
}


fn plugin_data_path(plugin: &str, relative: &str, create_parent: bool) -> Result<PathBuf, String> {
    helpers::validate_identifier(plugin)?;
    let relative = Path::new(relative);
    if relative.as_os_str().is_empty() || relative.is_absolute() {
        return Err("plugin paths must be non-empty and relative".to_string());
    }
    if relative
        .components()
        .any(|part| !matches!(part, Component::Normal(_)))
    {
        return Err("plugin path traversal is not allowed".to_string());
    }
    let root = Path::new("data/plugins").join(plugin);
    std::fs::create_dir_all(&root).map_err(|e| format!("create plugin data directory: {e}"))?;
    helpers::reject_symlink_components(&root)?;
    let target = root.join(relative);
    if create_parent {
        let parent = target.parent().ok_or("path has no parent")?;
        std::fs::create_dir_all(parent)
            .map_err(|e| format!("create plugin data directory: {e}"))?;
        helpers::reject_symlink_components(parent)?;
        if target.exists()
            && std::fs::symlink_metadata(&target)
                .map_err(|e| e.to_string())?
                .file_type()
                .is_symlink()
        {
            return Err("symbolic links are not allowed in plugin storage".to_string());
        }
    } else {
        let canonical_root =
            std::fs::canonicalize(&root).map_err(|e| format!("canonicalize plugin root: {e}"))?;
        let canonical_target =
            std::fs::canonicalize(&target).map_err(|e| format!("canonicalize plugin file: {e}"))?;
        if !canonical_target.starts_with(canonical_root) {
            return Err("plugin path escaped its data directory".to_string());
        }
    }
    Ok(target)
}



fn is_private_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(ip) => {
            ip.is_private()
                || ip.is_loopback()
                || ip.is_link_local()
                || ip.is_broadcast()
                || ip.is_documentation()
                || ip.is_unspecified()
                || ip.is_multicast()
        }
        IpAddr::V6(ip) => {
            ip.is_loopback()
                || ip.is_unspecified()
                || ip.is_multicast()
                || ip.is_unique_local()
                || ip.is_unicast_link_local()
        }
    }
}

fn read_limited_response(
    mut response: reqwest::blocking::Response,
    limit: usize,
) -> Result<String, String> {
    if !response.status().is_success() {
        return Err(format!("HTTP request returned {}", response.status()));
    }
    if response
        .content_length()
        .is_some_and(|length| length > limit as u64)
    {
        return Err("HTTP response exceeds plugin limit".to_string());
    }
    let mut bytes = Vec::new();
    response
        .by_ref()
        .take(limit.saturating_add(1) as u64)
        .read_to_end(&mut bytes)
        .map_err(|e| format!("read HTTP response: {e}"))?;
    if bytes.len() > limit {
        return Err("HTTP response exceeds plugin limit".to_string());
    }
    String::from_utf8(bytes).map_err(|e| format!("HTTP response is not UTF-8: {e}"))
}

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

    #[test]
    fn private_networks_are_detected() {
        assert!(is_private_ip("127.0.0.1".parse().unwrap()));
        assert!(is_private_ip("10.0.0.1".parse().unwrap()));
        assert!(is_private_ip("::1".parse().unwrap()));
        assert!(!is_private_ip("8.8.8.8".parse().unwrap()));
    }

    #[test]
    fn plugin_paths_reject_traversal_before_io() {
        assert!(plugin_data_path("test-plugin", "../secret", false).is_err());
        assert!(plugin_data_path("test-plugin", "/absolute", false).is_err());
    }
}
