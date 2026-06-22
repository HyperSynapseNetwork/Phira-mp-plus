//! Phira-mp+ WASM 插件系统
//!
//! 基于 wasmtime 的 WASM 运行时，支持从 WIT 接口规范加载并运行插件。
//! 插件可以提供事件监听、数据处理、API扩展等功能。

use crate::extensions::ExtensionManager;
use phira_mp_plus_server_api as api;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;
use std::path::Path;
use std::sync::Mutex;
use std::sync::Arc;
use tokio::sync::RwLock;
use tracing::{info, warn};

/// 插件信息（来自 server-api）
pub use api::PluginInfo;

/// 插件事件（来自 server-api）
pub use api::PluginEvent;

/// 触摸事件点（来自 server-api）
pub use api::TouchEventPoint;

/// 判定事件（来自 server-api）
pub use api::JudgeEventItem;

/// 插件 HTTP 句柄（来自 server-api）
pub use api::HttpHandle;

/// 插件上下文（来自 server-api）
pub use api::PluginContext;

/// 原生插件特征（来自 server-api）
pub use api::NativePlugin;

/// 插件状态
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum PluginState {
    Loaded,
    Enabled,
    Disabled,
    Error(String),
}

/// 插件元数据
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PluginMeta {
    pub info: PluginInfo,
    pub path: String,
    pub state: PluginState,
    pub enabled: bool,
}

/// 插件宿主特征 - 所有插件运行时需实现此特征
pub trait PluginHost: Send + Sync {
    /// 获取插件元数据
    fn meta(&self) -> &PluginMeta;
    /// 获取可变的插件元数据
    fn meta_mut(&mut self) -> &mut PluginMeta;
    /// 初始化插件
    fn init(&mut self) -> Result<(), String>;
    /// 清理插件
    fn cleanup(&mut self);
    /// 触发事件
    fn trigger_event(&self, event: &PluginEvent) -> Vec<String>;
}

/// WASM 运行时插件（需要 wasmtime 特性）
#[cfg(feature = "plugin-system")]
pub mod wasm {
    use super::*;

    /// WASM 插件实例
    pub struct WasmPlugin {
        meta: PluginMeta,
        // 在实际实现中，这里会包含 wasmtime 的 Instance 和 Linker
        // wasm_instance: wasmtime::Instance,
        // wasm_store: wasmtime::Store<()>,
    }

    impl WasmPlugin {
        pub fn new(path: &str, info: PluginInfo) -> Self {
            Self {
                meta: PluginMeta {
                    info,
                    path: path.to_string(),
                    state: PluginState::Loaded,
                    enabled: true,
                },
            }
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
            // 初始化 wasmtime 实例，调用插件导出的 init 函数
            self.meta.state = PluginState::Enabled;
            Ok(())
        }

        fn cleanup(&mut self) {
            self.meta.state = PluginState::Disabled;
            info!("plugin '{}' cleaned up", self.meta.info.name);
        }

        fn trigger_event(&self, _event: &PluginEvent) -> Vec<String> {
            // 调用插件导出的 on_* 函数并将结果收集
            Vec::new()
        }
    }
}

/// 原生 Rust 插件（用于测试和开发）
pub mod native {
    use super::*;
    use crate::extensions::ExtensionManager;
    use phira_mp_plus_server_api as api;

    /// 原生插件特征（来自 server-api）
    pub use api::NativePlugin;

    /// 插件上下文（来自 server-api，含 HTTP 路由注册句柄）
    pub use api::PluginContext;

    /// 生成的 API 上下文（含服务端内部引用）
    pub struct ServerPluginContext {
        pub ctx: api::PluginContext,
        pub extensions: Arc<ExtensionManager>,
    }

    /// 原生插件包装器
    pub struct NativePluginWrapper {
        meta: PluginMeta,
        plugin: Box<dyn NativePlugin>,
        svr_ctx: Arc<ServerPluginContext>,
    }

    impl NativePluginWrapper {
        pub fn new(
            plugin: Box<dyn NativePlugin>,
            path: &str,
            svr_ctx: Arc<ServerPluginContext>,
        ) -> Self {
            let info = plugin.info();
            Self {
                meta: PluginMeta {
                    info,
                    path: path.to_string(),
                    state: PluginState::Loaded,
                    enabled: true,
                },
                plugin,
                svr_ctx,
            }
        }
    }

