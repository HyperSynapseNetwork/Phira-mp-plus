//! Command metadata registry for Runtime v2.
//!
//! This module stores command metadata, help text and completion hints only.
//! The existing CLI dispatcher remains the source of truth for command
//! execution until commands are migrated one by one. Keeping metadata separate
//! lets TUI completion, in-game admin commands, WIT APIs and future Web admin
//! APIs converge on the same usage model without a risky rewrite of `cli.rs`.

use std::collections::{BTreeMap, BTreeSet};

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

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandSpec {
    pub name: String,
    pub group: String,
    pub description: String,
    pub usage: String,
    pub args: Vec<CommandArgSpec>,
    pub examples: Vec<String>,
    pub aliases: Vec<String>,
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
            aliases: Vec::new(),
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

    pub fn alias(mut self, alias: impl Into<String>) -> Self {
        self.aliases.push(alias.into());
        self
    }

    pub fn aliases<I, S>(mut self, aliases: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.aliases.extend(aliases.into_iter().map(Into::into));
        self
    }
}

#[derive(Debug, Clone, Default)]
pub struct CommandRegistry {
    commands: BTreeMap<String, CommandSpec>,
    aliases: BTreeMap<String, String>,
    roots: BTreeSet<String>,
    children: BTreeMap<String, BTreeSet<String>>,
}

impl CommandRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn register(&mut self, spec: CommandSpec) -> Result<(), String> {
        let name = normalize_command_name(&spec.name);
        if name.is_empty() {
            return Err("command name cannot be empty".to_string());
        }
        if self.commands.contains_key(&name) || self.aliases.contains_key(&name) {
            return Err(format!("duplicated command name: {name}"));
        }

        for alias in &spec.aliases {
            let alias = normalize_command_name(alias);
            if alias.is_empty() {
                return Err(format!("empty alias for command {name}"));
            }
            if self.commands.contains_key(&alias) || self.aliases.contains_key(&alias) {
                return Err(format!("duplicated command alias: {alias}"));
            }
        }

        self.index_command_path(&name);
        for alias in &spec.aliases {
            let alias = normalize_command_name(alias);
            self.index_command_path(&alias);
            self.aliases.insert(alias, name.clone());
        }

