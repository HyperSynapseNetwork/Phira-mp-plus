//! Command metadata registry for Runtime v2.
//!
//! This module intentionally stores command metadata only.  The existing CLI
//! dispatcher remains the source of truth for command execution until commands
//! are migrated one by one.  Keeping metadata separate lets TUI completion,
//! in-game admin commands, WIT APIs and future Web admin APIs converge on the
//! same help/usage model without a risky rewrite of `cli.rs`.

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
}

impl CommandRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn register(&mut self, spec: CommandSpec) -> Result<(), String> {
        if spec.name.trim().is_empty() {
            return Err("command name cannot be empty".to_string());
        }
        if self.commands.contains_key(&spec.name) || self.aliases.contains_key(&spec.name) {
            return Err(format!("duplicated command name: {}", spec.name));
        }
        for alias in &spec.aliases {
            if alias.trim().is_empty() {
                return Err(format!("empty alias for command {}", spec.name));
            }
            if self.commands.contains_key(alias) || self.aliases.contains_key(alias) {
                return Err(format!("duplicated command alias: {alias}"));
            }
        }

        for alias in &spec.aliases {
            self.aliases.insert(alias.clone(), spec.name.clone());
        }
        self.commands.insert(spec.name.clone(), spec);
        Ok(())
    }

    pub fn get(&self, name_or_alias: &str) -> Option<&CommandSpec> {
        let canonical = self
            .aliases
            .get(name_or_alias)
            .map(String::as_str)
            .unwrap_or(name_or_alias);
        self.commands.get(canonical)
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

    pub fn complete(&self, prefix: &str) -> Vec<String> {
        self.commands
            .keys()
            .chain(self.aliases.keys())
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
}

pub fn runtime_v2_registry() -> CommandRegistry {
    let mut registry = CommandRegistry::new();

    let specs = [
        CommandSpec::new("help", "core", "显示命令帮助。", "help [command]")
            .alias("h")
            .alias("?")
            .arg(CommandArgSpec::optional("command", "要查看详情的命令名或别名"))
            .example("help")
            .example("help room list"),
        CommandSpec::new("status", "core", "查看服务器运行状态。", "status")
            .alias("st")
            .example("status"),
        CommandSpec::new(
            "benchmark",
            "diagnostics",
            "运行真实网络压测。该命令需要显式配置 Phira token，不是 Runtime v2 默认压测入口。",
            "benchmark [seconds] [rooms]",
        )
        .alias("bench")
        .arg(CommandArgSpec::optional("seconds", "压测时长，默认 30，范围 5..300"))
        .arg(CommandArgSpec::optional("rooms", "目标房间数，默认 100，最大 5000"))
        .example("benchmark 30 100"),
        CommandSpec::new(
            "simulation status",
            "simulation",
            "查看 Runtime v2 Simulation 状态。",
            "simulation status",
        )
        .example("simulation status"),
        CommandSpec::new(
            "simulation seed",
            "simulation",
            "设置 deterministic simulation seed。当前 Step 1 只保存种子，不启动压测。",
            "simulation seed <value>",
        )
        .arg(CommandArgSpec::required("value", "u64 种子值"))
        .example("simulation seed 114514"),
        CommandSpec::new(
            "simulation cleanup",
            "simulation",
            "清理 Runtime v2 Simulation 内存状态。当前 Step 1 不会触碰真实房间。",
            "simulation cleanup",
        )
        .example("simulation cleanup"),
    ];

    for spec in specs {
        // Static registry construction should never fail.  If it does, keeping a
        // partial registry is safer than crashing server startup for metadata.
        let _ = registry.register(spec);
    }

    registry
}
