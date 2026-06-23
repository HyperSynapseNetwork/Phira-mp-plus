//! WASM 插件宿主运行时
//!
//! 基于 wasmtime 的简化 WASM 运行时，采用 JSON ABI 接口：
//!
//! - WASM 插件导出以下函数（通过 wasmtime::Func 调用）：
//!   - `phira_init() -> i32` (0=success)
//!   - `phira_get_info() -> i32` 填写 info 指针
//!   - `phira_cleanup()`
//!   - `phira_on_event(event_json_ptr: i32) -> i32` (0=handled, 1=unhandled)
//!   - `phira_alloc(size: i32) -> i32` 分配内存
//!   - `phira_dealloc(ptr: i32, size: i32)`
//!
//! - 宿主（服务器）提供以下导入函数：
//!   - `phira:host/log(level_ptr, level_len, msg_ptr, msg_len)`  — 记录日志
//!   - `phira:host/uuid(out_ptr, out_len)`                  — 生成 UUID
//!   - `phira:host/time() -> i64`                            — 获取时间戳(ms)
//!   - `phira:host/api(method_ptr,method_len,args_ptr,args_len,out_ptr,out_len) -> i32`
//!      通用 API 桥接，method + args (JSON) → result (JSON)
//!
//!  `phira:host/api` 支持的方法（与原生插件 PluginContext API 完全对应）：
//!    state.query   — 查询房间/用户/服务器状态 (对应 ctx.state.call())
//!    send.to_user  — 发送消息给指定用户 (对应 ctx.send_chat())
//!    send.to_room  — 向房间广播消息
//!    send.to_all   — 向所有用户广播
//!    ext.get/set   — 读写用户/房间扩展数据
//!    config.get/set— 读写插件配置
//!    http.get/post — 发送 HTTP 请求
//!    file.read/write— 读写插件数据文件
//!    uuid.v4       — 生成 UUID v4
//!    time.now      — 获取 Unix 时间戳
//!
//! 所有字符串/数据交换通过 WASM 线性内存中的 JSON 完成。
//! 插件通过 `phira_alloc` 分配内存，宿主写入结果。

use crate::extensions::ExtensionManager;
use crate::plugin::{CliCommand, PluginEvent, PluginInfo};
use phira_mp_plus_server_api as api;
use wasmtime::AsContext;
use std::collections::HashMap;
use std::sync::Arc;
use tracing::{error, info, warn};

// ── 共享宿主服务 ──

/// WASM 插件的共享服务（所有插件共用）
///
/// 内部使用 std::sync::RwLock（非异步），因为 WASM 宿主函数
/// 在同步上下文中被调用。
pub struct WasmPluginServices {
    pub extensions: Arc<ExtensionManager>,
    pub state_query: std::sync::RwLock<Option<api::ServerStateQuery>>,
    pub send_chat: std::sync::RwLock<Option<Arc<dyn Fn(i32, String) + Send + Sync>>>,
    pub cli_commands: std::sync::Mutex<HashMap<String, CliCommand>>,
    pub plugin_configs: std::sync::RwLock<HashMap<String, HashMap<String, String>>>,
    pub http_handle: std::sync::RwLock<Option<api::HttpHandle>>,
    pub api_handlers: std::sync::Mutex<HashMap<String, api::PluginApiHandler>>,
}

impl WasmPluginServices {
    pub fn new(extensions: Arc<ExtensionManager>) -> Self {
        Self {
            extensions,
            state_query: std::sync::RwLock::new(None),
            send_chat: std::sync::RwLock::new(None),
            cli_commands: std::sync::Mutex::new(HashMap::new()),
            plugin_configs: std::sync::RwLock::new(HashMap::new()),
            http_handle: std::sync::RwLock::new(None),
            api_handlers: std::sync::Mutex::new(HashMap::new()),
        }
    }
}

// ── WASM 插件实例 ──

/// WASM 插件在 wasmtime 中的导出函数表
pub struct WasmPluginInstance {
    #[allow(dead_code)]
    pub engine: wasmtime::Engine,
    instance: wasmtime::Instance,
    store: wasmtime::Store<()>,

    /// 共享宿主服务（用于 host/api 等调用）
    #[allow(dead_code)]
    services: Arc<WasmPluginServices>,

