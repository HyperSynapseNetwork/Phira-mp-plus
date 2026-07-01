//! CLI command dispatch implementation.
//!
//! This module keeps the top-level command routing out of `cli.rs` so the
//! console lifecycle, continuation handling, and command execution do not all
//! grow inside one file. Runtime v2 intentionally does not keep compatibility
//! aliases here: only canonical namespaces are dispatched.

use super::*;

impl CliHandler {
    pub(super) async fn dispatch_command(&self, command: &str, args: &[&str]) -> bool {
        match command {
    "exit" => {
        self.out(format!("  {} 正在关闭服务器...", c::yellow("⟳")));
        *self.running.write().await = false;
        self.state.shutdown.notify_one();
        self.out(format!("  {} 已发送关闭信号", c::green("✓")));
        return false;
    }
    "help" => self.print_help(args).await,
    "plugin" => {
        let sub = args.first().copied().unwrap_or("");
        match sub {
            "list" | "" => self.list_plugins().await,
            "enable" => {
                if args.len() < 2 {
                    self.out(format!("  {} {} plugin enable <插件名>", c::yellow("?"), c::bold("用法")));
                } else {
                    self.enable_plugin(args[1]).await;
                }
            }
            "disable" => {
                if args.len() < 2 {
                    self.out(format!("  {} {} plugin disable <插件名>", c::yellow("?"), c::bold("用法")));
                } else {
                    self.disable_plugin(args[1]).await;
                }
            }
            "reload" => self.reload_plugins().await,
            "info" => {
                if args.len() < 2 {
                    self.out(format!("  {} {} plugin info <插件ID或名称>", c::yellow("?"), c::bold("用法")));
                } else {
                    self.plugin_info(args[1]).await;
                }
            }
            "call" => {
                if args.len() < 3 {
                    self.out(format!("  {} {} plugin call <插件ID或名称> <方法> [JSON数组]", c::yellow("?"), c::bold("用法")));
                } else {
                    self.plugin_call(args[1], args[2], &args[3..].join(" ")).await;
                }
            }
            _ => {
                self.out(format!("  {} 未知子命令: {}  ", c::red("✗"), c::yellow(sub)));
                self.out(format!("  {} 可用: plugin list | enable | disable | reload | info | call", c::dim("▸")));
            }
        }
    }
    "users" => self.list_users().await,
    "rooms" => self.list_rooms().await,
    "room" => {
        let sub = args.first().copied().unwrap_or("");
        match sub {
            "list" | "" => self.list_rooms().await,
            "create-empty" => {
                if args.len() < 2 { self.out(format!("  {} {} room create-empty <房间ID> [phira_api_endpoint]", c::yellow("?"), c::bold("用法"))); }
                else { self.room_create_empty(args[1], args.get(2).copied()).await; }
            }
            "info" => {
                if args.len() < 2 { self.out(format!("  {} {} room info <房间ID>", c::yellow("?"), c::bold("用法"))); }
                else { self.room_info(args[1]).await; }
            }
            "start" => {
                if args.len() < 2 { self.out(format!("  {} {} room start <房间ID>", c::yellow("?"), c::bold("用法"))); }
                else { self.room_start(args[1]).await; }
            }
            "cancel" => {
                if args.len() < 2 { self.out(format!("  {} {} room cancel <房间ID>", c::yellow("?"), c::bold("用法"))); }
                else { self.room_cancel(args[1]).await; }
            }
            "kick" => {
                if args.len() < 3 { self.out(format!("  {} {} room kick <房间ID> <用户ID>", c::yellow("?"), c::bold("用法"))); }
                else { self.kick_from_room(args[1], args[2]).await; }
            }
            "host" => {
                if args.len() < 3 { self.out(format!("  {} {} room host <房间ID> <用户ID|?>", c::yellow("?"), c::bold("用法"))); }
                else {
                    let target = match parse_room_host_target(args[2]) {
                        Ok(target) => target,
                        Err(_) => { self.out(format!("  {} 无效的房主目标：请使用用户ID或 ?", c::red("✗"))); return true; }
                    };
                    self.room_set_host(args[1], target).await;
                }
            }
            "force-move" => {
                if args.len() < 3 { self.out(format!("  {} {} room force-move <房间ID> <用户ID> [monitor]", c::yellow("?"), c::bold("用法"))); }
                else { let uid: i32 = match args[2].parse() { Ok(id) => id, Err(_) => { self.out(format!("  {} 无效的用户ID", c::red("✗"))); return true; } };
                    let monitor = args.get(3).map(|v| parse_cli_bool(v)).unwrap_or(false);
                    self.room_force_move(args[1], uid, monitor).await; }
            }
            "hide" => {
                if args.len() < 2 { self.out(format!("  {} {} room hide <房间ID> [true|false]", c::yellow("?"), c::bold("用法"))); }
                else { self.room_hide(args[1], args.get(2).map(|v| parse_cli_bool(v)).unwrap_or(true)).await; }
            }
            "unhide" => {
                if args.len() < 2 { self.out(format!("  {} {} room unhide <房间ID>", c::yellow("?"), c::bold("用法"))); }
                else { self.room_hide(args[1], false).await; }
            }
            "close" => {
                if args.len() < 2 { self.out(format!("  {} {} room close <房间ID>", c::yellow("?"), c::bold("用法"))); }
                else { self.close_room(args[1]).await; }
            }
            "set" => {
                if args.len() < 4 { self.out(format!("  {} {} room set <房间ID> <字段> <值>", c::yellow("?"), c::bold("用法"))); }
                else { self.room_set(args[1], args[2], &args[3..].join(" ")).await; }
            }
            "history" => {
                if args.len() < 2 { self.out(format!("  {} {} room history <房间ID>", c::yellow("?"), c::bold("用法"))); }
                else { self.room_history(args[1]).await; }
            }
            "rounds" => {
                if args.len() < 2 { self.out(format!("  {} {} room rounds <房间ID>", c::yellow("?"), c::bold("用法"))); }
                else { self.room_rounds(args[1]).await; }
            }
            "round" => {
                if args.len() < 2 { self.out(format!("  {} {} room round <轮次UUID>", c::yellow("?"), c::bold("用法"))); }
                else { self.room_round_info(args[1]).await; }
            }
            "ban" => {
                if args.len() < 3 { self.out(format!("  {} {} room ban <房间ID> <用户ID>", c::yellow("?"), c::bold("用法"))); }
                else { let uid: i32 = match args[2].parse() { Ok(id) => id, Err(_) => { self.out(format!("  {} 无效的用户ID", c::red("✗"))); return true; } };
                    match self.state.ban_manager.room_ban_user(args[1], uid).await {
                        Ok(_) => self.out(format!("  {} 用户 {} 已加入房间 {} 的黑名单", c::green("✓"), uid, args[1])),
                        Err(e) => self.out(format!("  {} {}", c::red("✗"), e)),
                    }}
            }
            "unban" => {
                if args.len() < 3 { self.out(format!("  {} {} room unban <房间ID> <用户ID>", c::yellow("?"), c::bold("用法"))); }
                else { let uid: i32 = match args[2].parse() { Ok(id) => id, Err(_) => { self.out(format!("  {} 无效的用户ID", c::red("✗"))); return true; } };
                    match self.state.ban_manager.room_unban_user(args[1], uid).await {
                        Ok(_) => self.out(format!("  {} 用户 {} 已移出房间 {} 的黑名单", c::green("✓"), uid, args[1])),
                        Err(e) => self.out(format!("  {} {}", c::red("✗"), e)),
                    }}
            }
            "banlist" => {
                if args.len() < 2 { self.out(format!("  {} {} room banlist <房间ID>", c::yellow("?"), c::bold("用法"))); }
                else { self.room_ban_list(args[1]).await; }
            }
            "uuid" => {
                if args.len() < 2 { self.out(format!("  {} {} room uuid <房间ID>", c::yellow("?"), c::bold("用法"))); }
                else { self.room_show_uuid(args[1]).await; }
            }
            _ => {
                self.out(format!("  {} 未知子命令: {}  ", c::red("✗"), c::yellow(sub)));
                self.out(format!("  {} 可用: room list|create-empty|info|start|cancel|kick|transfer|force-move|hide|unhide|close|set|history|rounds|round|uuid|ban|unban|banlist", c::dim("▸")));
            }
        }
    }
    "kick" => {
        if args.len() == 1 {
            self.kick_user(args[0]).await;
        } else {
            self.out(format!("  {} {} <用户ID>", c::yellow("?"), c::bold("kick")));
            self.out(format!("  {} 房间踢人请使用: room kick <房间ID> <用户ID>", c::dim("▸")));
        }
    }
    "broadcast" => {
        if args.is_empty() {
            self.out(format!("  {} {} all <消息>          广播给所有用户", c::yellow("?"), c::bold("broadcast")));
            self.out(format!("  {} {} room <房间ID> <消息>  广播给指定房间", c::dim("▸"), c::bold("broadcast")));
            self.out(format!("  {} {} user <用户ID> <消息>  发送给指定用户", c::dim("▸"), c::bold("broadcast")));
        } else {
            let scope = args[0];
            let rest: Vec<&str> = args[1..].iter().copied().collect();
            match scope {
                "all" => {
                    self.broadcast_all(&rest.join(" ")).await;
                }
                "room" => {
                    if rest.len() < 2 {
                        self.out(format!("  {} {} room <房间ID> <消息>", c::yellow("?"), c::bold("broadcast")));
                    } else {
                        self.broadcast_room(rest[0], &rest[1..].join(" ")).await;
                    }
                }
                "user" => {
                    if rest.len() < 2 {
                        self.out(format!("  {} {} user <用户ID> <消息>", c::yellow("?"), c::bold("broadcast")));
                    } else {
                        let uid: i32 = match rest[0].parse() { Ok(id) => id, Err(_) => { self.out(format!("  {} 无效的用户ID", c::red("✗"))); return true; } };
                        self.broadcast_user(uid, &rest[1..].join(" ")).await;
                    }
                }
                _ => {
                    self.out(format!("  {} 未知 broadcast 范围: {}", c::red("✗"), c::yellow(scope)));
                    self.out(format!("  {} 可用: broadcast all|room|user", c::dim("▸")));
                }
            }
        }
    }
    "status" => self.status().await,
    "benchmark" => self.start_benchmark(args).await,
    "simulation" => self.simulation_command(args).await,
    "runtime" => self.runtime_command(args).await,
    "benchmark-bind" => self.bind_benchmark(args).await,
    "benchmark-cleanup" => {
        self.state.cleanup_benchmark_sync();
        self.out(format!("  {} 已清理 bench-* 压测房间", c::green("✓")));
    }
    "admin-id" => self.admin_ids(args).await,
    "ext-list" => self.list_extensions().await,
    "ext-get" => {
        if args.len() < 2 {
            self.out(format!("  {} {} <用户ID|房间ID> <key>", c::yellow("?"), c::bold("ext-get")));
        } else {
            self.get_extension(args[0], args[1]).await;
        }
    }
    "ban" => {
        if args.len() < 1 {
            self.out(format!("  {} {} <用户ID> [原因]", c::yellow("?"), c::bold("ban")));
        } else {
            let reason = if args.len() >= 2 { args[1..].join(" ") } else { "违规行为".to_string() };
            self.ban_user(args[0], &reason).await;
        }
    }
    "unban" => {
        if args.is_empty() {
            self.out(format!("  {} {} <用户ID>", c::yellow("?"), c::bold("unban")));
        } else {
            self.unban_user(args[0]).await;
        }
    }
    "banlist" => self.ban_list().await,
    _ => {
        // 尝试插件命令
        if !self.try_plugin_command(command, args).await {
            self.out(format!("  {} {}", c::red("✗"), self.state.command_registry.format_unknown(command)));
        }
    }
        }
        true
    }
}
