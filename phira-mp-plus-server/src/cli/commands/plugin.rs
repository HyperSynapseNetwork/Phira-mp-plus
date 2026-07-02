use super::super::*;

impl CliHandler {
    pub(in crate::cli) async fn dispatch_plugin_command(&self, args: &[&str]) {
        let sub = args.first().copied().unwrap_or("");
        match sub {
            "list" | "" => self.list_plugins().await,
            "enable" => {
                if args.len() < 2 {
                    self.out(format!(
                        "  {} {} plugin enable <插件名>",
                        c::yellow("?"),
                        c::bold("用法")
                    ));
                } else {
                    self.enable_plugin(args[1]).await;
                }
            }
            "disable" => {
                if args.len() < 2 {
                    self.out(format!(
                        "  {} {} plugin disable <插件名>",
                        c::yellow("?"),
                        c::bold("用法")
                    ));
                } else {
                    self.disable_plugin(args[1]).await;
                }
            }
            "reload" => self.reload_plugins().await,
            "info" => {
                if args.len() < 2 {
                    self.out(format!(
                        "  {} {} plugin info <插件ID或名称>",
                        c::yellow("?"),
                        c::bold("用法")
                    ));
                } else {
                    self.plugin_info(args[1]).await;
                }
            }
            "call" => {
                if args.len() < 3 {
                    self.out(format!(
                        "  {} {} plugin call <插件ID或名称> <方法> [JSON数组]",
                        c::yellow("?"),
                        c::bold("用法")
                    ));
                } else {
                    self.plugin_call(args[1], args[2], &args[3..].join(" "))
                        .await;
                }
            }
            _ => {
                self.out(format!(
                    "  {} 未知子命令: {}  ",
                    c::red("✗"),
                    c::yellow(sub)
                ));
                self.out(format!(
                    "  {} 可用: plugin list | enable | disable | reload | info | call",
                    c::dim("▸")
                ));
            }
        }
    }
}

impl CliHandler {
    pub(crate) async fn list_plugins(&self) {
        let plugins = self.state.plugin_manager.list_plugins().await;
        if plugins.is_empty() {
            self.out(format!("  {} 无已加载的插件", c::dim("·")));
            return;
        }
        self.out(format!(
            "  {} 已加载插件 ({})",
            c::green("◆"),
            plugins.len()
        ));
        self.out(format!(
            "  {}",
            c::dim("  ────────────────────────────────────────────")
        ));
        for p in &plugins {
            let state_str = match &p.state {
                crate::plugin::PluginState::Enabled => c::green("启用"),
                crate::plugin::PluginState::Disabled => c::yellow("禁用"),
                crate::plugin::PluginState::Loaded => c::cyan("已加载"),
                crate::plugin::PluginState::Error(_) => c::red("错误"),
            };
            let stable_id = std::path::Path::new(&p.path)
                .file_stem()
                .and_then(|value| value.to_str())
                .unwrap_or("?");
            self.out(format!(
                "  {} {:<18} {} {}  {}",
                c::dim("│"),
                stable_id,
                c::dim(p.info.version.as_str()),
                state_str,
                c::dim(&format!("({})", p.info.name))
            ));
        }
    }

    pub(crate) async fn enable_plugin(&self, name: &str) {
        match self.state.plugin_manager.enable_plugin(name).await {
            Ok(_) => self.out(format!("  {} 插件 {} 已启用", c::green("✓"), c::bold(name))),
            Err(e) => self.out(format!("  {} {}", c::red("✗"), e)),
        }
    }

    pub(crate) async fn disable_plugin(&self, name: &str) {
        match self.state.plugin_manager.disable_plugin(name).await {
            Ok(_) => self.out(format!("  {} 插件 {} 已禁用", c::green("✓"), c::bold(name))),
            Err(e) => self.out(format!("  {} {}", c::red("✗"), e)),
        }
    }

    pub(crate) async fn reload_plugins(&self) {
        self.out(format!("  {} 正在重载所有插件...", c::yellow("⟳")));
        match self.state.plugin_manager.reload_plugins().await {
            Ok(count) => self.out(format!("  {} 已重载 {} 个插件", c::green("✓"), count)),
            Err(e) => self.out(format!("  {} 重载失败: {}", c::red("✗"), e)),
        }
    }

    pub(crate) async fn plugin_info(&self, name: &str) {
        let plugins = self.state.plugin_manager.list_plugins().await;
        if let Some(p) = plugins.into_iter().find(|p| {
            p.info.name == name
                || std::path::Path::new(&p.path)
                    .file_stem()
                    .and_then(|value| value.to_str())
                    == Some(name)
        }) {
            let state_str = match &p.state {
                crate::plugin::PluginState::Enabled => c::green("启用"),
                crate::plugin::PluginState::Disabled => c::yellow("禁用"),
                crate::plugin::PluginState::Loaded => c::cyan("已加载"),
                crate::plugin::PluginState::Error(ref e) => c::red(&format!("错误: {}", e)),
            };
            self.out(format!(
                "  {} 插件详情: {}",
                c::green("◆"),
                c::bold(&p.info.name)
            ));
            let stable_id = std::path::Path::new(&p.path)
                .file_stem()
                .and_then(|value| value.to_str())
                .unwrap_or("?");
            self.out(format!("  {} ID:       {}", c::dim("│"), stable_id));
            self.out(format!("  {} 版本:     {}", c::dim("│"), p.info.version));
            self.out(format!("  {} 作者:     {}", c::dim("│"), p.info.author));
            self.out(format!(
                "  {} 描述:     {}",
                c::dim("│"),
                p.info.description
            ));
            self.out(format!("  {} 状态:     {}", c::dim("│"), state_str));
            self.out(format!("  {} 路径:     {}", c::dim("│"), c::dim(&p.path)));
        } else {
            self.out(format!("  {} 未找到插件: {}", c::yellow("!"), name));
        }
    }

    pub(crate) async fn plugin_call(&self, plugin: &str, method: &str, args_json: &str) {
        let args = if args_json.trim().is_empty() {
            Vec::new()
        } else {
            match serde_json::from_str::<Vec<serde_json::Value>>(args_json) {
                Ok(value) => value,
                Err(error) => {
                    self.out(format!("  {} 参数必须是 JSON 数组: {}", c::red("✗"), error));
                    return;
                }
            }
        };
        match self
            .state
            .plugin_manager
            .call_plugin_api(plugin, method, args)
            .await
        {
            Ok(value) => self.out(format!("  {} {}", c::green("✓"), value)),
            Err(error) => self.out(format!("  {} {}", c::red("✗"), error)),
        }
    }

    pub(crate) async fn try_plugin_command(&self, command: &str, args: &[&str]) -> bool {
        let result = self
            .state
            .plugin_manager
            .execute_cli_command(command, args)
            .await;
        match result {
            Some(output_lines) => {
                for line in output_lines {
                    self.out(format!("  {} {}", c::magenta("◈"), line));
                }
                true
            }
            None => false,
        }
    }
}