    // 导出的函数
    func_init: Option<wasmtime::TypedFunc<(), i32>>,
    #[allow(dead_code)]
    func_get_info: Option<wasmtime::TypedFunc<(), ()>>,
    func_cleanup: Option<wasmtime::TypedFunc<(), ()>>,
    func_on_event: Option<wasmtime::TypedFunc<(i32, i32), i32>>,
    func_alloc: Option<wasmtime::TypedFunc<i32, i32>>,

    // 插件元数据
    pub info: PluginInfo,
    pub plugin_name: String,
    pub plugin_path: String,
    pub initialized: bool,

    // 信息缓存（JSON 格式）
    #[allow(dead_code)]
    info_json_ptr: Option<i32>,
    #[allow(dead_code)]
    info_json_len: i32,
    #[allow(dead_code)]
    plugin_data_dir: String,
}

unsafe impl Send for WasmPluginInstance {}
unsafe impl Sync for WasmPluginInstance {}

impl WasmPluginInstance {
    pub fn new(
        wasm_bytes: &[u8],
        plugin_path: &str,
        services: Arc<WasmPluginServices>,
    ) -> Result<Self, String> {
        let plugin_name = std::path::Path::new(plugin_path)
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("unknown")
            .to_string();

        // 创建引擎
        let engine_config = wasmtime::Config::new();
        let engine = wasmtime::Engine::new(&engine_config)
            .map_err(|e| format!("engine creation: {}", e))?;

        // 创建模块
        let module = wasmtime::Module::new(&engine, wasm_bytes)
            .map_err(|e| format!("module compile error: {}", e))?;

        // 创建链接器并注册宿主函数
        let mut linker = wasmtime::Linker::new(&engine);

        let svc = Arc::clone(&services);

        // 注册日志宿主函数
        linker
            .func_wrap(
                "phira",
                "host/log",
                |mut caller: wasmtime::Caller<'_, ()>, level_ptr: i32, level_len: i32, msg_ptr: i32, msg_len: i32| {
                    let memory = match caller.get_export("memory").and_then(|e| e.into_memory()) {
                        Some(m) => m,
                        None => { warn!("WASM plugin has no memory export"); return; }
                    };
                    let level = read_str_from_memory(&memory, caller.as_context(), level_ptr, level_len);
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
                |mut caller: wasmtime::Caller<'_, ()>, out_ptr: i32, out_len: i32| {
                    let memory = match caller.get_export("memory").and_then(|e| e.into_memory()) {
                        Some(m) => m,
                        None => return,
                    };
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
            .func_wrap(
                "phira",
                "host/time",
                || -> i64 {
                    std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .map(|d| d.as_millis() as i64)
                        .unwrap_or(0)
                },
            )
            .map_err(|e| format!("link time: {}", e))?;

        // 注册通用 API 调用宿主函数：
        //   phira:host/api(method_ptr, method_len, args_ptr, args_len, out_ptr, out_len) -> i32
        //   method: JSON string — 方法名（如 "state.query", "send.to_user", "ext.get_user", "http.get"）
        //   args:   JSON string — 参数
        //   out:    输出缓冲区指针（由插件分配）
        //   returns: 0 = 成功，数据写入 out；非0 = 错误码
        //   输出格式: 写入内存为 [len:i32][json_bytes]
        linker
            .func_wrap(
                "phira",
                "host/api",
                {
                    let svc = Arc::clone(&svc);
                    let pn = plugin_name.clone();
                    move |mut caller: wasmtime::Caller<'_, ()>,
                          method_ptr: i32, method_len: i32,
                          args_ptr: i32, args_len: i32,
                          out_ptr: i32, out_len: i32| -> i32 {
                        let memory = match caller.get_export("memory").and_then(|e| e.into_memory()) {
                            Some(m) => m,
                            None => { warn!("WASM plugin has no memory"); return -1; }
                        };
                        let method = match read_str_from_memory(&memory, caller.as_context(), method_ptr, method_len) {
                            Some(s) => s,
                            None => return -2,
                        };
                        let args_str = read_str_from_memory(&memory, caller.as_context(), args_ptr, args_len)
                            .unwrap_or_default();

                        let result = Self::dispatch_api(&svc, &pn, &method, &args_str);

                        match result {
                            Ok(json) => {
                                let bytes = json.as_bytes();
                                let data_len = bytes.len();
                                let total_len = data_len + 4;
                                if total_len as i32 > out_len {
                                    warn!("WASM api output buffer too small: need {}, have {}", total_len, out_len);
                                    return -3;
                                }
                                let len_prefix = (data_len as i32).to_le_bytes();
                                if memory.write(&mut caller, out_ptr as usize, &len_prefix).is_err() {
                                    return -4;
                                }
                                if memory.write(&mut caller, (out_ptr + 4) as usize, bytes).is_err() {
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
                                    let _ = memory.write(&mut caller, (out_ptr + 4) as usize, err_bytes);
                                }
                                -1
                            }
                        }
                    }
                },
            )
            .map_err(|e| format!("link api: {}", e))?;

        // 创建 store 和实例
        let mut store = wasmtime::Store::new(&engine, ());
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
        let func_alloc = instance
            .get_typed_func::<i32, i32>(&mut store, "phira_alloc")
            .ok();
        let _func_dealloc = instance
            .get_typed_func::<(i32, i32), ()>(&mut store, "phira_dealloc")
            .ok();

        let pn_dir = plugin_name.clone();
        let mut plugin = Self {
            engine,
            instance,
            store,
            services: Arc::clone(&svc),
            func_init,
            func_get_info,
            func_cleanup,
            func_on_event,
            func_alloc,
            info: PluginInfo {
                name: pn_dir.clone(),
                version: "0.1.0".to_string(),
                author: "unknown".to_string(),
                description: format!("WASM plugin from {}", plugin_path),
            },
            plugin_name: pn_dir.clone(),
            plugin_path: plugin_path.to_string(),
            initialized: false,
            info_json_ptr: None,
            info_json_len: 0,
            plugin_data_dir: format!("data/plugins/{}", pn_dir),
        };

        // 尝试从 WASM 内存读取插件信息
        plugin.read_plugin_info();

        Ok(plugin)
    }

    /// 从 WASM 线性内存读取插件元数据
    ///
    /// 优先通过 phira_get_info 导出函数获取，
    /// 回退到从偏移 0 读取长度前缀的 JSON。
    fn read_plugin_info(&mut self) {
        // 方案一：如果插件导出 phira_get_info，先调用它以填充元数据
        let called_get_info = if let Some(ref get_info) = self.func_get_info {
            get_info.call(&mut self.store, ()).is_ok()
        } else {
            false
        };

        // 方案二：从内存偏移 0 读取长度前缀的 JSON 元数据（兼容模式）
        if let Some(memory) = self.instance.get_memory(&mut self.store, "memory") {
            let mut header = [0u8; 4];
            if memory.read(&self.store, 0, &mut header).is_ok() {
                let len = i32::from_le_bytes(header) as usize;
                if len > 0 && len < 65536 {
                    let mut buf = vec![0u8; len];
                    if memory.read(&self.store, 4, &mut buf).is_ok() {
                        if let Ok(json) = String::from_utf8(buf) {
                            if let Ok(info) = serde_json::from_str::<serde_json::Value>(&json) {
                                if let Some(name) = info.get("name").and_then(|v| v.as_str()) {
                                    self.info.name = name.to_string();
                                }
                                if let Some(version) = info.get("version").and_then(|v| v.as_str()) {
                                    self.info.version = version.to_string();
                                }
                                if let Some(author) = info.get("author").and_then(|v| v.as_str()) {
                                    self.info.author = author.to_string();
                                }
                                if let Some(desc) = info.get("description").and_then(|v| v.as_str()) {
                                    self.info.description = desc.to_string();
                                }
                            }
                        }
                    }
                }
            }
        }

        if called_get_info {
            info!("WASM plugin '{}' metadata read via phira_get_info", self.plugin_name);
        }
    }

    /// 调用插件 init
    pub fn call_init(&mut self) -> Result<(), String> {
        if let Some(ref init) = self.func_init {
            match init.call(&mut self.store, ()) {
                Ok(0) => {
                    self.initialized = true;
                    info!("WASM plugin '{}' initialized", self.plugin_name);
                    Ok(())
                }
                Ok(code) => {
                    let msg = format!("WASM plugin '{}' init returned error code {}", self.plugin_name, code);
                    error!("{}", msg);
                    Err(msg)
                }
                Err(trap) => {
                    let msg = format!("WASM plugin '{}' init trap: {}", self.plugin_name, trap);
                    error!("{}", msg);
                    Err(msg)
                }
            }
        } else {
            // 没有 phira_init 导出，可能是个纯数据处理插件
            self.initialized = true;
            Ok(())
        }
    }

    /// 调用插件 cleanup
    pub fn call_cleanup(&mut self) {
        if !self.initialized {
            return;
        }
        if let Some(ref cleanup) = self.func_cleanup {
            if let Err(e) = cleanup.call(&mut self.store, ()) {
                warn!("plugin '{}' cleanup error: {}", self.plugin_name, e);
            }
        }
        self.initialized = false;
        info!("WASM plugin '{}' cleaned up", self.plugin_name);
    }

    /// 获取事件对应的 JSON 字符串
    fn event_to_json(event: &PluginEvent) -> String {
        match event {
            PluginEvent::UserConnect { user_id, user_name, user_ip } => {
                serde_json::json!({"type": "user_connect", "user_id": user_id, "user_name": user_name, "user_ip": user_ip}).to_string()
            }
            PluginEvent::UserDisconnect { user_id, user_name } => {
                serde_json::json!({"type": "user_disconnect", "user_id": user_id, "user_name": user_name}).to_string()
            }
            PluginEvent::RoomCreate { user_id, room_id } => {
                serde_json::json!({"type": "room_create", "user_id": user_id, "room_id": room_id}).to_string()
            }
            PluginEvent::RoomJoin { user_id, room_id, is_monitor } => {
                serde_json::json!({"type": "room_join", "user_id": user_id, "room_id": room_id, "is_monitor": is_monitor}).to_string()
            }
            PluginEvent::RoomLeave { user_id, room_id } => {
                serde_json::json!({"type": "room_leave", "user_id": user_id, "room_id": room_id}).to_string()
            }
            PluginEvent::RoomModify { user_id, room_id, data } => {
                serde_json::json!({"type": "room_modify", "user_id": user_id, "room_id": room_id, "data": data}).to_string()
            }
            PluginEvent::GameStart { user_id, room_id } => {
                serde_json::json!({"type": "game_start", "user_id": user_id, "room_id": room_id}).to_string()
            }
            PluginEvent::GameEnd { user_id, user_name, room_id, score, accuracy, perfect, good, bad, miss, max_combo, full_combo } => {
                serde_json::json!({"type": "game_end", "user_id": user_id, "user_name": user_name, "room_id": room_id, "score": score, "accuracy": accuracy, "perfect": perfect, "good": good, "bad": bad, "miss": miss, "max_combo": max_combo, "full_combo": full_combo}).to_string()
            }
            PluginEvent::PlayerTouches { user_id, room_id, data } => {
                serde_json::json!({"type": "player_touches", "user_id": user_id, "room_id": room_id, "data": data}).to_string()
            }
            PluginEvent::PlayerJudges { user_id, room_id, data } => {
                serde_json::json!({"type": "player_judges", "user_id": user_id, "room_id": room_id, "data": data}).to_string()
            }
            PluginEvent::RoundComplete { room_id, chart_id, chart_name } => {
                serde_json::json!({"type": "round_complete", "room_id": room_id, "chart_id": chart_id, "chart_name": chart_name}).to_string()
            }
        }
    }

    /// 将事件 JSON 写入 WASM 内存并调用 phira_on_event
    pub fn call_on_event(&mut self, event: &PluginEvent) -> i32 {
        let json = Self::event_to_json(event);
        let bytes = json.as_bytes();
        let len = bytes.len() as i32;

        match self.write_to_wasm(bytes) {
            Some((ptr, _)) => {
                if let Some(ref on_event) = self.func_on_event {
                    match on_event.call(&mut self.store, (ptr, len)) {
                        Ok(code) => code,
                        Err(e) => {
                            warn!("plugin '{}' on_event error: {}", self.plugin_name, e);
                            -1
                        }
                    }
                } else {
                    1 // 插件未实现事件处理
                }
            }
            None => -2, // 内存分配失败
        }
    }

    /// 将数据写入 WASM 线性内存
    fn write_to_wasm(&mut self, data: &[u8]) -> Option<(i32, i32)> {
        let len = data.len() as i32;
        let ptr = self.alloc(len)?;
        if let Some(ref memory) = self.instance.get_memory(&mut self.store, "memory") {
            memory
                .write(&mut self.store, ptr as usize, data)
                .ok()?;
        }
        Some((ptr, len))
    }

    /// 在 WASM 线性内存中分配空间
    fn alloc(&mut self, size: i32) -> Option<i32> {
        self.func_alloc
            .as_ref()
            .and_then(|alloc| alloc.call(&mut self.store, size).ok())
    }

    /// 通用 API 分发：处理 WASM 插件的 `phira:host/api` 调用
    ///
    /// 支持的 method:
    /// - `state.query` → 调用 ServerStateQuery
    /// - `send.to_user` → 发送聊天消息给指定用户
    /// - `send.to_room` → 向房间广播消息
    /// - `send.to_all`  → 向所有用户广播
    /// - `ext.get_user` → 获取用户扩展数据
    /// - `ext.set_user` → 设置用户扩展数据
    /// - `ext.get_room` → 获取房间扩展数据
    /// - `ext.set_room` → 设置房间扩展数据
    /// - `config.get`   → 获取插件配置
    /// - `config.set`   → 设置插件配置
    /// - `http.get`     → 发送 HTTP GET 请求
    /// - `http.post`    → 发送 HTTP POST 请求
    /// - `file.read`    → 读取插件数据目录中的文件
    /// - `file.write`   → 写入插件数据目录中的文件
    /// - `uuid.v4`      → 生成 UUID v4
    /// - `time.now`     → 获取当前时间（ISO 8601）
    /// - `player.touches` → 查询指定用户的最近触控数据
    /// - `player.judges`  → 查询指定用户的最近判定数据
    fn dispatch_api(svc: &WasmPluginServices, plugin_name: &str, method: &str, args: &str) -> Result<String, String> {
        let (method_name, rest) = method.split_once('.').unwrap_or((method, ""));
        match (method_name, rest) {
            ("state", "query") => {
                let args_val: serde_json::Value = serde_json::from_str(args)
                    .map_err(|e| format!("invalid state.query args: {}", e))?;
                let qmethod = args_val.get("method").and_then(|v| v.as_str()).ok_or("state.query: missing 'method' field")?;
                let qparams: Vec<serde_json::Value> = args_val.get("params")
                    .and_then(|v| v.as_array())
                    .map(|a| a.clone())
                    .unwrap_or_default();
                let guard = svc.state_query.read().map_err(|e| format!("lock error: {}", e))?;
                match guard.as_ref() {
                    Some(sq) => sq.call(qmethod, &qparams).map(|v| v.to_string()),
                    None => Err("server state query not available".to_string()),
                }
            }
            ("send", "to_user") => {
                let args_val: serde_json::Value = serde_json::from_str(args)
                    .map_err(|e| format!("invalid args: {}", e))?;
                let uid = args_val.get("user_id").and_then(|v| v.as_i64()).ok_or("missing user_id")? as i32;
                let msg = args_val.get("message").and_then(|v| v.as_str()).ok_or("missing message")?.to_string();
                let guard = svc.send_chat.read().map_err(|e| format!("lock error: {}", e))?;
                match guard.as_ref() {
                    Some(sc) => { sc(uid, msg); Ok("ok".to_string()) }
                    None => Err("send_chat not available".to_string()),
                }
            }
            ("send", "to_room") => {
                let args_val: serde_json::Value = serde_json::from_str(args)
                    .map_err(|e| format!("invalid args: {}", e))?;
                let room_id = args_val.get("room_id").and_then(|v| v.as_str()).ok_or("missing room_id")?;
                let msg = args_val.get("message").and_then(|v| v.as_str()).ok_or("missing message")?;
                let guard = svc.state_query.read().map_err(|e| format!("lock error: {}", e))?;
                match guard.as_ref() {
                    Some(sq) => sq.call("send_room_chat", &[serde_json::json!(room_id), serde_json::json!(msg)]).map(|v| v.to_string()),
                    None => Err("state query not available".to_string()),
                }
            }
            ("send", "to_all") => {
                let args_val: serde_json::Value = serde_json::from_str(args)
                    .map_err(|e| format!("invalid args: {}", e))?;
                let msg = args_val.get("message").and_then(|v| v.as_str()).ok_or("missing message")?;
                let guard = svc.send_chat.read().map_err(|e| format!("lock error: {}", e))?;
                match guard.as_ref() {
                    Some(sc) => { sc(0, msg.to_string()); Ok("ok".to_string()) }
                    None => Err("send_chat not available".to_string()),
                }
            }
            ("ext", "get_user") => {
                let args_val: serde_json::Value = serde_json::from_str(args)
                    .map_err(|e| format!("invalid args: {}", e))?;
                let uid = args_val.get("user_id").and_then(|v| v.as_i64()).ok_or("missing user_id")? as i32;
                let key = args_val.get("key").and_then(|v| v.as_str()).ok_or("missing key")?;
                let store = svc.extensions.store();
                let guard = store.try_read().map_err(|_| "extension store busy".to_string())?;
                guard.get_user_extra(uid, key).cloned()
                    .ok_or_else(|| format!("field '{}' not found for user {}", key, uid))
            }
            ("ext", "set_user") => {
                let args_val: serde_json::Value = serde_json::from_str(args)
                    .map_err(|e| format!("invalid args: {}", e))?;
                let uid = args_val.get("user_id").and_then(|v| v.as_i64()).ok_or("missing user_id")? as i32;
                let key = args_val.get("key").and_then(|v| v.as_str()).ok_or("missing key")?;
                let value = args_val.get("value").and_then(|v| v.as_str()).ok_or("missing value")?;
                let store = svc.extensions.store();
                let mut guard = store.try_write().map_err(|_| "extension store busy".to_string())?;
                guard.set_user_extra(uid, key, value.to_string())
                    .map_err(|e| format!("set_user_extra: {}", e))?;
                Ok("ok".to_string())
            }
            ("ext", "get_room") => {
                let args_val: serde_json::Value = serde_json::from_str(args)
                    .map_err(|e| format!("invalid args: {}", e))?;
                let room_id = args_val.get("room_id").and_then(|v| v.as_str()).ok_or("missing room_id")?;
                let key = args_val.get("key").and_then(|v| v.as_str()).ok_or("missing key")?;
                let store = svc.extensions.store();
                let guard = store.try_read().map_err(|_| "extension store busy".to_string())?;
                guard.get_room_extra(room_id, key).cloned()
                    .ok_or_else(|| format!("field '{}' not found for room {}", key, room_id))
            }
            ("ext", "set_room") => {
                let args_val: serde_json::Value = serde_json::from_str(args)
                    .map_err(|e| format!("invalid args: {}", e))?;
                let room_id = args_val.get("room_id").and_then(|v| v.as_str()).ok_or("missing room_id")?;
                let key = args_val.get("key").and_then(|v| v.as_str()).ok_or("missing key")?;
                let value = args_val.get("value").and_then(|v| v.as_str()).ok_or("missing value")?;
                let store = svc.extensions.store();
                let mut guard = store.try_write().map_err(|_| "extension store busy".to_string())?;
                guard.set_room_extra(room_id, key, value.to_string())
                    .map_err(|e| format!("set_room_extra: {}", e))?;
                Ok("ok".to_string())
            }
            ("config", "get") => {
                let args_val: serde_json::Value = serde_json::from_str(args)
                    .map_err(|e| format!("invalid args: {}", e))?;
                let key = args_val.get("key").and_then(|v| v.as_str()).ok_or("missing key")?;
                let configs = svc.plugin_configs.read().map_err(|e| format!("lock error: {}", e))?;
                configs
                    .get(plugin_name)
                    .and_then(|cfg| cfg.get(key).cloned())
                    .ok_or_else(|| format!("config '{}' not found", key))
            }
            ("config", "set") => {
                let args_val: serde_json::Value = serde_json::from_str(args)
                    .map_err(|e| format!("invalid args: {}", e))?;
                let key = args_val.get("key").and_then(|v| v.as_str()).ok_or("missing key")?;
                let value = args_val.get("value").and_then(|v| v.as_str()).ok_or("missing value")?;
                let mut configs = svc.plugin_configs.write().map_err(|e| format!("lock error: {}", e))?;
                configs.entry(plugin_name.to_string()).or_default().insert(key.to_string(), value.to_string());
                Ok("ok".to_string())
            }
            ("http", "get") => {
                let args_val: serde_json::Value = serde_json::from_str(args)
                    .map_err(|e| format!("invalid args: {}", e))?;
                let url = args_val.get("url").and_then(|v| v.as_str()).ok_or("missing url")?;
                let response = reqwest::blocking::get(url)
                    .map_err(|e| format!("HTTP GET failed: {}", e))?;
                response.text().map_err(|e| format!("HTTP GET read failed: {}", e))
            }
            ("http", "post") => {
                let args_val: serde_json::Value = serde_json::from_str(args)
                    .map_err(|e| format!("invalid args: {}", e))?;
                let url = args_val.get("url").and_then(|v| v.as_str()).ok_or("missing url")?;
                let body = args_val.get("body").and_then(|v| v.as_str()).unwrap_or("");
                let content_type = args_val.get("content_type").and_then(|v| v.as_str()).unwrap_or("application/json");
                let client = reqwest::blocking::Client::new();
                let response = client
                    .post(url)
                    .header("Content-Type", content_type)
                    .body(body.to_string())
                    .send()
                    .map_err(|e| format!("HTTP POST failed: {}", e))?;
                response.text().map_err(|e| format!("HTTP POST read failed: {}", e))
            }
            ("file", "read") => {
                let args_val: serde_json::Value = serde_json::from_str(args)
                    .map_err(|e| format!("invalid args: {}", e))?;
                let path = args_val.get("path").and_then(|v| v.as_str()).ok_or("missing path")?;
                let full_path = if path.starts_with('/') {
                    path.to_string()
                } else {
                    format!("data/plugins/{}/{}", plugin_name, path)
                };
                std::fs::read_to_string(&full_path)
                    .map_err(|e| format!("file read failed: {}", e))
            }
            ("file", "write") => {
                let args_val: serde_json::Value = serde_json::from_str(args)
                    .map_err(|e| format!("invalid args: {}", e))?;
                let path = args_val.get("path").and_then(|v| v.as_str()).ok_or("missing path")?;
                let content = args_val.get("content").and_then(|v| v.as_str()).ok_or("missing content")?;
                let full_path = if path.starts_with('/') {
                    path.to_string()
                } else {
                    format!("data/plugins/{}/{}", plugin_name, path)
                };
                if let Some(parent) = std::path::Path::new(&full_path).parent() {
                    std::fs::create_dir_all(parent)
                        .map_err(|e| format!("create dir failed: {}", e))?;
                }
                std::fs::write(&full_path, content)
                    .map_err(|e| format!("file write failed: {}", e))?;
                Ok("ok".to_string())
            }
            ("uuid", "v4") => {
                Ok(uuid::Uuid::new_v4().to_string())
            }
            ("time", "now") => {
                let now = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_secs_f64())
                    .unwrap_or(0.0);
                Ok(format!("{}", now)) // 返回 Unix 时间戳（秒）
            }
            ("player", "touches") => {
                // 查询指定用户的最近触控数据
                let args_val: serde_json::Value = serde_json::from_str(args)
                    .map_err(|e| format!("invalid args: {}", e))?;
                let uid = args_val.get("user_id").and_then(|v| v.as_i64()).ok_or("missing user_id")? as i32;
                let guard = svc.state_query.read().map_err(|e| format!("lock error: {}", e))?;
                match guard.as_ref() {
                    Some(sq) => sq.call("player.touches", &[serde_json::json!(uid)])
                        .map(|v| v.to_string()),
                    None => Err("state query not available".to_string()),
                }
            }
            ("player", "judges") => {
                // 查询指定用户的最近判定数据
                let args_val: serde_json::Value = serde_json::from_str(args)
                    .map_err(|e| format!("invalid args: {}", e))?;
                let uid = args_val.get("user_id").and_then(|v| v.as_i64()).ok_or("missing user_id")? as i32;
                let guard = svc.state_query.read().map_err(|e| format!("lock error: {}", e))?;
                match guard.as_ref() {
                    Some(sq) => sq.call("player.judges", &[serde_json::json!(uid)])
                        .map(|v| v.to_string()),
                    None => Err("state query not available".to_string()),
                }
            }
            _ => Err(format!("unknown API method: {}.{}", method_name, rest)),
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

/// 从 WASM 线性内存读取字符串
fn read_str_from_memory(
    memory: &wasmtime::Memory,
    ctx: impl wasmtime::AsContext,
    ptr: i32,
    len: i32,
) -> Option<String> {
    if len <= 0 || ptr < 0 {
        return None;
    }
    let mut buf = vec![0u8; len as usize];
    memory.read(ctx, ptr as usize, &mut buf).ok()?;
    String::from_utf8(buf).ok()
}