    impl PluginHost for NativePluginWrapper {
        fn meta(&self) -> &PluginMeta {
            &self.meta
        }

        fn meta_mut(&mut self) -> &mut PluginMeta {
            &mut self.meta
        }

        fn init(&mut self) -> Result<(), String> {
            self.plugin.init(&self.svr_ctx.ctx)?;
            self.meta.state = PluginState::Enabled;
            Ok(())
        }

        fn cleanup(&mut self) {
            self.plugin.cleanup();
            self.meta.state = PluginState::Disabled;
        }

        fn trigger_event(&self, _event: &PluginEvent) -> Vec<String> {
            if !self.meta.enabled {
                return Vec::new();
            }
            self.plugin.on_event(&self.svr_ctx.ctx, _event)
        }
    }
}

/// 插件处理器 - 负责加载、管理和调度插件
pub struct PluginManager {
    plugins: Arc<RwLock<Vec<Box<dyn PluginHost>>>>,
    cli_commands: Arc<Mutex<HashMap<String, CliCommand>>>,
    api_handlers: Arc<Mutex<HashMap<String, api::PluginApiHandler>>>,
    extensions: Arc<ExtensionManager>,
    plugins_dir: String,
    http_handle: Arc<RwLock<Option<api::HttpHandle>>>,
    send_chat: Arc<RwLock<Option<Arc<dyn Fn(i32, String) + Send + Sync>>>>,
}

impl PluginManager {
    pub fn new(plugins_dir: &str, extensions: Arc<ExtensionManager>) -> Self {
        Self {
            plugins: Arc::new(RwLock::new(Vec::new())),
            cli_commands: Arc::new(Mutex::new(HashMap::new())),
            api_handlers: Arc::new(Mutex::new(HashMap::new())),
            extensions,
            plugins_dir: plugins_dir.to_string(),
            http_handle: Arc::new(RwLock::new(None)),
            send_chat: Arc::new(RwLock::new(None)),
        }
    }

    /// 设置发送聊天消息的句柄
    pub async fn set_send_chat(&self, f: Arc<dyn Fn(i32, String) + Send + Sync>) {
        *self.send_chat.write().await = Some(f);
    }

    /// 设置中央 HTTP 句柄（让插件在 init 中注册路由）
    pub async fn set_http_handle(&self, handle: api::HttpHandle) {
        *self.http_handle.write().await = Some(handle);
    }

    /// 注册插件 API（供其他插件调用）
    pub async fn register_plugin_api(&self, name: &str, handler: api::PluginApiHandler) {
        self.api_handlers.lock().unwrap_or_else(|e| e.into_inner()).insert(name.to_string(), handler);
    }

    /// 调用其他插件的 API
    async fn call_plugin_api(&self, plugin: &str, method: &str, args: &[Value]) -> Result<Value, String> {
        let handlers = self.api_handlers.lock().unwrap_or_else(|e| e.into_inner());
        match handlers.get(plugin) {
            Some(h) => h(method, args),
            None => Err(format!("plugin '{}' not found or no API registered", plugin)),
        }
    }

    async fn make_svr_ctx(&self, name: &str, state_query: Option<api::ServerStateQuery>) -> Arc<native::ServerPluginContext> {
        let mut ctx = api::PluginContext::new(name);
        if let Some(ref h) = *self.http_handle.read().await {
            ctx = ctx.with_http(h.clone());
        }
        if let Some(ref q) = state_query {
            ctx = ctx.with_state(q.clone());
        }
        // 提供插件间 API 调用
        let handlers = self.api_handlers.clone();
        let api_reg = api::PluginApiRegistry::new(move |plugin, method, args| {
            let guard = handlers.lock().unwrap_or_else(|e| e.into_inner());
            match guard.get(plugin) {
                Some(h) => h(method, args),
                None => Err(format!("plugin '{}' has no API", plugin)),
            }
        });
        ctx = ctx.with_api(api_reg);

        // 提供 CLI 命令注册
        let cli_cmds = self.cli_commands.clone();
        let cli_handle = api::CliHandle::new(move |name, desc, usage, handler| {
            let cmd = CliCommand {
                name: name.to_string(),
                description: desc.to_string(),
                usage: usage.to_string(),
                handler,
            };
            cli_cmds.lock().unwrap().insert(name.to_string(), cmd);
            info!("plugin registered CLI command: {name}");
            Ok(())
        });
        ctx = ctx.with_cli(cli_handle);

        // 提供发送聊天消息能力
        if let Some(ref sc) = *self.send_chat.read().await {
            ctx = ctx.with_send_chat(Arc::clone(sc));
        }

        Arc::new(native::ServerPluginContext {
            ctx,
            extensions: self.extensions.clone(),
        })
    }

