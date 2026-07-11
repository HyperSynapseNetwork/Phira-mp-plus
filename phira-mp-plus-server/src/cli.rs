//! Administrative command parsing and dispatch.

use crate::plugin::PluginEvent;
use crate::server::PlusServerState;
use std::sync::Arc;
use tokio::sync::{mpsc, RwLock};
use tracing::info;

mod commands;
mod dispatch;

mod c {
    fn paint(code: &str, text: &str) -> String {
        if std::env::var_os("NO_COLOR").is_some() {
            text.to_string()
        } else {
            format!("\x1b[{code}m{text}\x1b[0m")
        }
    }

    pub fn green(text: &str) -> String {
        paint("32", text)
    }
    pub fn cyan(text: &str) -> String {
        paint("36", text)
    }
    pub fn yellow(text: &str) -> String {
        paint("33", text)
    }
    pub fn red(text: &str) -> String {
        paint("31", text)
    }
    pub fn bold(text: &str) -> String {
        paint("1", text)
    }
    pub fn dim(text: &str) -> String {
        paint("2", text)
    }
    pub fn magenta(text: &str) -> String {
        paint("35", text)
    }
}

fn parse_cli_bool(value: &str) -> bool {
    matches!(
        value.trim().to_ascii_lowercase().as_str(),
        "true"
            | "1"
            | "yes"
            | "y"
            | "on"
            | "enable"
            | "enabled"
            | "hide"
            | "hidden"
            | "锁定"
            | "隐藏"
            | "是"
    )
}

fn parse_room_host_target(value: &str) -> Result<Option<i32>, std::num::ParseIntError> {
    let trimmed = value.trim();
    if trimmed == "?"
        || trimmed == "-"
        || trimmed.eq_ignore_ascii_case("system")
        || trimmed.eq_ignore_ascii_case("none")
        || trimmed.eq_ignore_ascii_case("null")
    {
        Ok(None)
    } else {
        trimmed.parse::<i32>().map(Some)
    }
}

const CLIENT_CLI_OUTPUT_LINE_LIMIT: usize = 512;

fn strip_trailing_cli_join_marker(value: &str) -> Option<String> {
    let trimmed_len = value.trim_end().len();
    let without_trailing_ws = &value[..trimmed_len];
    without_trailing_ws
        .strip_suffix("--")
        .map(|prefix| prefix.to_string())
}

/// Merge CLI continuation fragments.
///
/// A line ending in `--` is kept pending and must be continued by the next
/// line beginning with `--`. Markers are removed before concatenation, so:
/// `room set a--` + `-- b--` + `-- c` becomes `room set a b c`.
pub fn collect_cli_continuation(
    pending: &mut Option<String>,
    line: String,
) -> Result<Option<String>, String> {
    let line = line.trim().to_string();
    if line.is_empty() {
        return Ok(None);
    }

    let mut merged = if let Some(mut prefix) = pending.take() {
        let Some(suffix) = line.trim_start().strip_prefix("--") else {
            return Err("上一条命令以 -- 结尾，下一条必须以 -- 开头；已取消续行".to_string());
        };
        prefix.push_str(suffix);
        prefix
    } else {
        line
    };

    if let Some(prefix) = strip_trailing_cli_join_marker(&merged) {
        *pending = Some(prefix);
        return Ok(None);
    }

    Ok(Some(std::mem::take(&mut merged).trim().to_string()))
}

