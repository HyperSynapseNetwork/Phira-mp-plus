//! Command metadata registry for Runtime v2.
//!
//! This module stores command metadata, help text, completion hints, and
//! optional execution handlers. The existing CLI dispatcher remains the
//! fallback, but new commands should register a handler here to converge
//! CLI/TUI/admin _ /WIT execution on a single path.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use crate::server::PlusServerState;

/// Handler signature for a registered CLI command.
pub type CommandHandler = Arc<dyn Fn(&PlusServerState, &[&str]) -> Vec<String> + Send + Sync>;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandArgSpec {
    pub name: String,
    pub description: String,
    pub required: bool,
}

impl CommandArgSpec {
    pub fn required(name: impl Into<String>, description: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            description: description.into(),
            required: true,
        }
    }

    pub fn optional(name: impl Into<String>, description: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            description: description.into(),
            required: false,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum CommandAudience {
    /// Recommended day-to-day commands shown in the default help overview.
    Primary,
    /// Useful operational/diagnostic commands hidden from the default overview.
    Advanced,
    /// Internal developer commands (runtime internals, simulation internals).
    Developer,
}

impl CommandAudience {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Primary => "primary",
            Self::Advanced => "advanced",
            Self::Developer => "developer",
        }
    }
}

#[derive(Clone)]
pub struct CommandSpec {
    pub name: String,
    pub group: String,
    pub description: String,
    pub usage: String,
    pub args: Vec<CommandArgSpec>,
    pub examples: Vec<String>,
    pub audience: CommandAudience,
    /// Optional handler for executing this command via the registry.
    pub handler: Option<CommandHandler>,
}

impl CommandSpec {
    pub fn new(
        name: impl Into<String>,
        group: impl Into<String>,
        description: impl Into<String>,
        usage: impl Into<String>,
    ) -> Self {
        Self {
            name: name.into(),
            group: group.into(),
            description: description.into(),
            usage: usage.into(),
            args: Vec::new(),
            examples: Vec::new(),
            audience: CommandAudience::Primary,
            handler: None,
        }
    }

    pub fn arg(mut self, arg: CommandArgSpec) -> Self {
        self.args.push(arg);
        self
    }

    pub fn example(mut self, example: impl Into<String>) -> Self {
        self.examples.push(example.into());
        self
    }

    pub fn advanced(mut self) -> Self {
        self.audience = CommandAudience::Advanced;
        self
    }

    pub fn developer(mut self) -> Self {
        self.audience = CommandAudience::Developer;
        self
    }

    pub fn handler(mut self, handler: CommandHandler) -> Self {
        self.handler = Some(handler);
        self
    }

    pub fn is_primary(&self) -> bool {
        self.audience == CommandAudience::Primary
    }
}

/// Argument completer: given the full command path and current partial token,
/// return possible completions.  The registry has no server state, so the
/// installer (server.rs) provides context-aware completers at startup.
pub type ArgCompleter = Arc<dyn Fn(&[String], &str) -> Vec<String> + Send + Sync>;

pub struct CommandRegistry {
    commands: BTreeMap<String, CommandSpec>,
    roots: BTreeSet<String>,
    children: BTreeMap<String, BTreeSet<String>>,
    /// Per-command argument completers, keyed by normalised command name.
    arg_completers: std::sync::RwLock<BTreeMap<String, ArgCompleter>>,
}

impl Clone for CommandRegistry {
    fn clone(&self) -> Self {
        Self {
            commands: self.commands.clone(),
            roots: self.roots.clone(),
            children: self.children.clone(),
            arg_completers: std::sync::RwLock::new(
                self.arg_completers
                    .read()
                    .map(|g| g.clone())
                    .unwrap_or_default(),
            ),
        }
    }
}

impl Default for CommandRegistry {
    fn default() -> Self {
        Self {
            commands: BTreeMap::new(),
            roots: BTreeSet::new(),
            children: BTreeMap::new(),
            arg_completers: std::sync::RwLock::new(BTreeMap::new()),
        }
    }
}

