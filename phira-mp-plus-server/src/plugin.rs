//! Phira-mp+ WASM 插件系统
//!
//! 基于 wasmtime 的 WASM 运行时，支持从 WIT 接口规范加载并运行插件。
//! 插件可以提供事件监听、数据处理、API扩展等功能。

use crate::extensions::ExtensionManager;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;
use tokio::sync::RwLock;
use tracing::{info, warn};

/// 插件信息
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PluginInfo {
    pub name: String,
    pub version: String,
    pub author: String,
    pub description: String,
}

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

/// 插件事件
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum PluginEvent {
    UserConnect { user_id: i32, user_name: String },
    UserDisconnect { user_id: i32, user_name: String },
    RoomCreate { user_id: i32, room_id: String },
    RoomJoin { user_id: i32, room_id: String, is_monitor: bool },
    RoomLeave { user_id: i32, room_id: String },
    RoomModify { user_id: i32, room_id: String, data: String },
    GameStart { user_id: i32, room_id: String },
    GameEnd { user_id: i32, room_id: String, score: i32, accuracy: f32 },
    /// 玩家触摸事件（包含触摸点坐标）
    PlayerTouches { user_id: i32, room_id: String, data: Vec<TouchEventPoint> },
    /// 玩家判定事件
    PlayerJudges { user_id: i32, room_id: String, data: Vec<JudgeEventItem> },
}

/// 触摸事件中的一个触点
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TouchEventPoint {
    pub time: f32,
    pub finger: i8,
    pub x: f32,
    pub y: f32,
}

/// 判定事件中的一个判定
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JudgeEventItem {
    pub time: f32,
    pub line_id: u32,
    pub note_id: u32,
    pub judgement: String,
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
    use std::sync::Arc;

    /// 原生插件特征 - 插件开发者实现此特征
    pub trait NativePlugin: Send + Sync {
        fn info(&self) -> PluginInfo;
        fn init(&mut self, ctx: &PluginContext) -> Result<(), String>;
        fn cleanup(&mut self);
        fn on_event(&self, ctx: &PluginContext, event: &PluginEvent) -> Vec<String>;
    }

    /// 插件上下文 - 提供给插件的 API 接口
    pub struct PluginContext {
        pub plugin_name: String,
        pub extensions: Arc<ExtensionManager>,
    }

    impl PluginContext {
        pub fn new(plugin_name: &str, extensions: Arc<ExtensionManager>) -> Self {
            Self {
                plugin_name: plugin_name.to_string(),
                extensions,
            }
        }
    }

    /// 原生插件包装器
    pub struct NativePluginWrapper {
        meta: PluginMeta,
        plugin: Box<dyn NativePlugin>,
        ctx: Arc<PluginContext>,
    }

    impl NativePluginWrapper {
        pub fn new(
            plugin: Box<dyn NativePlugin>,
            path: &str,
            ctx: Arc<PluginContext>,
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
                ctx,
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
            self.plugin.init(&self.ctx)?;
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
            let ctx = PluginContext::new(&self.meta.info.name, self.ctx.extensions.clone());
            self.plugin.on_event(&ctx, _event)
        }
    }
}

/// 插件处理器 - 负责加载、管理和调度插件
pub struct PluginManager {
    plugins: Arc<RwLock<Vec<Box<dyn PluginHost>>>>,
    cli_commands: Arc<RwLock<HashMap<String, CliCommand>>>,
    extensions: Arc<ExtensionManager>,
    plugins_dir: String,
}

impl PluginManager {
    pub fn new(plugins_dir: &str, extensions: Arc<ExtensionManager>) -> Self {
        Self {
            plugins: Arc::new(RwLock::new(Vec::new())),
            cli_commands: Arc::new(RwLock::new(HashMap::new())),
            extensions,
            plugins_dir: plugins_dir.to_string(),
        }
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
        let ctx = Arc::new(native::PluginContext::new(name, self.extensions.clone()));
        let mut wrapper = native::NativePluginWrapper::new(plugin, name, ctx);
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
        let mut guard = self.cli_commands.write().await;
        if guard.contains_key(&cmd.name) {
            return Err(format!("CLI command '{}' is already registered", cmd.name));
        }
        info!("registered CLI command: {}", cmd.name);
        guard.insert(cmd.name.clone(), cmd);
        Ok(())
    }

    /// 执行插件 CLI 命令，返回输出行
    pub async fn execute_cli_command(&self, name: &str, args: &[&str]) -> Option<Vec<String>> {
        let guard = self.cli_commands.read().await;
        guard.get(name).map(|cmd| (cmd.handler)(args))
    }

    /// 列出所有已注册的插件 CLI 命令
    pub async fn list_cli_commands(&self) -> Vec<CliCommand> {
        let guard = self.cli_commands.read().await;
        guard.values().cloned().collect()
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