/// Execute one CLI command and collect its output.
///
/// Used by the in-game admin `_<command>` room creation shortcut. The normal
/// interactive banner is filtered so the client receives only command output.
pub async fn execute_cli_once(state: Arc<PlusServerState>, line: String) -> Vec<String> {
    let (out_tx, mut out_rx) = mpsc::channel::<String>(256);
    let (cmd_tx, cmd_rx) = mpsc::channel::<String>(16);
    let handler = CliHandler::new(state, out_tx);
    let task = tokio::spawn(async move {
        handler.start(cmd_rx).await;
    });
    let _ = cmd_tx.try_send(line);
    drop(cmd_tx);

    let mut lines = Vec::new();
    loop {
        match tokio::time::timeout(std::time::Duration::from_millis(800), out_rx.recv()).await {
            Ok(Some(line)) => {
                let trimmed = strip_ansi_for_client(&line).trim().to_string();
                if trimmed.is_empty()
                    || trimmed.contains("管理控制台")
                    || trimmed.contains("输入 help 查看命令帮助")
                {
                    continue;
                }
                lines.push(trimmed);
                if lines.len() >= CLIENT_CLI_OUTPUT_LINE_LIMIT {
                    lines.push(format!(
                        "……输出过长，仅显示前 {CLIENT_CLI_OUTPUT_LINE_LIMIT} 行"
                    ));
                    break;
                }
            }
            Ok(None) => break,
            Err(_) => break,
        }
    }
    task.abort();
    if lines.is_empty() {
        lines.push("命令已执行，无输出".to_string());
    }
    lines
}

fn redact_cli_command_for_event(line: &str) -> String {
    let mut parts = line.split_whitespace();
    let command = parts.next().unwrap_or_default();
    match command {
        "plugin" if matches!(parts.next(), Some("call")) => "plugin call <args>".to_string(),
        _ => line.to_string(),
    }
}

fn strip_ansi_for_client(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut chars = input.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch == '\u{1b}' {
            if matches!(chars.peek(), Some('[')) {
                chars.next();
                while let Some(c) = chars.next() {
                    if ('@'..='~').contains(&c) {
                        break;
                    }
                }
            }
            continue;
        }
        if !ch.is_control() || ch == '\t' {
            out.push(ch);
        }
    }
    out
}

/// CLI 命令处理器
pub struct CliHandler {
    state: Arc<PlusServerState>,
    running: Arc<RwLock<bool>>,
    out_tx: mpsc::Sender<String>,
}

impl CliHandler {
    pub fn new(state: Arc<PlusServerState>, out_tx: mpsc::Sender<String>) -> Self {
        Self {
            state,
            running: Arc::new(RwLock::new(true)),
            out_tx,
        }
    }

    pub fn is_running(&self) -> Arc<RwLock<bool>> {
        Arc::clone(&self.running)
    }

    /// 发送一行输出到 TUI
    fn out(&self, msg: impl Into<String>) {
        // 使用 try_send 而非 send: output channel 满时直接丢弃（非关键数据）
        let _ = self.out_tx.try_send(msg.into());
    }

    /// 启动 CLI 处理器（运行在 tokio 任务中）
    pub async fn start(&self, mut cmd_rx: mpsc::Receiver<String>) {
        info!("CLI management console started");

        self.out(String::new());
        self.out(format!(
            "  {} Phira-mp+ v{} 管理控制台",
            c::bold("◆"),
            env!("CARGO_PKG_VERSION")
        ));
        self.out(format!(
            "  {} 输入 {} 查看命令帮助，{} 关闭服务器",
            c::dim("▸"),
            c::cyan("help"),
            c::red("exit")
        ));
        self.out(String::new());

        let mut pending_line: Option<String> = None;
        let mut status_interval = tokio::time::interval(std::time::Duration::from_secs(5));
        loop {
            tokio::select! {
                line = cmd_rx.recv() => {
                    let Some(line) = line else { break; };
                    // 如果未运行（exit 后），直接退出
                    if !*self.running.read().await { break; }

                    let line = match collect_cli_continuation(&mut pending_line, line) {
                        Ok(Some(line)) => line,
                        Ok(None) => {
                            if pending_line.is_some() {
                                self.out(format!("  {} 已暂存续行；下一条命令需以 -- 开头", c::dim("▸")));
                            }
                            continue;
                        }
                        Err(err) => {
                            self.out(format!("  {} {err}", c::red("✗")));
                            continue;
                        }
                    };
                    if line.is_empty() { continue; }

                    let mut parts = line.split_whitespace();
                    let command = parts.next().unwrap_or("");
                    let args: Vec<&str> = parts.collect();

                    if !command.is_empty() {
                        self.state.event_bus.publish(crate::event_bus::MpEvent::AdminCommandExecuted {
                            user_id: None,
                            command: redact_cli_command_for_event(&line),
                        });
                    }

                    if !self.dispatch_command(command, &args).await { break; }
                }
                _ = status_interval.tick() => {
                    self.broadcast_status().await;
                }
            }
        }

        info!("CLI session ended");
    }