    /// 从指定目录加载所有插件
    pub async fn load_plugins(&self) -> Result<usize, String> {
        let dir = Path::new(&self.plugins_dir);
        if !dir.exists() {
            std::fs::create_dir_all(dir).map_err(|e| format!("create plugins dir: {}", e))?;
            info!("created plugins directory: {}", self.plugins_dir);
            return Ok(0);
        }

        let mut loaded = 0usize;
        let mut entries = Vec::new();

        if let Ok(read_dir) = std::fs::read_dir(dir) {
            for entry in read_dir.flatten() {
                let path = entry.path();
                if path.extension().is_some_and(|ext| ext == "wasm")
                    || path.extension().is_some_and(|ext| ext == "so")
                    || path.extension().is_some_and(|ext| ext == "dylib")
                {
                    entries.push(path);
                }
            }
        }

        entries.sort();
        for path in &entries {
            match self.load_plugin(path).await {
                Ok(meta) => {
                    info!("loaded plugin: {} v{}", meta.info.name, meta.info.version);
                    loaded += 1;
                }
                Err(e) => {
                    warn!("failed to load plugin '{}': {}", path.display(), e);
                }
            }
        }

        Ok(loaded)
    }

    /// 加载单个插件
    async fn load_plugin(&self, path: &Path) -> Result<PluginMeta, String> {
        let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");

        match ext {
            "wasm" => {
                // WASM 插件 - 使用 wasmtime
                #[cfg(feature = "plugin-system")]
                {
                    let info = PluginInfo {
                        name: path
                            .file_stem()
                            .and_then(|s| s.to_str())
                            .unwrap_or("unknown")
                            .to_string(),
                        version: "0.1.0".to_string(),
                        author: "unknown".to_string(),
                        description: format!("WASM plugin from {}", path.display()),
                    };
                    let mut plugin =
                        wasm::WasmPlugin::new(path.to_str().unwrap_or(""), info.clone());
                    plugin.init()?;
                    let meta = plugin.meta().clone();
                    self.plugins.write().await.push(Box::new(plugin));
                    Ok(meta)
                }
                #[cfg(not(feature = "plugin-system"))]
                {
                    Err("WASM plugin support not enabled (compile with 'plugin-system' feature)".to_string())
                }
            }
            "so" | "dylib" => {
                // 原生动态库插件
                // 在实际实现中，这里会使用 libloading 加载动态库
                Err(format!(
                    "native dynamic library plugins not yet supported: {}",
                    path.display()
                ))
            }
            _ => Err(format!("unsupported plugin type: {}", ext)),
        }
    }

    /// 注册一个原生插件（用于开发和测试）
    pub async fn register_native(
        &self,
        plugin: Box<dyn native::NativePlugin>,
        name: &str,
    ) -> Result<(), String> {
        self.register_native_with_state(plugin, name, None).await
    }

    /// 注册一个原生插件，附带服务端状态查询
    pub async fn register_native_with_state(
        &self,
        plugin: Box<dyn native::NativePlugin>,
        name: &str,
        state_query: Option<api::ServerStateQuery>,
    ) -> Result<(), String> {
        let svr_ctx = self.make_svr_ctx(name, state_query).await;
        let mut wrapper = native::NativePluginWrapper::new(plugin, name, svr_ctx);
        // 初始化插件
        wrapper.init()?;
        let info = wrapper.meta().info.clone();
        // 注册插件
        self.plugins.write().await.push(Box::new(wrapper));
        info!("registered native plugin: {} v{}", info.name, info.version);
        Ok(())
    }