        let mut spec = spec;
        spec.name = name.clone();
        self.commands.insert(name, spec);
        Ok(())
    }

    pub fn get(&self, name_or_alias: &str) -> Option<&CommandSpec> {
        let canonical = self.canonical_name(name_or_alias);
        self.commands.get(canonical.as_str())
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
        let prefix = if ends_with_space { "" } else { tokens.last().copied().unwrap_or("") };

        let mut out = self
            .children
            .get(&parent)
            .into_iter()
            .flat_map(|children| children.iter())
            .filter(|child| child.starts_with(prefix))
            .cloned()
            .collect::<Vec<_>>();

        // Fallback: if the exact parent has no children, try completing any
        // full command whose final token starts with the current prefix and the
        // preceding tokens match. This makes aliases and old one-off commands
        // still discoverable.
        if out.is_empty() {
            let parent_prefix = if parent.is_empty() {
                String::new()
            } else {
                format!("{parent} ")
            };
            out = self
                .commands
                .keys()
                .chain(self.aliases.keys())
                .filter_map(|name| name.strip_prefix(&parent_prefix))
                .filter(|rest| !rest.contains(' ') && rest.starts_with(prefix))
                .map(ToOwned::to_owned)
                .collect::<BTreeSet<_>>()
                .into_iter()
                .collect();
        }

        out
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

        if !spec.args.is_empty() {
            lines.push(String::new());
            lines.push("ARGS".to_string());
            for arg in &spec.args {
                let marker = if arg.required { "required" } else { "optional" };
                lines.push(format!("    {:<18} {:<8} {}", arg.name, marker, arg.description));
            }
        }

        if !spec.examples.is_empty() {
            lines.push(String::new());
            lines.push("EXAMPLES".to_string());
            for example in &spec.examples {
                lines.push(format!("    {example}"));
            }
        }

        if !spec.aliases.is_empty() {
            lines.push(String::new());
            lines.push("ALIASES".to_string());
            lines.push(format!("    {}", spec.aliases.join(", ")));
        }

        Some(lines.join("\n"))
    }

    pub fn format_overview(&self) -> String {
        let mut lines = Vec::new();
        lines.push("Phira-mp+ 管理命令".to_string());
        lines.push("─────────────────────────────────────────────".to_string());
        lines.push("提示：help <命令> 可查看统一格式详情，例如 help room list".to_string());
        lines.push("提示：游戏内管理员入口仍使用 _ 命令，__ 表示字面量下划线".to_string());
        lines.push(String::new());

        for group in self.groups() {
            lines.push(format!("▸ {group}"));
            for spec in self.commands.values().filter(|spec| spec.group == group) {
                lines.push(format!("    {:<28} {}", spec.usage, spec.description));
            }
            lines.push(String::new());
        }

        lines.push("─────────────────────────────────────────────".to_string());
        lines.join("\n")
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

    fn canonical_name(&self, name_or_alias: &str) -> String {
        let normalized = normalize_command_name(name_or_alias);
        self.aliases
            .get(&normalized)
            .cloned()
            .unwrap_or(normalized)
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
    // Runtime v2 metadata should not make server startup fragile. If a duplicate
    // slips in, keep the rest of the registry usable and let tests/CI catch it.
    let _ = registry.register(spec);
}

pub fn runtime_v2_registry() -> CommandRegistry {
    let mut registry = CommandRegistry::new();

    register(
        &mut registry,
        CommandSpec::new("help", "core", "显示命令帮助。", "help [command]")
            .alias("h")
            .alias("?")
            .arg(CommandArgSpec::optional("command", "要查看详情的命令名或别名"))
            .example("help")
            .example("help room list"),
    );
    register(
        &mut registry,
        CommandSpec::new("exit", "core", "关闭服务器。", "exit")
            .aliases(["quit", "q"])
            .example("exit"),
    );
    register(
        &mut registry,
        CommandSpec::new("status", "core", "查看服务器运行状态。", "status")
            .alias("st")
            .example("status"),
    );

    for spec in [
        CommandSpec::new("runtime status", "runtime-v2", "查看 Runtime v2 骨架状态。", "runtime status"),
        CommandSpec::new("runtime commands", "runtime-v2", "查看 Command Registry 统计。", "runtime commands"),
        CommandSpec::new("runtime events", "runtime-v2", "查看 EventBus 发布统计与最近事件。", "runtime events"),
        CommandSpec::new("runtime persistence", "runtime-v2", "查看 Persistence Worker 队列统计。", "runtime persistence"),
    ] {
        register(&mut registry, spec.example("runtime status"));
    }

    for spec in [
        CommandSpec::new("simulation status", "simulation", "查看 Runtime v2 Simulation 状态。", "simulation status"),
        CommandSpec::new(
            "simulation run",
            "simulation",
            "启动隔离 shadow world；默认自动 tick，到达 duration 后自动停止。",
            "simulation run <baseline|small|medium|large|custom> [users=N] [rooms=N] [duration=N] [tick_ms=N] [auto=true] [persist_every=N] [touch=true] [judge=true]",
        )
        .example("simulation run baseline")
        .example("simulation run custom users=500 rooms=50 duration=300 tick_ms=1000 persist_every=30")
        .example("simulation run small auto=false"),
        CommandSpec::new("simulation tick", "simulation", "手动推进 deterministic shadow world tick；auto=false 时常用。", "simulation tick [count]")
            .alias("simulation advance")
            .example("simulation tick 10"),
        CommandSpec::new("simulation inspect", "simulation", "查看 shadow users/rooms/rounds/recent events 样本。", "simulation inspect [limit]")
            .aliases(["simulation world", "simulation rooms", "simulation users"])
            .example("simulation inspect 20"),
        CommandSpec::new("simulation stop", "simulation", "停止当前 Simulation 运行状态并广播结束提示。", "simulation stop"),
        CommandSpec::new("simulation seed", "simulation", "设置 deterministic simulation seed。", "simulation seed <value>"),
        CommandSpec::new("simulation cleanup", "simulation", "清理 Runtime v2 Simulation shadow world。", "simulation cleanup"),
        CommandSpec::new("simulation persist", "simulation", "将当前 shadow world snapshot 发送到 EventBus / PersistenceWorker 的 simulation 专用路径。", "simulation persist")
            .alias("simulation snapshot")
            .example("simulation persist"),
        CommandSpec::new("simulation sample", "simulation", "查看 deterministic touches/judges 示例数据规模。", "simulation sample"),
    ] {
        register(&mut registry, spec);
    }

    register(
        &mut registry,
        CommandSpec::new(
            "benchmark",
            "diagnostics",
            "运行显式真实网络压测。该命令需要 Phira token，不是 Runtime v2 默认压测入口。",
            "benchmark [seconds] [rooms]",
        )
        .alias("bench")
        .arg(CommandArgSpec::optional("seconds", "压测时长，默认 30，范围 5..300"))
        .arg(CommandArgSpec::optional("rooms", "目标房间数，默认 100，最大 5000"))
        .example("benchmark 30 100"),
    );
    register(
        &mut registry,
        CommandSpec::new("benchmark-bind", "diagnostics", "绑定真实网络压测使用的 Phira token。", "benchmark-bind <token1[,token2...]>")
            .arg(CommandArgSpec::required("token", "Phira token；不要提交到 Git")),
    );
    register(
        &mut registry,
        CommandSpec::new("benchmark-cleanup", "diagnostics", "清理 bench-* 压测房间。", "benchmark-cleanup"),
    );

    for spec in [
        CommandSpec::new("users", "users", "查看在线用户。", "users").alias("u"),
        CommandSpec::new("kick", "users", "踢出用户，兼容旧语法。", "kick <user_id> | kick <room_id> <user_id>"),
        CommandSpec::new("broadcast all", "users", "广播消息给所有用户。", "broadcast all <message>"),
        CommandSpec::new("broadcast room", "users", "广播消息给指定房间。", "broadcast room <room_id> <message>"),
        CommandSpec::new("broadcast user", "users", "发送消息给指定用户。", "broadcast user <user_id> <message>"),
        CommandSpec::new("admin-id", "users", "管理游戏内管理员 Phira ID。", "admin-id list|add|remove"),
    ] {
        register(&mut registry, spec);
    }

    register(
        &mut registry,
        CommandSpec::new("rooms", "rooms", "查看活跃房间。", "rooms").alias("r"),
    );
    for spec in [
        CommandSpec::new("room list", "rooms", "查看活跃房间。", "room list"),
        CommandSpec::new("room create-empty", "rooms", "创建无人持久空房间。", "room create-empty <room_id> [phira_api_endpoint]"),
        CommandSpec::new("room info", "rooms", "查看房间详情。", "room info <room_id>"),
        CommandSpec::new("room start", "rooms", "强制开始房间。", "room start <room_id>"),
        CommandSpec::new("room cancel", "rooms", "取消房间开始状态。", "room cancel <room_id>"),
        CommandSpec::new("room kick", "rooms", "从房间踢出用户。", "room kick <room_id> <user_id>"),
        CommandSpec::new("room host", "rooms", "设置房主，? 表示系统房主。", "room host <room_id> <user_id|?>"),
        CommandSpec::new("room force-move", "rooms", "强制迁移用户到指定房间。", "room force-move <room_id> <user_id> [monitor]"),
        CommandSpec::new("room hide", "rooms", "隐藏房间，使其默认不出现在 Web API 与欢迎语。", "room hide <room_id> [true|false]"),
        CommandSpec::new("room unhide", "rooms", "取消隐藏房间。", "room unhide <room_id>"),
        CommandSpec::new("room close", "rooms", "解散房间。", "room close <room_id>"),
        CommandSpec::new("room set", "rooms", "修改房间设置。", "room set <room_id> <field> <value>"),
        CommandSpec::new("room history", "rooms", "查看房间游玩历史。", "room history <room_id>"),
        CommandSpec::new("room rounds", "rooms", "查看房间轮次列表。", "room rounds <room_id>"),
        CommandSpec::new("room round", "rooms", "查看指定轮次详情。", "room round <round_uuid>"),
        CommandSpec::new("room uuid", "rooms", "查看房间 UUID。", "room uuid <room_id>"),
        CommandSpec::new("room ban", "rooms", "加入房间黑名单。", "room ban <room_id> <user_id>"),
        CommandSpec::new("room unban", "rooms", "移出房间黑名单。", "room unban <room_id> <user_id>"),
        CommandSpec::new("room banlist", "rooms", "查看房间黑名单。", "room banlist <room_id>"),
    ] {
        register(&mut registry, spec);
    }

    for spec in [
        CommandSpec::new("plugin list", "plugins", "列出所有 WASM 插件。", "plugin list"),
        CommandSpec::new("plugin enable", "plugins", "启用插件。", "plugin enable <name>"),
        CommandSpec::new("plugin disable", "plugins", "禁用插件。", "plugin disable <name>"),
        CommandSpec::new("plugin reload", "plugins", "重载所有插件。", "plugin reload"),
        CommandSpec::new("plugin info", "plugins", "查看插件详情。", "plugin info <id_or_name>"),
        CommandSpec::new("plugin call", "plugins", "调用插件导出 API。", "plugin call <id_or_name> <method> [JSON_ARRAY]"),
        CommandSpec::new("plugins", "plugins", "列出所有 WASM 插件。", "plugins").alias("pl"),
        CommandSpec::new("plug-enable", "plugins", "旧式插件启用命令。", "plug-enable <name>").alias("pe"),
        CommandSpec::new("plug-disable", "plugins", "旧式插件禁用命令。", "plug-disable <name>").alias("pd"),
        CommandSpec::new("plug-reload", "plugins", "旧式插件重载命令。", "plug-reload").alias("pr"),
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

    for spec in [
        CommandSpec::new("ext-list", "extensions", "查看扩展字段列表。", "ext-list"),
        CommandSpec::new("ext-get", "extensions", "查看扩展数据。", "ext-get <user_id> <key>"),
        CommandSpec::new("welcome-config", "extensions", "查看或调整欢迎语配置。", "welcome-config"),
        CommandSpec::new("player-count", "extensions", "查看玩家统计扩展。", "player-count"),
        CommandSpec::new("playtime", "extensions", "查看游玩时长扩展。", "playtime"),
        CommandSpec::new("round-last", "extensions", "查看最近轮次扩展。", "round-last"),
    ] {
        register(&mut registry, spec);
    }

    registry
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registry_resolves_aliases_and_children() {
        let registry = runtime_v2_registry();
        assert_eq!(registry.get("h").map(|cmd| cmd.name.as_str()), Some("help"));
        assert!(registry.child_commands("room").contains(&"list".to_string()));
        assert!(registry.complete_line("simulation ").contains(&"status".to_string()));
        assert!(registry.complete_line("room f").contains(&"force-move".to_string()));
    }

    #[test]
    fn help_uses_structured_sections() {
        let registry = runtime_v2_registry();
        let help = registry.format_help("room list").expect("room list help");
        assert!(help.contains("NAME"));
        assert!(help.contains("USAGE"));
        assert!(help.contains("room list"));
    }
}