    async fn broadcast_status(&self) {
        let rooms = self.state.rooms.read().await.len();
        let users = self
            .state
            .users
            .read()
            .await
            .values()
            .filter(|u| u.id > 0)
            .count();
        let sessions = self.state.sessions.read().await.len();
        let plugins = self.state.plugin_manager.list_plugins().await.len();
        let sim = self.state.simulation.status().await;
        let sim_status = if sim.running {
            format!("运行{}u/{}r", sim.virtual_users, sim.virtual_rooms)
        } else {
            "停止".into()
        };
        self.out(format!(
            "📊 rooms={rooms} users={users} sessions={sessions} plugins={plugins} sim={sim_status}"
        ));
    }

    async fn print_help(&self, args: &[&str]) {
        if !args.is_empty() {
            match args {
                ["all"] => {
                    for line in self.state.command_registry.format_overview_all().lines() {
                        self.out(format!("  {line}"));
                    }
                    return;
                }
                ["groups"] | ["group"] => {
                    for line in self.state.command_registry.format_groups().lines() {
                        self.out(format!("  {line}"));
                    }
                    return;
                }
                ["group", group] => {
                    for line in self
                        .state
                        .command_registry
                        .format_group(group, false)
                        .lines()
                    {
                        self.out(format!("  {line}"));
                    }
                    return;
                }
                ["group", group, "all"] | ["group", group, "full"] => {
                    for line in self
                        .state
                        .command_registry
                        .format_group(group, true)
                        .lines()
                    {
                        self.out(format!("  {line}"));
                    }
                    return;
                }
                ["advanced"] => {
                    for line in self.state.command_registry.format_advanced().lines() {
                        self.out(format!("  {line}"));
                    }
                    return;
                }
                ["dev"] => {
                    for line in self.state.command_registry.format_dev().lines() {
                        self.out(format!("  {line}"));
                    }
                    return;
                }
                _ => {}
            }

            let query = args.join(" ");
            if let Some(help) = self.state.command_registry.format_help(&query) {
                for line in help.lines() {
                    self.out(format!("  {line}"));
                }
                return;
            }
            self.out(format!("  {} 未找到命令帮助: {}", c::yellow("!"), query));
            self.out(format!(
                "  {} {}",
                c::dim("▸"),
                self.state.command_registry.format_unknown(&query)
            ));
            return;
        }

        for line in self.state.command_registry.format_overview().lines() {
            if line.starts_with('▸') {
                self.out(format!("  {}", c::cyan(line)));
            } else if line.starts_with('─') || line.starts_with("提示") {
                self.out(format!("  {}", c::dim(line)));
            } else {
                self.out(format!("  {line}"));
            }
        }

        let plugin_cmds = self.state.plugin_manager.list_cli_commands().await;
        if !plugin_cmds.is_empty() {
            self.out(String::new());
            self.out(format!("  {} 插件扩展命令", c::cyan("▸")));
            for cmd in &plugin_cmds {
                self.out(format!("    {:<22} {}", c::dim(&cmd.name), cmd.description));
            }
        }
        self.out(String::new());
        self.out(format!(
            "  {} help <命令> 查看详情；help all / help groups / help group <分组> 可展开",
            c::dim("▸")
        ));
    }

