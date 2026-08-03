//! Command metadata registry for Runtime.
//!
//! This module stores command metadata, help text, completion hints, and
//! optional execution handlers. The existing CLI dispatcher remains the
//! fallback, but new commands should register a handler here to converge
//! CLI/TUI/admin_/WIT execution on a single path.

use std::collections::{BTreeMap, BTreeSet};
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use crate::server::PlusServerState;

/// Handler signature for a registered CLI command.
///
/// Commands receive the server state plus the remaining argument tokens and
/// return the output lines to display. Handlers may be async; the returned
/// future is `'static` so the handler can move owned state into it.
pub type CommandHandler = Arc<
    dyn Fn(&Arc<PlusServerState>, &[&str]) -> Pin<Box<dyn Future<Output = Vec<String>> + Send>>
        + Send
        + Sync,
>;

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
    /// Internal developer commands (runtime internals, diagnostics).
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
            return self.complete_root(tokens.first().copied().unwrap_or(""));
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

    /// Install argument completers. Called from `runtime_registry()`.
    /// Dynamic completers (room IDs, user IDs) reference server state via
    /// `Weak` and are only active while the server is running.
    pub fn init_completers(&self) {
        self.set_arg_completer(
            "benchmark run",
            Arc::new(|_cmd, prefix| {
                let mut candidates: Vec<&str> = vec![
                    "real",
                    "--mode",
                    "--scenario",
                    "--preset",
                    "--clients",
                    "--rooms",
                    "--duration",
                    "--seed",
                    "--output",
                ];
                candidates.retain(|c| c.starts_with(prefix));
                candidates.into_iter().map(|s| s.to_string()).collect()
            }),
        );

        self.set_arg_completer(
            "benchmark suite",
            Arc::new(|_cmd, prefix| {
                ["--preset"]
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
        lines.push("-".repeat(50));
        lines.push("提示：help <命令> 查看详情".to_string());
        lines.push("提示：游戏内管理员入口仍使用 _ 命令，__ 表示字面量下划线".to_string());
        lines.push(String::new());

        for group in self.groups() {
            let visible: Vec<&CommandSpec> = self
                .commands
                .values()
                .filter(|cmd| cmd.group == group)
                .collect();
            if visible.is_empty() {
                continue;
            }
            lines.push(format!("> {group}"));
            for spec in &visible {
                let marker = match spec.audience {
                    CommandAudience::Primary => " ",
                    CommandAudience::Advanced => "advanced",
                    CommandAudience::Developer => "dev",
                };
                lines.push(format!(
                    "    {:<32} {:<9} {}",
                    spec.usage, marker, spec.description
                ));
            }
            lines.push(String::new());
        }

        lines.push("-".repeat(50));
        lines.push("help <命令> 查看详情".to_string());
        lines.join("\n")
    }

    pub fn format_overview_all(&self) -> String {
        let mut lines = Vec::new();
        let (primary, advanced, developer) = self.command_surface_counts();
        lines.push("Phira-mp+ 管理命令（完整视图）".to_string());
        lines.push("-".repeat(50));
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
        lines.push("-".repeat(50));
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
        lines.push("-".repeat(50));
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
    pub fn format_audience(&self, audience: CommandAudience) -> String {
        let mut lines = Vec::new();
        let label = audience.as_str();
        lines.push(format!("命令（{label}）"));
        lines.push("-".repeat(50));
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
            lines.push(format!("> {group}"));
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
    /// or `None` if no handler is registered (caller should fall back to plugin/unknown).
    pub async fn execute(
        &self,
        state: &Arc<PlusServerState>,
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
                    return Some(handler(state, &tokens[command_len..]).await);
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

/// Construct a fully-populated `CommandRegistry` with every registered
/// command spec.  Called once at server startup.
pub fn runtime_registry() -> CommandRegistry {
    let mut registry = CommandRegistry::new();

    for spec in crate::cli::commands::specs::all_specs() {
        let name = spec.name.clone();
        registry
            .register(spec)
            .unwrap_or_else(|err| panic!("failed to register command `{name}`: {err}"));
    }

    registry.init_completers();
    registry
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registry_indexes_canonical_children() {
        let registry = runtime_registry();
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
            .complete_line("room f")
            .contains(&"force-move".to_string()));
        assert!(!registry
            .complete_line("plug-")
            .contains(&"plug-enable".to_string()));
    }

    #[test]
    fn help_uses_structured_sections() {
        let registry = runtime_registry();
        let help = registry.format_help("rooms").expect("rooms help");
        assert!(help.contains("NAME"));
        assert!(help.contains("USAGE"));
        assert!(help.contains("SURFACE"));
        assert!(help.contains("rooms"));
    }

    #[test]
    fn overview_has_no_compatibility_surface() {
        let registry = runtime_registry();
        let overview = registry.format_overview();
        assert!(!overview.contains("plug-enable"));
        assert!(registry.get("plug-enable").is_none());
        assert!(registry.get("plugin enable").is_some());
    }

    #[test]
    fn canonical_rooms_command_exists() {
        let registry = runtime_registry();
        let spec = registry.get("rooms").expect("rooms should exist");
        assert_eq!(spec.name, "rooms");
    }

    #[test]
    fn primary_count_is_within_limit() {
        let registry = runtime_registry();
        let (primary, _advanced, _dev) = registry.command_surface_counts();
        assert!(primary <= 40, "primary count {} exceeds 40 limit", primary);
        assert!(primary > 0);
    }

    #[test]
    fn audience_methods_produce_distinct_output() {
        let registry = runtime_registry();
        assert!(!registry.format_advanced().is_empty());
        assert!(!registry.format_dev().is_empty());
    }
}
