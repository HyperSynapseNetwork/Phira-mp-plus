use super::super::*;

impl CliHandler {
    pub(in crate::cli) async fn dispatch_room_command(&self, args: &[&str]) {
        let sub = args.first().copied().unwrap_or("");
        match sub {
            "list" | "" => self.list_rooms().await,
            "create-empty" => {
                if args.len() < 2 {
                    self.out(format!("  {} {} room create-empty <房间ID> [phira_api_endpoint]", c::yellow("?"), c::bold("用法")));
                } else {
                    self.room_create_empty(args[1], args.get(2).copied()).await;
                }
            }
            "info" => {
                if args.len() < 2 {
                    self.out(format!("  {} {} room info <房间ID>", c::yellow("?"), c::bold("用法")));
                } else {
                    self.room_info(args[1]).await;
                }
            }
            "start" => {
                if args.len() < 2 {
                    self.out(format!("  {} {} room start <房间ID>", c::yellow("?"), c::bold("用法")));
                } else {
                    self.room_start(args[1]).await;
                }
            }
            "cancel" => {
                if args.len() < 2 {
                    self.out(format!("  {} {} room cancel <房间ID>", c::yellow("?"), c::bold("用法")));
                } else {
                    self.room_cancel(args[1]).await;
                }
            }
            "kick" => {
                if args.len() < 3 {
                    self.out(format!("  {} {} room kick <房间ID> <用户ID>", c::yellow("?"), c::bold("用法")));
                } else {
                    self.kick_from_room(args[1], args[2]).await;
                }
            }
            "host" => {
                if args.len() < 3 {
                    self.out(format!("  {} {} room host <房间ID> <用户ID|?>", c::yellow("?"), c::bold("用法")));
                } else {
                    let target = match parse_room_host_target(args[2]) {
                        Ok(target) => target,
                        Err(_) => {
                            self.out(format!("  {} 无效的房主目标：请使用用户ID或 ?", c::red("✗")));
                            return;
                        }
                    };
                    self.room_set_host(args[1], target).await;
                }
            }
            "force-move" => {
                if args.len() < 3 {
                    self.out(format!("  {} {} room force-move <房间ID> <用户ID> [monitor]", c::yellow("?"), c::bold("用法")));
                } else {
                    let uid: i32 = match args[2].parse() {
                        Ok(id) => id,
                        Err(_) => {
                            self.out(format!("  {} 无效的用户ID", c::red("✗")));
                            return;
                        }
                    };
                    let monitor = args.get(3).map(|v| parse_cli_bool(v)).unwrap_or(false);
                    self.room_force_move(args[1], uid, monitor).await;
                }
            }
            "hide" => {
                if args.len() < 2 {
                    self.out(format!("  {} {} room hide <房间ID> [true|false]", c::yellow("?"), c::bold("用法")));
                } else {
                    self.room_hide(args[1], args.get(2).map(|v| parse_cli_bool(v)).unwrap_or(true)).await;
                }
            }
            "unhide" => {
                if args.len() < 2 {
                    self.out(format!("  {} {} room unhide <房间ID>", c::yellow("?"), c::bold("用法")));
                } else {
                    self.room_hide(args[1], false).await;
                }
            }
            "close" => {
                if args.len() < 2 {
                    self.out(format!("  {} {} room close <房间ID>", c::yellow("?"), c::bold("用法")));
                } else {
                    self.close_room(args[1]).await;
                }
            }
            "set" => {
                if args.len() < 4 {
                    self.out(format!("  {} {} room set <房间ID> <字段> <值>", c::yellow("?"), c::bold("用法")));
                } else {
                    self.room_set(args[1], args[2], &args[3..].join(" ")).await;
                }
            }
            "history" => {
                if args.len() < 2 {
                    self.out(format!("  {} {} room history <房间ID>", c::yellow("?"), c::bold("用法")));
                } else {
                    self.room_history(args[1]).await;
                }
            }
            "rounds" => {
                if args.len() < 2 {
                    self.out(format!("  {} {} room rounds <房间ID>", c::yellow("?"), c::bold("用法")));
                } else {
                    self.room_rounds(args[1]).await;
                }
            }
            "round" => {
                if args.len() < 2 {
                    self.out(format!("  {} {} room round <轮次UUID>", c::yellow("?"), c::bold("用法")));
                } else {
                    self.room_round_info(args[1]).await;
                }
            }
            "ban" => {
                if args.len() < 3 {
                    self.out(format!("  {} {} room ban <房间ID> <用户ID>", c::yellow("?"), c::bold("用法")));
                } else {
                    let uid: i32 = match args[2].parse() {
                        Ok(id) => id,
                        Err(_) => {
                            self.out(format!("  {} 无效的用户ID", c::red("✗")));
                            return;
                        }
                    };
                    match self.state.ban_manager.room_ban_user(args[1], uid).await {
                        Ok(_) => self.out(format!("  {} 用户 {} 已加入房间 {} 的黑名单", c::green("✓"), uid, args[1])),
                        Err(e) => self.out(format!("  {} {}", c::red("✗"), e)),
                    }
                }
            }
            "unban" => {
                if args.len() < 3 {
                    self.out(format!("  {} {} room unban <房间ID> <用户ID>", c::yellow("?"), c::bold("用法")));
                } else {
                    let uid: i32 = match args[2].parse() {
                        Ok(id) => id,
                        Err(_) => {
                            self.out(format!("  {} 无效的用户ID", c::red("✗")));
                            return;
                        }
                    };
                    match self.state.ban_manager.room_unban_user(args[1], uid).await {
                        Ok(_) => self.out(format!("  {} 用户 {} 已移出房间 {} 的黑名单", c::green("✓"), uid, args[1])),
                        Err(e) => self.out(format!("  {} {}", c::red("✗"), e)),
                    }
                }
            }
            "banlist" => {
                if args.len() < 2 {
                    self.out(format!("  {} {} room banlist <房间ID>", c::yellow("?"), c::bold("用法")));
                } else {
                    self.room_ban_list(args[1]).await;
                }
            }
            "uuid" => {
                if args.len() < 2 {
                    self.out(format!("  {} {} room uuid <房间ID>", c::yellow("?"), c::bold("用法")));
                } else {
                    self.room_show_uuid(args[1]).await;
                }
            }
            _ => {
                self.out(format!("  {} 未知子命令: {}  ", c::red("✗"), c::yellow(sub)));
                self.out(format!("  {} 可用: room list|create-empty|info|start|cancel|kick|host|force-move|hide|unhide|close|set|history|rounds|round|uuid|ban|unban|banlist", c::dim("▸")));
            }
        }
    }
}