    async fn list_users(&self) {
        let users = self.state.users.read().await;
        let player_count = users.values().filter(|user| user.id > 0).count();
        if player_count == 0 {
            self.out(format!("  {} 当前无在线用户", c::dim("·")));
        } else {
            self.out(format!("  {} 在线用户 ({})", c::green("◆"), player_count));
            self.out(format!(
                "  {}",
                c::dim("  ────────────────────────────────────────────")
            ));
            for user in users.values().filter(|user| user.id > 0) {
                let monitor = if user.monitor.load(std::sync::atomic::Ordering::Relaxed) {
                    c::yellow(" [观]")
                } else {
                    String::new()
                };
                let in_room = {
                    let room_guard = user.room.read().await;
                    room_guard
                        .as_ref()
                        .map(|r| format!(" {} 房间 {}", c::dim("·"), c::cyan(&r.id.to_string())))
                        .unwrap_or_default()
                };
                self.out(format!(
                    "  {} {:<6} {}{}{}",
                    c::dim("│"),
                    user.id,
                    c::bold(&user.name),
                    monitor,
                    in_room
                ));
            }
        }
        drop(users);

        // 显示房间概要
        let rooms = self.state.rooms.read().await;
        if !rooms.is_empty() {
            self.out(String::new());
            self.out(format!("  {} 活跃房间 ({})", c::green("◆"), rooms.len()));
            for room in rooms.values() {
                let users_count = room.users().await.len();
                let monitors_count = room.monitors().await.len();
                let state_str = match &*room.state.read().await {
                    crate::room::InternalRoomState::SelectChart => c::cyan("选曲中"),
                    crate::room::InternalRoomState::WaitForReady { .. } => c::yellow("等待准备"),
                    crate::room::InternalRoomState::Playing { .. } => c::magenta("游戏中"),
                };
                self.out(format!(
                    "  {} {:<15}  {}  {}+{} 人",
                    c::dim("│"),
                    room.id.to_string(),
                    state_str,
                    users_count,
                    monitors_count
                ));
            }
        }
    }

    async fn list_rooms(&self) {
        let rooms: Vec<Arc<crate::room::Room>> = {
            let guard = self.state.rooms.read().await;
            guard.values().map(Arc::clone).collect()
        };
        if rooms.is_empty() {
            self.out(format!("  {} 当前无活跃房间", c::dim("·")));
            return;
        }

        self.out(format!("  {} 活跃房间 ({})", c::green("◆"), rooms.len()));
        self.out(format!(
            "  {}",
            c::dim("  ────────────────────────────────────────────")
        ));
        for room in &rooms {
            self.state.refresh_room_display_metadata(room).await;
            let users_in_room = room.users().await;
            let monitors_in_room = room.monitors().await;

            let state_str = match &*room.state.read().await {
                crate::room::InternalRoomState::SelectChart => "SelectChart",
                crate::room::InternalRoomState::WaitForReady { .. } => "WaitForReady",
                crate::room::InternalRoomState::Playing { .. } => "Playing",
            };
            let locked = if room.is_locked() {
                c::yellow("锁定")
            } else {
                c::dim("未锁定")
            };
            let cycling = if room.is_cycle() {
                c::cyan("轮换")
            } else {
                c::dim("不轮换")
            };
            let hidden = if room.is_hidden() {
                c::magenta("隐藏")
            } else {
                c::dim("公开")
            };

            self.out(format!(
                "  {} {}",
                c::dim("┏"),
                c::bold(&room.id.to_string())
            ));
            self.out(format!(
                "  {} 状态: {}  {}  {}  {}  {}",
                c::dim("┃"),
                state_str,
                locked,
                cycling,
                hidden,
                if users_in_room.len() + monitors_in_room.len() > 0 {
                    c::cyan(&format!(
                        "{} 人在线",
                        users_in_room.len() + monitors_in_room.len()
                    ))
                } else {
                    c::dim("空闲")
                }
            ));
            if !users_in_room.is_empty() {
                let mut labels = Vec::new();
                for u in &users_in_room {
                    labels.push(format!(
                        "{}({})",
                        c::bold(&room.display_name(u).await),
                        u.id
                    ));
                }
                self.out(format!("  {} 玩家: {}", c::dim("┃"), labels.join(", ")));
            }
            if !monitors_in_room.is_empty() {
                let mut labels = Vec::new();
                for u in &monitors_in_room {
                    labels.push(format!(
                        "{}({})",
                        c::bold(&room.display_name(u).await),
                        u.id
                    ));
                }
                self.out(format!("  {} 旁观: {}", c::dim("┃"), labels.join(", ")));
            }
            self.out(format!("  {}", c::dim("  ─ ─ ─ ─ ─ ─")));
        }

        // 在线用户概要
        let users = self.state.users.read().await;
        if !users.is_empty() {
            self.out(String::new());
            self.out(format!("  {} 在线用户 ({})", c::green("◆"), users.len()));
            for user in users.values() {
                let in_room = if user.room.read().await.is_some() {
                    c::cyan(" 在房间中")
                } else {
                    c::dim(" 大厅")
                };
                self.out(format!(
                    "  {} {:<6}  {}{}",
                    c::dim("│"),
                    user.id,
                    c::bold(&user.name),
                    in_room
                ));
            }
        }
    }