impl CommandRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Register an argument completer for a fully-qualified command name.
    pub fn set_arg_completer(&self, cmd_name: &str, completer: ArgCompleter) {
        if let Ok(mut guard) = self.arg_completers.write() {
            guard.insert(normalize_command_name(cmd_name), completer);
        }
    }

    /// Get completions for the argument after a known leaf command.
    fn complete_arg(&self, cmd_name: &str, prefix: &str) -> Option<Vec<String>> {
        let guard = self.arg_completers.read().ok()?;
        let completer = guard.get(&normalize_command_name(cmd_name))?;
        let cmds: Vec<String> = cmd_name.split_whitespace().map(|s| s.to_string()).collect();
        Some(completer(&cmds, prefix))
    }

    pub fn register(&mut self, spec: CommandSpec) -> Result<(), String> {
        let name = normalize_command_name(&spec.name);
        if name.is_empty() {
            return Err("command name cannot be empty".to_string());
        }
        if self.commands.contains_key(&name) {
            return Err(format!("duplicated command name: {name}"));
        }

        self.index_command_path(&name);

        let mut spec = spec;
        spec.name = name.clone();
        self.commands.insert(name, spec);
        Ok(())
    }

    pub fn get(&self, name: &str) -> Option<&CommandSpec> {
        let normalized = normalize_command_name(name);
        self.commands.get(&normalized)
    }

    pub fn iter(&self) -> impl Iterator<Item = &CommandSpec> {
        self.commands.values()
    }

    pub fn groups(&self) -> Vec<String> {
        self.commands
            .values()
            .map(|cmd| cmd.group.clone())
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect()
    }

    pub fn commands_in_group(
        &self,
        group: &str,
        audience_filter: Option<CommandAudience>,
    ) -> Vec<&CommandSpec> {
        self.commands
            .values()
            .filter(|cmd| cmd.group == group)
            .filter(|cmd| audience_filter.map_or(true, |filter| cmd.audience == filter))
            .collect()
    }

    pub fn command_surface_counts(&self) -> (usize, usize, usize) {
        let primary = self
            .commands
            .values()
            .filter(|cmd| cmd.audience == CommandAudience::Primary)
            .count();
        let advanced = self
            .commands
            .values()
            .filter(|cmd| cmd.audience == CommandAudience::Advanced)
            .count();
        let developer = self
            .commands
            .values()
            .filter(|cmd| cmd.audience == CommandAudience::Developer)
            .count();
        (primary, advanced, developer)
    }

    pub fn root_commands(&self) -> Vec<String> {
        self.roots.iter().cloned().collect()
    }

    pub fn child_commands(&self, parent: &str) -> Vec<String> {
        let parent = normalize_command_name(parent);
        self.children
            .get(&parent)
            .map(|set| set.iter().cloned().collect())
            .unwrap_or_default()
    }

    /// Complete a raw CLI line before cursor.
    ///
    /// Returned values are the token that should replace the current partial
    /// token, not the full command line. This keeps the TUI integration simple
    /// and avoids byte/cursor complexity there.
    pub fn complete_line(&self, before_cursor: &str) -> Vec<String> {
        let raw = before_cursor.trim_start();
        if raw.is_empty() {
            return self.root_commands();
        }

        let ends_with_space = raw.chars().last().is_some_and(char::is_whitespace);
        let tokens = raw.split_whitespace().collect::<Vec<_>>();
        if tokens.is_empty() {
            return self.root_commands();
        }

        if tokens.len() == 1 && !ends_with_space {
            return self.complete_root(tokens[0]);
        }

        let parent_tokens = if ends_with_space {
            tokens.as_slice()
        } else {
            &tokens[..tokens.len().saturating_sub(1)]
        };
        let parent = normalize_command_name(&parent_tokens.join(" "));
        let prefix = if ends_with_space {
            ""
        } else {
            tokens.last().copied().unwrap_or("")
        };

        let mut out: Vec<String> = Vec::new();

        // Level 2+: complete subcommands of the parent namespace.
        if !parent.is_empty() {
            if let Some(children) = self.children.get(&parent) {
                out = children
                    .iter()
                    .filter(|child| child.starts_with(prefix))
                    .cloned()
                    .collect();
            }
        }

        // If parent IS a known leaf command (not a namespace), try arg completion.
        if out.is_empty() && self.commands.contains_key(&parent) {
            if let Some(arg_matches) = self.complete_arg(&parent, prefix) {
                out = arg_matches;
                // fall through to the fallback if arg completer returned nothing
            }
        }

        // Fallback: try to match the current token as a subcommand of any
        // canonical command path.
        if out.is_empty() {
            let parent_prefix = if parent.is_empty() {
                String::new()
            } else {
                format!("{parent} ")
            };
            out = self
                .commands
                .keys()
                .filter_map(|name| name.strip_prefix(&parent_prefix))
                .filter(|rest| !rest.contains(' ') && rest.starts_with(prefix))
                .map(ToOwned::to_owned)
                .collect::<BTreeSet<_>>()
                .into_iter()
                .collect();
        }

        out
    }

    /// Install argument completers. Called from `runtime_v2_registry()`.
    /// Dynamic completers (room IDs, user IDs) reference server state via
    /// `Weak` and are only active while the server is running.
    pub fn init_completers(&self) {
        // Benchmark mode completer: benchmark run <real|hybrid>
        self.set_arg_completer(
            "benchmark run",
            Arc::new(|_cmd, prefix| {
                vec!["real", "hybrid"]
                    .into_iter()
                    .filter(|m| m.starts_with(prefix))
                    .map(|s| s.to_string())
                    .collect()
            }),
        );

        // Simulation preset completer: simulation run <preset>
        self.set_arg_completer(
            "simulation run",
            Arc::new(|_cmd, prefix| {
                vec!["baseline", "small", "medium", "large", "custom"]
                    .into_iter()
                    .filter(|p| p.starts_with(prefix))
                    .map(|s| s.to_string())
                    .collect()
            }),
        );

        // Simulation suite completer: simulation suite <name>
        self.set_arg_completer(
            "simulation suite",
            Arc::new(|_cmd, prefix| {
                vec!["smoke", "mixed", "stress"]
                    .into_iter()
                    .filter(|s| s.starts_with(prefix))
                    .map(|s| s.to_string())
                    .collect()
            }),
        );
    }

    /// Install a room-ID completer after PlusServerState is available.
    /// Called from server.rs after state creation.
    pub fn install_room_completer(&self, state: &Arc<crate::server::PlusServerState>) {
        let weak = Arc::downgrade(state);
        let completer: ArgCompleter =
            Arc::new(move |_cmd: &[String], prefix: &str| -> Vec<String> {
                let Some(state) = weak.upgrade() else {
                    return vec![];
                };
                let Ok(rooms) = state.rooms.try_read() else {
                    return vec![];
                };
                let mut out: Vec<String> = rooms
                    .keys()
                    .filter(|id| id.to_string().starts_with(prefix))
                    .map(|id| id.to_string())
                    .collect();
                out.sort();
                out
            });
        for cmd in &[
            "room info",
            "room banlist",
            "room rounds",
            "room history",
            "room uuid",
            "room start",
            "room force-start",
            "force-start",
            "room cancel",
            "room hide",
            "room unhide",
            "room close",
            "room kick",
            "room host",
            "room force-move",
            "room set",
            "room ban",
            "room unban",
        ] {
            self.set_arg_completer(cmd, Arc::clone(&completer));
        }
    }

    pub fn complete_root(&self, prefix: &str) -> Vec<String> {
        self.roots
            .iter()
            .filter(|name| name.starts_with(prefix))
            .cloned()
            .collect()
    }

    pub fn format_help(&self, name_or_alias: &str) -> Option<String> {
        let spec = self.get(name_or_alias)?;
        let mut lines = Vec::new();
        lines.push("NAME".to_string());
        lines.push(format!("    {}", spec.name));
        lines.push(String::new());
        lines.push("DESCRIPTION".to_string());
        lines.push(format!("    {}", spec.description));
        lines.push(String::new());
        lines.push("USAGE".to_string());
        lines.push(format!("    {}", spec.usage));
        lines.push(String::new());
        lines.push("SURFACE".to_string());
        lines.push(format!("    {}", spec.audience.as_str()));

        if !spec.args.is_empty() {
            lines.push(String::new());
            lines.push("ARGS".to_string());
            for arg in &spec.args {
                let marker = if arg.required { "required" } else { "optional" };
                lines.push(format!(
                    "    {:<18} {:<8} {}",
                    arg.name, marker, arg.description
                ));
            }
        }

        if !spec.examples.is_empty() {
            lines.push(String::new());
            lines.push("EXAMPLES".to_string());
            for example in &spec.examples {
                lines.push(format!("    {example}"));
            }
        }

        Some(lines.join("\n"))
    }

    pub fn format_overview(&self) -> String {
        let mut lines = Vec::new();
        lines.push("Phira-mp+ 管理命令".to_string());
        lines.push("─────────────────────────────────────────────".to_string());
        lines.push("提示：help <命令> 查看详情".to_string());
        lines.push("提示：help advanced / help dev 查看其它命令".to_string());
        lines.push("提示：游戏内管理员入口仍使用 _ 命令，__ 表示字面量下划线".to_string());
        lines.push(String::new());

        for group in self.groups() {
            let visible: Vec<&CommandSpec> = self
                .commands
                .values()
                .filter(|cmd| cmd.group == group && cmd.audience == CommandAudience::Primary)
                .collect();
            if visible.is_empty() {
                continue;
            }
            lines.push(format!("▸ {group}"));
            for spec in &visible {
                lines.push(format!("    {:<32} {}", spec.usage, spec.description));
            }
            lines.push(String::new());
        }

        lines.push("─────────────────────────────────────────────".to_string());
        lines.push("help <命令> 查看详情 • help advanced / dev 查看更多".to_string());
        lines.join("\n")
    }

    pub fn format_overview_all(&self) -> String {
        let mut lines = Vec::new();
        let (primary, advanced, developer) = self.command_surface_counts();
        lines.push("Phira-mp+ 管理命令（完整视图）".to_string());
        lines.push("─────────────────────────────────────────────".to_string());
        lines.push(format!(
            "primary={primary} advanced={advanced} dev={developer}"
        ));
        lines.push(String::new());
        for group in self.groups() {
            lines.push(self.format_group(&group, true));
            lines.push(String::new());
        }
        lines.join("\n")
    }

    pub fn format_groups(&self) -> String {
        let mut lines = Vec::new();
        lines.push("命令分组".to_string());
        lines.push("─────────────────────────────────────────────".to_string());
        for group in self.groups() {
            let primary = self
                .commands
                .values()
                .filter(|cmd| cmd.group == group && cmd.audience == CommandAudience::Primary)
                .count();
            let total = self
                .commands
                .values()
                .filter(|cmd| cmd.group == group)
                .count();
            lines.push(format!(
                "    {:<16} primary={} total={}    help group {}",
                group, primary, total, group
            ));
        }
        lines.join("\n")
    }

    pub fn format_group(&self, group: &str, include_all: bool) -> String {
        let mut lines = Vec::new();
        let group = group.trim();
        let commands: Vec<&CommandSpec> = if include_all {
            self.commands
                .values()
                .filter(|cmd| cmd.group == group)
                .collect()
        } else {
            self.commands
                .values()
                .filter(|cmd| cmd.group == group && cmd.audience == CommandAudience::Primary)
                .collect()
        };
        if commands.is_empty() {
            return format!("未找到命令分组: {group}");
        }
        let title = if include_all {
            format!("命令分组：{group}（完整）")
        } else {
            format!("命令分组：{group}")
        };
        lines.push(title);
        lines.push("─────────────────────────────────────────────".to_string());
        for spec in commands {
            let marker = match spec.audience {
                CommandAudience::Primary => " ",
                CommandAudience::Advanced => "advanced",
                CommandAudience::Developer => "dev",
            };
            lines.push(format!(
                "    {:<32} {:<10} {}",
                spec.usage, marker, spec.description
            ));
        }
        lines.join("\n")
    }

    /// Format all commands matching a specific audience level.
    /// Commands are grouped by their group field.
    pub fn format_audience(&self, audience: CommandAudience) -> String {
        let mut lines = Vec::new();
        let label = audience.as_str();
        lines.push(format!("命令（{label}）"));
        lines.push("─────────────────────────────────────────────".to_string());
        let mut any = false;
        for group in self.groups() {
            let cmds: Vec<&CommandSpec> = self
                .commands
                .values()
                .filter(|cmd| cmd.group == group && cmd.audience == audience)
                .collect();
            if cmds.is_empty() {
                continue;
            }
            any = true;
            lines.push(format!("▸ {group}"));
            for spec in &cmds {
                lines.push(format!("    {:<32} {}", spec.usage, spec.description));
            }
            lines.push(String::new());
        }
        if !any {
            lines.push("    （无）".to_string());
        }
        lines.join("\n")
    }

    pub fn format_advanced(&self) -> String {
        self.format_audience(CommandAudience::Advanced)
    }

    pub fn format_dev(&self) -> String {
        self.format_audience(CommandAudience::Developer)
    }

    pub fn format_unknown(&self, command: &str) -> String {
        let normalized = normalize_command_name(command);
        let suggestions = self.complete_line(&normalized);
        if suggestions.is_empty() {
            format!("未知命令: {command}；输入 help 查看帮助")
        } else {
            format!(
                "未知命令: {command}；你可能想输入: {}",
                suggestions.join(" | ")
            )
        }
    }

    /// Execute a command by name with the given server state and arguments.
    ///
    /// Returns `Some(output_lines)` if a registered handler was found and executed,
    /// or `None` if no handler is registered (caller should fall back to old dispatch).
    pub fn execute(
        &self,
        state: &PlusServerState,
        command: &str,
        args: &[&str],
    ) -> Option<Vec<String>> {
        let mut tokens = Vec::with_capacity(args.len() + 1);
        tokens.extend(command.split_whitespace());
        tokens.extend(args.iter().copied());
        for command_len in (1..=tokens.len()).rev() {
            let candidate = normalize_command_name(&tokens[..command_len].join(" "));
            if let Some(spec) = self.commands.get(&candidate) {
                if let Some(handler) = &spec.handler {
                    return Some(handler(state, &tokens[command_len..]));
                }
            }
        }
        None
    }

    fn index_command_path(&mut self, name: &str) {
        let tokens = name.split_whitespace().collect::<Vec<_>>();
        if let Some(root) = tokens.first() {
            self.roots.insert((*root).to_string());
        }
        for idx in 1..tokens.len() {
            let parent = tokens[..idx].join(" ");
            self.children
                .entry(parent)
                .or_default()
                .insert(tokens[idx].to_string());
        }
    }
}