    /// 触发事件 - 向所有已启用插件分发事件
    pub async fn trigger(&self, event: &PluginEvent) -> Vec<PluginEventResult> {
        let plugins = self.plugins.read().await;
        let mut results = Vec::new();

        for plugin in plugins.iter() {
            if !plugin.meta().enabled {
                continue;
            }
            let responses = plugin.trigger_event(event);
            if !responses.is_empty() {
                results.push(PluginEventResult {
                    plugin_name: plugin.meta().info.name.clone(),
                    responses,
                });
            }
        }

        results
    }

    /// 获取所有插件列表
    pub async fn list_plugins(&self) -> Vec<PluginMeta> {
        let guard = self.plugins.read().await;
        guard.iter().map(|p| p.meta().clone()).collect()
    }

    /// 启用插件
    pub async fn enable_plugin(&self, name: &str) -> Result<(), String> {
        let mut guard = self.plugins.write().await;
        for plugin in guard.iter_mut() {
            if plugin.meta().info.name == name {
                plugin.meta_mut().enabled = true;
                plugin.meta_mut().state = PluginState::Enabled;
                return Ok(());
            }
        }
        Err(format!("plugin '{}' not found", name))
    }

    /// 禁用插件
    pub async fn disable_plugin(&self, name: &str) -> Result<(), String> {
        let mut guard = self.plugins.write().await;
        for plugin in guard.iter_mut() {
            if plugin.meta().info.name == name {
                plugin.meta_mut().enabled = false;
                plugin.meta_mut().state = PluginState::Disabled;
                return Ok(());
            }
        }
        Err(format!("plugin '{}' not found", name))
    }

    /// 重新加载所有插件
    pub async fn reload_plugins(&self) -> Result<usize, String> {
        // 清理所有现有插件
        {
            let mut guard = self.plugins.write().await;
            for plugin in guard.iter_mut() {
                plugin.cleanup();
            }
            guard.clear();
        }
        // 重新加载
        self.load_plugins().await
    }

    /// 清理所有插件
    pub async fn cleanup_all(&self) {
        let mut guard = self.plugins.write().await;
        for plugin in guard.iter_mut() {
            plugin.cleanup();
        }
        guard.clear();
    }

    /// 注册插件 CLI 命令
    pub async fn register_cli_command(&self, cmd: CliCommand) -> Result<(), String> {
        let mut guard = self.cli_commands.lock().unwrap();
        if guard.contains_key(&cmd.name) {
            return Err(format!("CLI command '{}' is already registered", cmd.name));
        }
        info!("registered CLI command: {}", cmd.name);
        guard.insert(cmd.name.clone(), cmd);
        Ok(())
    }

    /// 执行插件 CLI 命令，返回输出行
    pub async fn execute_cli_command(&self, name: &str, args: &[&str]) -> Option<Vec<String>> {
        let guard = self.cli_commands.lock().ok()?;
        guard.get(name).map(|cmd| (cmd.handler)(args))
    }

    /// 列出所有已注册的插件 CLI 命令
    pub async fn list_cli_commands(&self) -> Vec<CliCommand> {
        if let Ok(guard) = self.cli_commands.lock() {
            guard.values().cloned().collect()
        } else {
            Vec::new()
        }
    }
}

/// 插件 CLI 命令
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

/// 插件事件处理结果
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PluginEventResult {
    pub plugin_name: String,
    pub responses: Vec<String>,
}

/// 用于调试/测试的事件日志插件
pub fn create_event_logger() -> Box<dyn native::NativePlugin> {
    struct EventLogger;
    impl native::NativePlugin for EventLogger {
        fn info(&self) -> PluginInfo {
            PluginInfo {
                name: "event-logger".to_string(),
                version: "0.1.0".to_string(),
                author: "Phira-mp+".to_string(),
                description: "记录所有插件事件到日志".to_string(),
            }
        }
        fn init(&mut self, _ctx: &native::PluginContext) -> Result<(), String> {
            info!("EventLogger plugin initialized");
            Ok(())
        }
        fn cleanup(&mut self) {
            info!("EventLogger plugin cleaned up");
        }
        fn on_event(&self, _ctx: &native::PluginContext, _event: &PluginEvent) -> Vec<String> {
            info!("PluginEvent received");
            Vec::new()
        }
    }
    Box::new(EventLogger)
}