    async fn admin_ids(&self, args: &[&str]) {
        let sub = args.first().copied().unwrap_or("list");
        match sub {
            "list" | "" => {
                let mut ids: Vec<i32> = self.state.admin_ids.read().await.iter().copied().collect();
                ids.sort_unstable();
                if ids.is_empty() {
                    self.out(format!("  {} 当前没有配置管理员 Phira ID", c::yellow("!")));
                } else {
                    self.out(format!(
                        "  {} 管理员 Phira ID: {}",
                        c::green("◆"),
                        ids.iter()
                            .map(ToString::to_string)
                            .collect::<Vec<_>>()
                            .join(", ")
                    ));
                }
            }
            "add" => {
                if args.len() < 2 {
                    self.out(format!("  {} admin-id add <PhiraID>", c::yellow("?")));
                    return;
                }
                let Ok(id) = args[1].parse::<i32>() else {
                    self.out(format!("  {} 无效 Phira ID", c::red("✗")));
                    return;
                };
                self.state.add_admin_id(id).await;
                self.out(format!("  {} 已添加管理员 {}", c::green("✓"), id));
            }
            "remove" => {
                if args.len() < 2 {
                    self.out(format!("  {} admin-id remove <PhiraID>", c::yellow("?")));
                    return;
                }
                let Ok(id) = args[1].parse::<i32>() else {
                    self.out(format!("  {} 无效 Phira ID", c::red("✗")));
                    return;
                };
                self.state.remove_admin_id(id).await;
                self.out(format!("  {} 已移除管理员 {}", c::green("✓"), id));
            }
            "set" => {
                let mut ids = Vec::new();
                for arg in &args[1..] {
                    match arg.parse::<i32>() {
                        Ok(id) => ids.push(id),
                        Err(_) => {
                            self.out(format!("  {} 无效 Phira ID: {}", c::red("✗"), arg));
                            return;
                        }
                    }
                }
                self.state.set_admin_ids(ids.clone()).await;
                self.out(format!(
                    "  {} 已设置管理员列表: {}",
                    c::green("✓"),
                    ids.iter()
                        .map(ToString::to_string)
                        .collect::<Vec<_>>()
                        .join(", ")
                ));
            }
            _ => {
                self.out(format!("  {} admin-id list|add|remove|set", c::yellow("?")));
            }
        }
    }