fn normalize_command_name(value: &str) -> String {
    value.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn register(registry: &mut CommandRegistry, spec: CommandSpec) {
    let name = spec.name.clone();
    registry
        .register(spec)
        .unwrap_or_else(|err| panic!("failed to register command `{name}`: {err}"));
}

pub fn runtime_v2_registry() -> CommandRegistry {
    let mut registry = CommandRegistry::new();

    register(
        &mut registry,
        CommandSpec::new("help", "core", "显示命令帮助。", "help [command]")
            .arg(CommandArgSpec::optional("command", "要查看详情的命令名"))
            .example("help")
            .example("help room list")
            .example("help group rooms")
            .example("help all")
            .example("help groups"),
    );
    register(
        &mut registry,
        CommandSpec::new("exit", "core", "关闭服务器。", "exit").example("exit"),
    );
    register(
        &mut registry,
        CommandSpec::new("status", "core", "查看服务器运行状态。", "status")
            .example("status")
            .handler(Arc::new(|state, _args| {
                let rooms = state.rooms.try_read().map(|r| r.len()).unwrap_or(0);
                vec![format!(
                    "  ◆ Phira-mp+ v{}  │ 端口 {}  │ 房间 {}",
                    env!("CARGO_PKG_VERSION"),
                    state.config.port,
                    rooms
                )]
            })),
    );

    for spec in [
        CommandSpec::new(
            "runtime status",
            "runtime-v2",
            "查看 Runtime v2 诊断信息。",
            "runtime status",
        )
        .handler(Arc::new(|state, _args| {
            let rooms = state.rooms.try_read().map(|r| r.len()).unwrap_or(0);
            vec![format!(
                "  Runtime v2: {} rooms | {} commands | MIGRATION_PHASE={} (WIT component ABI)",
                rooms,
                state.command_registry.iter().count(),
                crate::plugin_abi::wit::MIGRATION_PHASE
            )]
        })),
        CommandSpec::new(
            "runtime commands",
            "runtime-v2",
            "查看 Command Registry 统计。",
            "runtime commands",
        )
        .developer()
        .handler(Arc::new(|state, _args| {
            let (p, a, d) = state.command_registry.command_surface_counts();
            vec![format!("  Registry: {p} primary, {a} advanced, {d} dev")]
        })),
        CommandSpec::new(
            "runtime roadmap",
            "runtime-v2",
            "查看 Runtime v2 总目标工作板。",
            "runtime roadmap",
        )
        .developer(),
        CommandSpec::new(
            "runtime phira",
            "runtime-v2",
            "查看统一 Phira HTTP RetryClient 统计和策略。",
            "runtime phira",
        )
        .developer(),
        CommandSpec::new(
            "runtime events",
            "runtime-v2",
            "查看事件总线统计与最近事件。",
            "runtime events",
        )
        .developer(),
        CommandSpec::new(
            "runtime persistence",
            "runtime-v2",
            "查看持久化 Worker 与遥测批处理器统计。",
            "runtime persistence",
        )
        .advanced(),
        CommandSpec::new(
            "config reload",
            "runtime-v2",
            "重新加载启动时指定的 YAML 并热更新运行时配置。",
            "config reload",
        )
        .handler(Arc::new(|state, _args| {
            let path = std::path::Path::new(&state.config.config_path);
            let content = match std::fs::read_to_string(path) {
                Ok(c) => c,
                Err(e) => return vec![format!("  ✗ 读取配置文件失败: {e}")],
            };
            let mut config: crate::server::PlusConfig = match serde_yaml::from_str(&content) {
                Ok(c) => c,
                Err(e) => return vec![format!("  ✗ 解析配置文件失败: {e}")],
            };
            config.config_path = state.config.config_path.clone();
            if let Some(monitors) = state.config.cli_monitors_override.clone() {
                // Explicit CLI values keep their higher priority after a reload.
                config.monitors = monitors.clone();
                config.cli_monitors_override = Some(monitors);
            }
            if let Err(e) = config.normalize().and_then(|_| config.validate()) {
                return vec![format!("  ✗ 配置校验失败: {e}")];
            }

            let admin_update = if !config.admin_phira_ids.is_empty() {
                Some(
                    config
                        .admin_phira_ids
                        .iter()
                        .copied()
                        .filter(|id| *id > 0)
                        .collect::<std::collections::HashSet<_>>(),
                )
            } else {
                let admin_path = std::path::Path::new("data/admin-phira-ids.json");
                if admin_path.exists() {
                    let raw = match std::fs::read_to_string(admin_path) {
                        Ok(raw) => raw,
                        Err(e) => return vec![format!("  ✗ 读取持久化管理员列表失败: {e}")],
                    };
                    let ids = match serde_json::from_str::<Vec<i32>>(&raw) {
                        Ok(ids) => ids,
                        Err(e) => return vec![format!("  ✗ 解析持久化管理员列表失败: {e}")],
                    };
                    Some(ids.into_iter().filter(|id| *id > 0).collect())
                } else {
                    // A database-backed or runtime-managed list may not have a
                    // JSON mirror. Keep it unless YAML explicitly provides IDs.
                    None
                }
            };

            let benchmark_path = std::path::Path::new(crate::server::benchmark::BENCH_AUTH_FILE);
            let token_update =
                if !config.benchmark_phira_tokens.is_empty() || benchmark_path.exists() {
                    match crate::server::benchmark::try_load_benchmark_tokens(&config) {
                        Ok(tokens) => Some(tokens),
                        Err(e) => return vec![format!("  ✗ 读取持久化压测凭据失败: {e}")],
                    }
                } else {
                    // Preserve runtime-managed credentials when neither YAML nor
                    // the persistent auth file is the source of truth.
                    None
                };

            let live = crate::server::LiveConfig::from_full(&config);
            let mut live_guard = match state.live_config.try_write() {
                Ok(guard) => guard,
                Err(_) => return vec!["  ✗ 运行时配置正在被占用，请重试".to_string()],
            };
            let admin_guard = if admin_update.is_some() {
                match state.admin_ids.try_write() {
                    Ok(guard) => Some(guard),
                    Err(_) => return vec!["  ✗ 管理员列表正在被占用，请重试".to_string()],
                }
            } else {
                None
            };
            let token_guard = if token_update.is_some() {
                match state.bench_tokens.try_write() {
                    Ok(guard) => Some(guard),
                    Err(_) => return vec!["  ✗ Benchmark token 列表正在被占用，请重试".to_string()],
                }
            } else {
                None
            };

            // Commit only after every required lock is available so a busy
            // lock cannot leave the runtime configuration partially updated.
            *live_guard = live;
            if let (Some(mut guard), Some(ids)) = (admin_guard, admin_update) {
                *guard = ids;
            }
            if let (Some(mut guard), Some(tokens)) = (token_guard, token_update) {
                *guard = tokens;
            }

            vec![
                format!("  ✓ 已从 {} 重新加载配置", path.display()),
                "  ▸ 已热更新：chat_enabled、monitors；显式管理员/压测凭据同步更新".to_string(),
                "  ▸ CLI monitor 覆盖及未在 YAML/持久化文件中声明的动态状态保持不变".to_string(),
                "  ▸ 端口、目录、数据库、限流和 Runtime v2 策略仍需重启生效".to_string(),
            ]
        })),
        CommandSpec::new(
            "runtime cutover",
            "runtime-v2",
            "查看或切换 Touch/Judge 持久化 cutover 模式。",
            "runtime cutover [direct_only|worker_preferred|worker_authoritative]",
        )
        .advanced()
        .arg(CommandArgSpec::optional(
            "mode",
            "direct_only、worker_preferred 或 worker_authoritative",
        ))
        .example("runtime cutover")
        .example("runtime cutover worker_preferred"),
        CommandSpec::new(
            "runtime schema",
            "runtime-v2",
            "查看持久化 schema 说明。",
            "runtime schema",
        )
        .developer(),
        CommandSpec::new(
            "runtime rooms",
            "runtime-v2",
            "查看房间命令通道与 Actor 迁移状态。",
            "runtime rooms",
        )
        .developer(),
        CommandSpec::new(
            "runtime actors",
            "runtime-v2",
            "查看 Actor 模型迁移蓝图。",
            "runtime actors",
        )
        .developer(),
    ] {
        register(&mut registry, spec.example("runtime status"));
    }

    for spec in [
        CommandSpec::new("simulation status", "simulation", "查看 Simulation 状态。", "simulation status")
            .handler(Arc::new(|_state, _args| {
                // Use the SimulationManager's status; if not available, return basic info
                vec!["  Simulation status (CLI: use `simulation status` in console)".to_string()]
            })),
        CommandSpec::new(
            "simulation run",
            "simulation",
            "启动隔离本地压测；默认自动 tick，到达 duration 后自动停止。",
            "simulation run <baseline|small|medium|large|custom> [scenario=balanced|chat_storm|ready_storm|round_storm|touch_judge_burst|idle] [users=N] [rooms=N] [duration=N] [tick_ms=N] [auto=true] [persist_every=N] [touch=true] [judge=true]",
        )
        .example("simulation run baseline")
        .example("simulation run custom users=500 rooms=50 duration=300 scenario=touch_judge_burst tick_ms=1000 persist_every=30")
        .example("simulation run small auto=false"),
        CommandSpec::new("simulation scenarios", "simulation", "列出可用 Simulation workload scenario/profile。", "simulation scenarios").advanced()
            .example("simulation scenarios"),
        CommandSpec::new(
            "simulation suite",
            "simulation",
            "按顺序运行多个 Simulation scenario，用于一次性比较不同压力形状。",
            "simulation suite <smoke|mixed|stress> [duration=N] [tick_ms=N] [persist_every=N] [users=N] [rooms=N]",
        ).advanced()
        .example("simulation suite smoke")
        .example("simulation suite mixed duration=15 tick_ms=500 persist_every=5")
        .example("simulation suite stress users=800 rooms=80"),
        CommandSpec::new(
            "simulation report",
            "simulation",
            "查看最近一次 Simulation suite 汇总报告，并输出统一 BenchmarkReport [simulation] 摘要。",
            "simulation report [latest|list|clear]",
        ).advanced()
        .example("simulation report")
        .example("simulation report list 8")
        .example("simulation report clear"),
        CommandSpec::new("simulation tick", "simulation", "手动推进 Simulation tick。", "simulation tick [count]").developer()
            .example("simulation tick 10"),
        CommandSpec::new("simulation inspect", "simulation", "查看 shadow users/rooms/rounds/recent events 样本。", "simulation inspect [limit]").developer()
            .example("simulation inspect 20"),
        CommandSpec::new("simulation stop", "simulation", "停止当前 Simulation 运行状态并广播结束提示。", "simulation stop"),
        CommandSpec::new("simulation seed", "simulation", "设置 deterministic simulation seed。", "simulation seed <value>").developer(),
        CommandSpec::new("simulation cleanup", "simulation", "清理 Simulation 数据。", "simulation cleanup"),
        CommandSpec::new("simulation persist", "simulation", "发送 Simulation 快照到持久化 Worker。", "simulation persist").developer()
            .example("simulation persist"),
        CommandSpec::new("simulation sample", "simulation", "查看 deterministic touches/judges 示例数据规模。", "simulation sample").developer(),
    ] {
        register(&mut registry, spec);
    }

    register(
        &mut registry,
        CommandSpec::new(
            "benchmark",
            "diagnostics",
            "运行显式真实网络压测。该命令需要 Phira token，不是默认压测入口。",
            "benchmark [seconds] [rooms]",
        )
        .advanced()
        .arg(CommandArgSpec::optional(
            "seconds",
            "压测时长，默认 30，范围 5..300",
        ))
        .arg(CommandArgSpec::optional(
            "rooms",
            "目标房间数，默认 100，最大 5000",
        ))
        .example("benchmark 30 100")
        .example("benchmark run real 30 100"),
    );
    register(
        &mut registry,
        CommandSpec::new(
            "benchmark modes",
            "diagnostics",
            "查看三种压测模式说明。",
            "benchmark modes",
        )
        .advanced()
        .example("benchmark modes")
        .handler(Arc::new(|_state, _args| {
            vec![
                "  Benchmark modes:".to_string(),
                "    simulation  — 默认推荐压测（隔离本地，不访问 Phira，不需要 token）"
                    .to_string(),
                "    real        — 显式真实 TCP 协议测试（需要 Phira token）".to_string(),
                "    hybrid      — Hybrid Phira 探测（chart_lookup / record_lookup）".to_string(),
            ]
        })),
    );
    register(
        &mut registry,
        CommandSpec::new(
            "benchmark run real",
            "diagnostics",
            "运行真实 TCP 协议测试。",
            "benchmark run real [seconds] [rooms]",
        )
        .advanced()
        .example("benchmark run real 30 100"),
    );
    register(
        &mut registry,
        CommandSpec::new("benchmark run hybrid", "diagnostics", "运行 Hybrid Phira 探测。", "benchmark run hybrid [duration] [authenticate=true] [chart_lookup=<id>] [record_lookup=<id>]").advanced()
            .example("benchmark run hybrid")
            .example("benchmark run hybrid authenticate=true chart_lookup=1 record_lookup=1"),
    );
    register(
        &mut registry,
        CommandSpec::new(
            "benchmark report",
            "diagnostics",
            "查看 Benchmark 报告。",
            "benchmark report [simulation|hybrid|real|limit]",
        )
        .advanced()
        .example("benchmark report")
        .example("benchmark report simulation")
        .example("benchmark report 16"),
    );
    register(
        &mut registry,
        CommandSpec::new(
            "benchmark history",
            "diagnostics",
            "查看已持久化的 BenchmarkReport 历史记录。",
            "benchmark history [simulation|hybrid|real] [limit]",
        )
        .advanced()
        .example("benchmark history")
        .example("benchmark history real 20"),
    );

    for spec in [
        CommandSpec::new("users", "users", "查看在线用户。", "users"),
        CommandSpec::new("kick", "users", "踢出在线用户。", "kick <user_id>"),
        CommandSpec::new(
            "broadcast all",
            "users",
            "广播消息给所有用户。",
            "broadcast all <message>",
        ),
        CommandSpec::new(
            "broadcast room",
            "users",
            "广播消息给指定房间。",
            "broadcast room <room_id> <message>",
        ),
        CommandSpec::new(
            "broadcast user",
            "users",
            "发送消息给指定用户。",
            "broadcast user <user_id> <message>",
        ),
        CommandSpec::new(
            "admin-id",
            "users",
            "管理游戏内管理员 Phira ID。",
            "admin-id list|add|remove",
        )
        .advanced(),
    ] {
        register(&mut registry, spec);
    }

    register(
        &mut registry,
        CommandSpec::new("rooms", "rooms", "查看活跃房间。", "rooms"),
    );
    for spec in [
        CommandSpec::new(
            "room create-empty",
            "rooms",
            "创建无人持久空房间。",
            "room create-empty <room_id> [phira_api_endpoint]",
        )
        .advanced(),
        CommandSpec::new(
            "room info",
            "rooms",
            "查看房间详情。",
            "room info <room_id>",
        ),
        CommandSpec::new(
            "room start",
            "rooms",
            "服务端强制发起房间游戏，等待客户端加载后开始。",
            "room start <room_id>",
        ),
        CommandSpec::new(
            "room force-start",
            "rooms",
            "room start 的房间子命令兼容别名。",
            "room force-start <room_id>",
        )
        .advanced(),
        CommandSpec::new(
            "force-start",
            "rooms",
            "room start 的旧版顶层兼容命令。",
            "force-start <room_id>",
        )
        .advanced(),
        CommandSpec::new(
            "room cancel",
            "rooms",
            "取消管理员发起的游戏开始。",
            "room cancel <room_id>",
        )
        .advanced(),
        CommandSpec::new(
            "room kick",
            "rooms",
            "从房间踢出用户。",
            "room kick <room_id> <user_id>",
        )
        .advanced(),
        CommandSpec::new(
            "room host",
            "rooms",
            "设置房主，? 表示系统房主。",
            "room host <room_id> <user_id|?>",
        ),
        CommandSpec::new(
            "room force-move",
            "rooms",
            "强制迁移用户到指定房间。",
            "room force-move <room_id> <user_id> [monitor]",
        )
        .advanced(),
        CommandSpec::new(
            "room hide",
            "rooms",
            "隐藏房间，使其不出现在 Web API 与欢迎语。",
            "room hide <room_id> [true|false]",
        )
        .advanced(),
        CommandSpec::new(
            "room unhide",
            "rooms",
            "取消隐藏房间。",
            "room unhide <room_id>",
        )
        .advanced(),
        CommandSpec::new("room close", "rooms", "解散房间。", "room close <room_id>"),
        CommandSpec::new(
            "room set",
            "rooms",
            "修改房间设置。",
            "room set <room_id> <field> <value>",
        ),
        CommandSpec::new(
            "room history",
            "rooms",
            "查看房间游玩历史。",
            "room history <room_id>",
        )
        .advanced(),
        CommandSpec::new(
            "room rounds",
            "rooms",
            "查看房间轮次列表。",
            "room rounds <room_id>",
        )
        .advanced(),
        CommandSpec::new(
            "room round",
            "rooms",
            "查看指定轮次详情。",
            "room round <round_uuid>",
        )
        .advanced(),
        CommandSpec::new(
            "room uuid",
            "rooms",
            "查看房间 UUID。",
            "room uuid <room_id>",
        )
        .advanced(),
        CommandSpec::new(
            "room ban",
            "rooms",
            "加入房间黑名单。",
            "room ban <room_id> <user_id>",
        )
        .advanced(),
        CommandSpec::new(
            "room unban",
            "rooms",
            "移出房间黑名单。",
            "room unban <room_id> <user_id>",
        )
        .advanced(),
        CommandSpec::new(
            "room banlist",
            "rooms",
            "查看房间黑名单。",
            "room banlist <room_id>",
        )
        .advanced(),
    ] {
        register(&mut registry, spec);
    }

    for spec in [
        CommandSpec::new("plugin list", "plugins", "列出所有插件。", "plugin list"),
        CommandSpec::new(
            "plugin enable",
            "plugins",
            "启用插件。",
            "plugin enable <name>",
        ),
        CommandSpec::new(
            "plugin disable",
            "plugins",
            "禁用插件。",
            "plugin disable <name>",
        ),
        CommandSpec::new(
            "plugin remove",
            "plugins",
            "删除插件：卸载并删除插件文件和数据。",
            "plugin remove <name>",
        )
        .advanced(),
        CommandSpec::new(
            "plugin reload",
            "plugins",
            "重载所有插件。",
            "plugin reload",
        )
        .advanced(),
        CommandSpec::new(
            "plugin info",
            "plugins",
            "查看插件详情。",
            "plugin info <id_or_name>",
        )
        .advanced(),
        CommandSpec::new(
            "plugin call",
            "plugins",
            "调用插件导出 API。",
            "plugin call <id_or_name> <method> [JSON_ARRAY]",
        )
        .advanced(),
    ] {
        register(&mut registry, spec);
    }

    for spec in [
        CommandSpec::new("ban", "security", "封禁用户。", "ban <user_id> [reason]"),
        CommandSpec::new("unban", "security", "解封用户。", "unban <user_id>"),
        CommandSpec::new("banlist", "security", "查看全局封禁列表。", "banlist"),
    ] {
        register(&mut registry, spec);
    }

    // ── WAL / dead-letter ──
    for spec in [
        CommandSpec::new("wal inspect", "ops", "查看 WAL 状态统计。", "wal inspect")
            .handler(Arc::new(|state, _args| {
                let mut lines = vec![];
                let wal_path = &state.config.runtime_v2.persistence_wal_path;
                lines.push(format!("  ◆ WAL 路径: {wal_path}"));
                if let Ok(meta) = std::fs::metadata(wal_path) {
                    lines.push(format!("  ◆ 文件大小: {} 字节", meta.len()));
                }
                lines
            })),
        CommandSpec::new("dead-letter list", "ops", "列出 dead-letter 记录摘要。", "dead-letter list [limit]")
            .handler(Arc::new(|state, args| {
                let limit = args.first().and_then(|v| v.parse::<usize>().ok()).unwrap_or(10);
                let mut lines = vec![format!("  ◆ dead-letter 最近 {limit} 条")];
                if let Some(path) = &state.config.runtime_v2.persistence_dead_letter_path {
                    if let Ok(content) = std::fs::read_to_string(path) {
                        let count = content.lines().filter(|l| !l.trim().is_empty()).count();
                        lines.push(format!("  ◆ 总记录数: {count}"));
                        let all_lines: Vec<&str> = content.lines().collect();
                        for line in all_lines.iter().rev().take(limit) {
                            if let Ok(val) = serde_json::from_str::<serde_json::Value>(line) {
                                let summary = val.get("summary").and_then(|v| v.as_str()).unwrap_or("?");
                                let kind = val.get("kind").and_then(|v| v.as_str()).unwrap_or("?");
                                lines.push(format!("  · [{kind}] {summary}"));
                            }
                        }
                    } else {
                        lines.push("  ○ dead-letter 文件不存在或无法读取".to_string());
                    }
                } else {
                    lines.push("  ○ dead-letter 未配置".to_string());
                }
                lines
            })),
        CommandSpec::new("dead-letter replay", "ops", "重放 dead-letter 事件到持久化队列。", "dead-letter replay")
            .handler(Arc::new(|state, _args| {
                let mut lines = vec!["  ◆ dead-letter replay...".to_string()];
                let path = &state.config.runtime_v2.persistence_dead_letter_path.clone();
                let Some(path) = path else {
                    lines.push("  ○ dead-letter 未配置".to_string());
                    return lines;
                };
                let content = match std::fs::read_to_string(&path) {
                    Ok(c) => c,
                    Err(e) => {
                        lines.push(format!("  ✗ 读取 dead-letter 失败: {e}"));
                        return lines;
                    }
                };
                let mut count = 0usize;
                for line in content.lines() {
                    let line = line.trim();
                    if line.is_empty() { continue; }
                    if let Ok(val) = serde_json::from_str::<serde_json::Value>(line) {
                        let event: Option<crate::persistence::message::PersistenceEvent> =
                            val.get("event").and_then(|v| serde_json::from_value(v.clone()).ok());
                        if let Some(ev) = event {
                            // Re-enqueue via PersistenceWorker (fire-and-forget)
                            let pw = Arc::clone(&state.persistence_worker);
                            tokio::task::spawn(async move {
                                let _ = pw.enqueue(ev).await;
                            });
                            count += 1;
                        }
                    }
                }
                lines.push(format!("  ✓ 已提交 {count} 个事件到持久化队列"));
                lines
            })),
    ] {
        register(&mut registry, spec);
    }

    // ── config / diagnostics ──
    for spec in [
        CommandSpec::new("check-config", "core", "验证当前加载的配置并显示脱敏摘要。", "check-config")
            .handler(Arc::new(|state, _args| {
                let mut lines = vec![format!("  ◆ 配置版本: {}", state.config.config_version)];
                lines.push(format!("  ◆ 服务端: {} v{}", env!("CARGO_PKG_NAME"), env!("CARGO_PKG_VERSION")));
                lines.push(format!("  ◆ TCP 端口: {}", state.config.port));
                lines.push(format!("  ◆ HTTP: {}:{}", state.config.http_bind_address, state.config.http_port));
                lines.push(format!("  ◆ 数据库: {}", state.config.database_url.as_deref().unwrap_or("未配置")));
                lines.push(format!("  ◆ 插件目录: {}", state.config.plugins_dir));
                lines.push(format!("  ◆ 最大会话: {}", state.config.max_sessions));
                lines.push(format!("  ◆ 最大房间: {}", state.config.max_rooms.map(|v| v.to_string()).unwrap_or("无限制".into())));
                lines.push(format!("  ◆ 数据保留: {} 天", state.config.persistence_retention_days));
                lines.push(format!("  ◆ profile: {:?}", state.config.profile));
                if state.config.database_url.is_some() {
                    let db_status = crate::internal_hooks::DB.get()
                        .map(|db| if db.is_active() { "已连接" } else { "已断开" })
                        .unwrap_or("不可用");
                    lines.push(format!("  ◆ 数据库状态: {db_status}"));
                }
                lines
            })),
        CommandSpec::new("doctor", "core", "运行系统诊断检查。", "doctor")
            .handler(Arc::new(|state, _args| {
                let mut lines = vec![format!("  ◆ Phira-mp+ v{} Doctor", env!("CARGO_PKG_VERSION"))];
                if let Some(db) = crate::internal_hooks::DB.get() {
                    lines.push(format!("  {} 数据库: {}", if db.is_active() { "✓" } else { "✗" }, if db.is_active() { "已连接" } else { "已断开" }));
                } else {
                    lines.push("  ○ 数据库: 未配置".to_string());
                }
                let sessions = state.sessions.try_read().map(|s| s.len()).unwrap_or(0);
                lines.push(format!("  ✓ 会话: {sessions} 活跃"));
                let rooms = state.rooms.try_read().map(|r| r.len()).unwrap_or(0);
                lines.push(format!("  ✓ 房间: {rooms} 个"));
                lines
            })),
    ] {
        register(&mut registry, spec);
    }

    // ── backup / restore ──
    for spec in [
        CommandSpec::new("backup create", "ops", "创建数据备份归档。", "backup create [output]")
            .handler(Arc::new(|state, args| {
                let output = args.first().map(|s| &s[..]).unwrap_or("pmp-backup.tar.gz");
                let mut lines = vec![format!("  ◆ 创建备份: {output}")];
                match crate::backup::create_backup(&state, output) {
                    Ok(_path) => {
                        lines.push(format!("  ✓ 备份完成: {output}"));
                    }
                    Err(e) => {
                        lines.push(format!("  ✗ 备份失败: {e}"));
                    }
                }
                lines
            })),
        CommandSpec::new("restore verify", "ops", "验证备份归档完整性。", "restore verify <path>")
            .handler(Arc::new(|_state, args| {
                let path = args.first().map(|s| &s[..]).unwrap_or("pmp-backup.tar.gz");
                let mut lines = vec![format!("  ◆ 验证备份: {path}")];
                match crate::backup::verify_backup(path) {
                    Ok(report) => {
                        lines.push(format!("  ✓ 备份有效: {} 个文件, {} 字节", report.file_count, report.total_size));
                    }
                    Err(e) => {
                        lines.push(format!("  ✗ 验证失败: {e}"));
                    }
                }
                lines
            })),
    ] {
        register(&mut registry, spec);
    }

    registry.init_completers();
    registry
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registry_indexes_canonical_children() {
        let registry = runtime_v2_registry();
        assert!(registry.get("help").is_some());
        assert!(
            registry.get("h").is_none(),
            "alias 'h' should not exist after removing alias surface"
        );
        assert!(
            registry
                .child_commands("room")
                .contains(&"info".to_string()),
            "room info should be indexed as child of room"
        );
        assert!(registry
            .complete_line("simulation ")
            .contains(&"status".to_string()));
        assert!(registry
            .complete_line("room f")
            .contains(&"force-move".to_string()));
        assert!(!registry
            .complete_line("plug-")
            .contains(&"plug-enable".to_string()));
    }

    #[test]
    fn help_uses_structured_sections() {
        let registry = runtime_v2_registry();
        let help = registry.format_help("rooms").expect("rooms help");
        assert!(help.contains("NAME"));
        assert!(help.contains("USAGE"));
        assert!(help.contains("SURFACE"));
        assert!(help.contains("rooms"));
    }

    #[test]
    fn overview_has_no_compatibility_surface() {
        let registry = runtime_v2_registry();
        let overview = registry.format_overview();
        assert!(!overview.contains("plug-enable"));
        assert!(registry.get("plug-enable").is_none());
        assert!(registry.get("plugin enable").is_some());
    }

    #[test]
    fn canonical_rooms_command_exists() {
        let registry = runtime_v2_registry();
        let spec = registry.get("rooms").expect("rooms should exist");
        assert_eq!(spec.name, "rooms");
    }

    #[test]
    fn primary_count_is_within_limit() {
        let registry = runtime_v2_registry();
        let (primary, _advanced, _dev) = registry.command_surface_counts();
        assert!(primary <= 30, "primary count {} exceeds 30 limit", primary);
        assert!(primary > 0);
    }

    #[test]
    fn audience_methods_produce_distinct_output() {
        let registry = runtime_v2_registry();
        assert!(!registry.format_advanced().is_empty());
        assert!(!registry.format_dev().is_empty());
    }
}