    async fn status(&self) {
        let users = self
            .state
            .users
            .read()
            .await
            .values()
            .filter(|user| user.id > 0)
            .count();
        let rooms = self.state.rooms.read().await.len();
        let sessions = self.state.sessions.read().await.len();
        let plugins = self.state.plugin_manager.list_plugins().await.len();
        self.out(format!(
            "  {} Phira-mp+ v{}  │ 端口 {}  │ 用户 {} 会话 {} 房间 {} 插件 {}",
            c::bold("◆"),
            env!("CARGO_PKG_VERSION"),
            self.state.config.port,
            users,
            sessions,
            rooms,
            plugins
        ));
    }

    async fn ban_user(&self, target: &str, reason: &str) {
        let uid: i32 = match target.parse() {
            Ok(id) => id,
            Err(_) => {
                self.out(format!("  {} 无效的用户ID: {}", c::red("✗"), target));
                return;
            }
        };
        match self.state.ban_manager.ban_user(uid, reason).await {
            Ok(reason) => {
                let disconnected = self.state.disconnect_banned_user(uid, &reason).await;
                self.out(format!(
                    "  {} 用户 {} 已封禁{}\n    理由：{}",
                    c::green("✓"),
                    uid,
                    if disconnected {
                        "，在线会话已通知并断开"
                    } else {
                        ""
                    },
                    c::yellow(&reason),
                ));
            }
            Err(e) => self.out(format!("  {} {}", c::red("✗"), e)),
        }
    }

    async fn unban_user(&self, target: &str) {
        let uid: i32 = match target.parse() {
            Ok(id) => id,
            Err(_) => {
                self.out(format!("  {} 无效的用户ID: {}", c::red("✗"), target));
                return;
            }
        };
        match self.state.ban_manager.unban_user(uid).await {
            Ok(_) => self.out(format!("  {} 用户 {} 已解封", c::green("✓"), uid)),
            Err(e) => self.out(format!("  {} {}", c::red("✗"), e)),
        }
    }

    async fn ban_list(&self) {
        let list = self.state.ban_manager.list_banned().await;
        if list.is_empty() {
            self.out(format!("  {} 黑名单为空", c::dim("·")));
            return;
        }
        self.out(format!("  {} 封禁用户 ({})", c::green("◆"), list.len()));
        self.out(format!(
            "  {}",
            c::dim("  ────────────────────────────────────────────")
        ));
        for entry in &list {
            self.out(format!(
                "  {} {:<6}  {}",
                c::dim("│"),
                entry.user_id,
                c::dim(&entry.reason),
            ));
        }
    }

    async fn list_extensions(&self) {
        let user_fields = self.state.extensions.list_user_fields().await;
        let room_fields = self.state.extensions.list_room_fields().await;

        self.out(format!("  {} 用户扩展字段:", c::cyan("◆")));
        if user_fields.is_empty() {
            self.out(format!("    {} 无", c::dim("-")));
        } else {
            for f in &user_fields {
                self.out(format!("    {} {}", c::dim("·"), f));
            }
        }

        self.out(format!("  {} 房间扩展字段:", c::cyan("◆")));
        if room_fields.is_empty() {
            self.out(format!("    {} 无", c::dim("-")));
        } else {
            for f in &room_fields {
                self.out(format!("    {} {}", c::dim("·"), f));
            }
        }
    }

    async fn get_extension(&self, id: &str, key: &str) {
        if let Ok(uid) = id.parse::<i32>() {
            if let Some(val) = self.state.extensions.get_user_extra(uid, key).await {
                self.out(format!(
                    "  {} 用户 {} 的 {} = {}",
                    c::green("◆"),
                    uid,
                    c::cyan(key),
                    val
                ));
                return;
            }
        }
        if let Some(val) = self.state.extensions.get_room_extra(id, key).await {
            self.out(format!(
                "  {} 房间 {} 的 {} = {}",
                c::green("◆"),
                id,
                c::cyan(key),
                val
            ));
            return;
        }
        self.out(format!(
            "  {} 未找到扩展数据: id={}, key={}",
            c::yellow("!"),
            id,
            key
        ));
    }
}
