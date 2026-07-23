use super::super::*;

impl CliHandler {
    pub(in crate::cli) async fn dispatch_room_command(&self, args: &[&str]) {
        let sub = args.first().copied().unwrap_or("");
        match sub {
            "list" | "" => self.list_rooms().await,
            "create-empty" => {
                if args.len() < 2 {
                    self.out(format!(
                        "  {} {} room create-empty <房间ID> [phira_api_endpoint]",
                        c::yellow("?"),
                        c::bold("用法")
                    ));
                } else {
                    self.room_create_empty(args[1], args.get(2).copied()).await;
                }
            }
            "info" => {
                if args.len() < 2 {
                    self.out(format!(
                        "  {} {} room info <房间ID>",
                        c::yellow("?"),
                        c::bold("用法")
                    ));
                } else {
                    self.room_info(args[1]).await;
                }
            }
            "start" | "force-start" => {
                if args.len() < 2 {
                    self.out(format!(
                        "  {} {} room start <房间ID>",
                        c::yellow("?"),
                        c::bold("用法")
                    ));
                } else {
                    self.room_start(args[1]).await;
                }
            }
            "cancel" => {
                if args.len() < 2 {
                    self.out(format!(
                        "  {} {} room cancel <房间ID>",
                        c::yellow("?"),
                        c::bold("用法")
                    ));
                } else {
                    self.room_cancel(args[1]).await;
                }
            }
            "kick" => {
                if args.len() < 3 {
                    self.out(format!(
                        "  {} {} room kick <房间ID> <用户ID>",
                        c::yellow("?"),
                        c::bold("用法")
                    ));
                } else {
                    self.kick_from_room(args[1], args[2]).await;
                }
            }
            "host" => {
                if args.len() < 3 {
                    self.out(format!(
                        "  {} {} room host <房间ID> <用户ID|?>",
                        c::yellow("?"),
                        c::bold("用法")
                    ));
                } else {
                    let target = match parse_room_host_target(args[2]) {
                        Ok(target) => target,
                        Err(_) => {
                            self.out(format!(
                                "  {} 无效的房主目标：请使用用户ID或 ?",
                                c::red("✗")
                            ));
                            return;
                        }
                    };
                    self.room_set_host(args[1], target).await;
                }
            }
            "force-move" => {
                if args.len() < 3 {
                    self.out(format!(
                        "  {} {} room force-move <房间ID> <用户ID> [monitor]",
                        c::yellow("?"),
                        c::bold("用法")
                    ));
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
                    self.out(format!(
                        "  {} {} room hide <房间ID> [true|false]",
                        c::yellow("?"),
                        c::bold("用法")
                    ));
                } else {
                    self.room_hide(
                        args[1],
                        args.get(2).map(|v| parse_cli_bool(v)).unwrap_or(true),
                    )
                    .await;
                }
            }
            "unhide" => {
                if args.len() < 2 {
                    self.out(format!(
                        "  {} {} room unhide <房间ID>",
                        c::yellow("?"),
                        c::bold("用法")
                    ));
                } else {
                    self.room_hide(args[1], false).await;
                }
            }
            "close" => {
                if args.len() < 2 {
                    self.out(format!(
                        "  {} {} room close <房间ID>",
                        c::yellow("?"),
                        c::bold("用法")
                    ));
                } else {
                    self.close_room(args[1]).await;
                }
            }
            "set" => {
                if args.len() < 4 {
                    self.out(format!(
                        "  {} {} room set <房间ID> <字段> <值>",
                        c::yellow("?"),
                        c::bold("用法")
                    ));
                } else {
                    self.room_set(args[1], args[2], &args[3..].join(" ")).await;
                }
            }
            "history" => {
                if args.len() < 2 {
                    self.out(format!(
                        "  {} {} room history <房间ID>",
                        c::yellow("?"),
                        c::bold("用法")
                    ));
                } else {
                    self.room_history(args[1]).await;
                }
            }
            "rounds" => {
                if args.len() < 2 {
                    self.out(format!(
                        "  {} {} room rounds <房间ID>",
                        c::yellow("?"),
                        c::bold("用法")
                    ));
                } else {
                    self.room_rounds(args[1]).await;
                }
            }
            "round" => {
                if args.len() < 2 {
                    self.out(format!(
                        "  {} {} room round <轮次UUID>",
                        c::yellow("?"),
                        c::bold("用法")
                    ));
                } else {
                    self.room_round_info(args[1]).await;
                }
            }
            "ban" => {
                if args.len() < 3 {
                    self.out(format!(
                        "  {} {} room ban <房间ID> <用户ID> [原因]",
                        c::yellow("?"),
                        c::bold("用法")
                    ));
                } else {
                    let uid: i32 = match args[2].parse() {
                        Ok(id) => id,
                        Err(_) => {
                            self.out(format!("  {} 无效的用户ID", c::red("✗")));
                            return;
                        }
                    };
                    let reason = if args.len() > 3 {
                        args[3..].join(" ")
                    } else {
                        String::new()
                    };
                    match self.find_room(args[1]).await {
                        Some(room) => {
                            let uuid = room.uuid.to_string();
                            match self
                                .state
                                .ban_manager
                                .room_ban_user(&uuid, uid, &reason)
                                .await
                            {
                                Ok(_) => {
                                    let target = room
                                        .users()
                                        .await
                                        .into_iter()
                                        .chain(room.monitors().await)
                                        .find(|user| user.id == uid);
                                    let mut removed = false;
                                    if let Some(target) = target {
                                        let detail = if reason.trim().is_empty() {
                                            "你已被加入该房间的黑名单".to_string()
                                        } else {
                                            format!("你已被加入该房间的黑名单：{}", reason.trim())
                                        };
                                        target
                                            .try_send(phira_mp_common::ServerCommand::Message(
                                                phira_mp_common::Message::Chat {
                                                    user: 0,
                                                    content: detail,
                                                },
                                            ))
                                            .await;
                                        removed = self
                                            .state
                                            .room_commands
                                            .kick_user(&self.state, args[1], uid)
                                            .await
                                            .is_ok();
                                    }
                                    self.out(format!(
                                        "  {} 用户 {} 已加入房间 {} ({}) 的黑名单{}{}",
                                        c::green("✓"),
                                        uid,
                                        args[1],
                                        &uuid[..8],
                                        if reason.is_empty() {
                                            String::new()
                                        } else {
                                            format!("，原因：{reason}")
                                        },
                                        if removed {
                                            "，并已立即移出房间"
                                        } else {
                                            ""
                                        }
                                    ));
                                }
                                Err(e) => self.out(format!("  {} {}", c::red("✗"), e)),
                            }
                        }
                        None => self.out(format!("  {} 未找到房间 {}", c::red("✗"), args[1])),
                    }
                }
            }
            "unban" => {
                if args.len() < 3 {
                    self.out(format!(
                        "  {} {} room unban <房间ID> <用户ID>",
                        c::yellow("?"),
                        c::bold("用法")
                    ));
                } else {
                    let uid: i32 = match args[2].parse() {
                        Ok(id) => id,
                        Err(_) => {
                            self.out(format!("  {} 无效的用户ID", c::red("✗")));
                            return;
                        }
                    };
                    let room_uuid = self.find_room(args[1]).await.map(|r| r.uuid.to_string());
                    match room_uuid {
                        Some(uuid) => {
                            match self.state.ban_manager.room_unban_user(&uuid, uid).await {
                                Ok(_) => self.out(format!(
                                    "  {} 用户 {} 已移出房间 {} 的黑名单",
                                    c::green("✓"),
                                    uid,
                                    args[1]
                                )),
                                Err(e) => self.out(format!("  {} {}", c::red("✗"), e)),
                            }
                        }
                        None => self.out(format!("  {} 未找到房间 {}", c::red("✗"), args[1])),
                    }
                }
            }
            "banlist" => {
                if args.len() < 2 {
                    self.out(format!(
                        "  {} {} room banlist <房间ID>",
                        c::yellow("?"),
                        c::bold("用法")
                    ));
                } else {
                    let room_uuid = self.find_room(args[1]).await.map(|r| r.uuid.to_string());
                    match room_uuid {
                        Some(uuid) => self.room_ban_list(&uuid, args[1]).await,
                        None => self.out(format!("  {} 未找到房间 {}", c::red("✗"), args[1])),
                    }
                }
            }
            "uuid" => {
                if args.len() < 2 {
                    self.out(format!(
                        "  {} {} room uuid <房间ID>",
                        c::yellow("?"),
                        c::bold("用法")
                    ));
                } else {
                    self.room_show_uuid(args[1]).await;
                }
            }
            _ => {
                self.out(format!(
                    "  {} 未知子命令: {}  ",
                    c::red("✗"),
                    c::yellow(sub)
                ));
                self.out(format!("  {} 可用: room list|create-empty|info|start|force-start|cancel|kick|host|force-move|hide|unhide|close|set|history|rounds|round|uuid|ban|unban|banlist", c::dim("▸")));
            }
        }
    }
}

impl CliHandler {
    pub(crate) async fn kick_from_room(&self, room_id: &str, target_id: &str) {
        let target: i32 = match target_id.parse() {
            Ok(id) => id,
            Err(_) => {
                self.out(format!("  {} 无效的用户ID: {}", c::red("✗"), target_id));
                return;
            }
        };

        match self
            .state
            .room_commands
            .kick_user(&self.state, room_id, target)
            .await
        {
            Ok(value) => {
                let name = value
                    .get("user_name")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                if name.is_empty() {
                    self.out(format!(
                        "  {} 用户 {} 已从房间 {} 踢出",
                        c::green("✓"),
                        target,
                        room_id
                    ));
                } else {
                    self.out(format!(
                        "  {} 用户 {} ({}) 已从房间 {} 踢出",
                        c::green("✓"),
                        name,
                        target,
                        room_id
                    ));
                }
            }
            Err(e) => self.out(format!("  {} {}", c::red("✗"), e)),
        }
    }

    pub(crate) async fn kick_user(&self, target_id: &str) {
        let target: i32 = match target_id.parse() {
            Ok(id) => id,
            Err(_) => {
                self.out(format!("  {} 无效的用户ID: {}", c::red("✗"), target_id));
                return;
            }
        };

        match crate::server::run_admin_kick_user(&self.state, target, "kicked by admin").await {
            Ok(_) => self.out(format!(
                "  {} 用户 {} 已从服务器踢出",
                c::green("✓"),
                target
            )),
            Err(error) => self.out(format!("  {} {}", c::red("✗"), error)),
        }
    }

    pub(crate) async fn close_room(&self, room_id: &str) {
        match self
            .state
            .room_commands
            .close_room(&self.state, room_id)
            .await
        {
            Ok(_) => self.out(format!(
                "  {} 房间 {} 已解散",
                c::green("✓"),
                c::bold(room_id)
            )),
            Err(e) => self.out(format!("  {} {}", c::red("✗"), e)),
        }
    }

    /// 从字符串查找房间
    pub(crate) async fn find_room(&self, room_id: &str) -> Option<Arc<crate::room::Room>> {
        let rid: phira_mp_common::RoomId = room_id.to_string().try_into().ok()?;
        self.state.rooms.read().await.get(&rid).map(Arc::clone)
    }

    pub(crate) async fn room_info(&self, room_id: &str) {
        let room = match self.find_room(room_id).await {
            Some(r) => r,
            None => {
                self.out(format!("  {} 未找到房间 {}", c::red("✗"), room_id));
                return;
            }
        };
        self.state.refresh_room_display_metadata(&room).await;
        let users = room.users().await;
        let monitors = room.monitors().await;
        let control = room.control_snapshot();
        let snap = if let Some(server) = room.server.upgrade() {
            server.room_snapshot(&room.id.to_string())
        } else {
            None
        };
        let state_str = match snap.as_ref().map(|s| s.stripped) {
            Some(phira_mp_common::StrippedRoomState::SelectingChart) | None => "SelectChart",
            Some(phira_mp_common::StrippedRoomState::WaitingForReady) => "WaitForReady",
            Some(phira_mp_common::StrippedRoomState::Playing) => "Playing",
        };
        let locked = if control.locked { c::yellow("锁定") } else { c::dim("未锁定") };
        let cycling = if control.cycle { c::cyan("轮换") } else { c::dim("不轮换") };
        let hidden = if control.hidden { c::magenta("隐藏") } else { c::dim("公开") };
        let chart_info = match snap.as_ref().and_then(|s| s.chart) {
            Some(cid) => format!("id={}", cid),
            None => "未选择".to_string(),
        };
        let endpoint_override = control.phira_api_endpoint.clone();
        let endpoint_info = endpoint_override
            .unwrap_or_else(|| self.state.config.phira_api_endpoint.clone());
        let endpoint_mode = if control.phira_api_endpoint.is_some() {
            "房间覆盖"
        } else {
            "全局默认"
        };
        let host_name = match control.host_id {
            Some(hid) => {
                let user = users.iter().chain(monitors.iter()).find(|u| u.id == hid);
                match user {
                    Some(user) => user.name.clone(),
                    None => hid.to_string(),
                }
            }
            None if control.system_host => "?（系统房主）".to_string(),
            None => "无（等待首个玩家）".to_string(),
        };

        self.out(format!("  {} 房间: {}", c::green("◆"), c::bold(room_id)));
        let persistent = if control.persistent_empty {
            c::cyan("无人保留")
        } else {
            c::dim("无人清除")
        };
        self.out(format!(
            "  {} 状态: {} | {} | {} | {} | {}",
            c::dim("│"),
            state_str,
            locked,
            cycling,
            hidden,
            persistent
        ));
        self.out(format!("  {} 房主: {}", c::dim("│"), host_name));
        self.out(format!("  {} 谱面: {}", c::dim("│"), chart_info));
        self.out(format!(
            "  {} Phira API: {} ({})",
            c::dim("│"),
            endpoint_info,
            endpoint_mode
        ));
        let mut user_labels = Vec::new();
        for u in &users {
            user_labels.push(format!("{}({})", u.name, u.id));
        }
        self.out(format!(
            "  {} 玩家: {}",
            c::dim("│"),
            user_labels.join(", ")
        ));
        if !monitors.is_empty() {
            let mut monitor_labels = Vec::new();
            for u in &monitors {
                monitor_labels.push(format!("{}({})", u.name, u.id));
            }
            self.out(format!(
                "  {} 旁观: {}",
                c::dim("│"),
                monitor_labels.join(", ")
            ));
        }
        // 历史记录统计
        let total_rounds = room.play_history.len().await;
        if total_rounds > 0 {
            self.out(format!("  {} 历史对局: {} 轮", c::dim("│"), total_rounds));
        }
    }

    /// 由管理员发起游戏，等待所有客户端完成谱面加载后再开始。
    pub(crate) async fn room_start(&self, room_id: &str) {
        match self
            .state
            .room_commands
            .start_room(&self.state, room_id)
            .await
        {
            Ok(_) => self.out(format!(
                "  {} 已发起游戏，正在等待玩家和监控端加载谱面",
                c::green("✓")
            )),
            Err(e) => self.out(format!("  {} 无法开始游戏: {}", c::red("✗"), e)),
        }
    }

    /// 取消准备状态（管理员操作）
    pub(crate) async fn room_cancel(&self, room_id: &str) {
        match self
            .state
            .room_commands
            .cancel_start(&self.state, room_id)
            .await
        {
            Ok(value) => {
                if value
                    .get("canceled")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false)
                {
                    self.out(format!("  {} 已取消准备状态", c::green("✓")));
                } else {
                    self.out(format!("  {} 当前状态不需要取消", c::yellow("!")));
                }
            }
            Err(e) => self.out(format!("  {} {}", c::red("✗"), e)),
        }
    }

    pub(crate) async fn room_set_host(&self, room_id: &str, target: Option<i32>) {
        match self
            .state
            .room_commands
            .set_host(&self.state, room_id, target)
            .await
        {
            Ok(value) => {
                if value
                    .get("host_is_system")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false)
                {
                    self.out(format!("  {} 房主已设为系统 ?", c::green("✓")));
                } else {
                    let host = value
                        .get("host")
                        .and_then(|v| v.as_i64())
                        .unwrap_or_default();
                    let name = value
                        .get("host_name")
                        .and_then(|v| v.as_str())
                        .unwrap_or("");
                    self.out(format!(
                        "  {} 房主已设为用户 {} ({})",
                        c::green("✓"),
                        name,
                        host
                    ));
                }
            }
            Err(e) => self.out(format!("  {} {}", c::red("✗"), e)),
        }
    }

    pub(crate) async fn room_create_empty(&self, room_id: &str, endpoint: Option<&str>) {
        let endpoint = match endpoint {
            Some(value) => match crate::server::parse_room_endpoint_value(value) {
                Ok(endpoint) => endpoint,
                Err(e) => {
                    self.out(format!("  {} {}", c::red("✗"), e));
                    return;
                }
            },
            None => None,
        };
        match self.state.create_empty_room(room_id, endpoint, true).await {
            Ok(res) => {
                let effective = res
                    .get("phira_api_endpoint")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                self.out(format!(
                    "  {} 已创建无人持久房间 {}，Phira API: {}",
                    c::green("✓"),
                    c::bold(room_id),
                    effective
                ));
            }
            Err(e) => self.out(format!("  {} {}", c::red("✗"), e)),
        }
    }

    pub(crate) async fn room_force_move(&self, room_id: &str, user_id: i32, monitor: bool) {
        match self
            .state
            .force_move_user_to_room(room_id, user_id, monitor)
            .await
        {
            Ok(_) => self.out(format!(
                "  {} 已强制转移用户 {} 到房间 {}{}",
                c::green("✓"),
                user_id,
                c::bold(room_id),
                if monitor { "（旁观）" } else { "" }
            )),
            Err(e) => self.out(format!("  {} {}", c::red("✗"), e)),
        }
    }

    pub(crate) async fn room_hide(&self, room_id: &str, hidden: bool) {
        match self
            .state
            .room_commands
            .set_hidden(&self.state, room_id, hidden)
            .await
        {
            Ok(_) => self.out(format!(
                "  {} 房间 {} 已{}隐藏",
                c::green("✓"),
                c::bold(room_id),
                if hidden { "设为" } else { "取消" }
            )),
            Err(e) => self.out(format!("  {} {}", c::red("✗"), e)),
        }
    }

    pub(crate) async fn room_set(&self, room_id: &str, field: &str, value: &str) {
        let room = match self.find_room(room_id).await {
            Some(r) => r,
            None => {
                self.out(format!("  {} 未找到房间 {}", c::red("✗"), room_id));
                return;
            }
        };
        match field {
            "lock" => {
                let v = value == "true" || value == "1" || value == "锁定";
                match self
                    .state
                    .room_commands
                    .set_lock(&self.state, room_id, v)
                    .await
                {
                    Ok(_) => self.out(format!(
                        "  {} 房间 {} 已{}锁定",
                        c::green("✓"),
                        room_id,
                        if v { "" } else { "解除" }
                    )),
                    Err(e) => self.out(format!("  {} {}", c::red("✗"), e)),
                }
            }
            "cycle" => {
                let v = value == "true" || value == "1" || value == "轮换";
                match self
                    .state
                    .room_commands
                    .set_cycle(&self.state, room_id, v)
                    .await
                {
                    Ok(_) => self.out(format!(
                        "  {} 房间 {} 已{}轮换",
                        c::green("✓"),
                        room_id,
                        if v { "开启" } else { "关闭" }
                    )),
                    Err(e) => self.out(format!("  {} {}", c::red("✗"), e)),
                }
            }
            "hidden" => {
                let v = parse_cli_bool(value);
                match self
                    .state
                    .room_commands
                    .set_hidden(&self.state, room_id, v)
                    .await
                {
                    Ok(_) => self.out(format!(
                        "  {} 房间 {} 已{}隐藏",
                        c::green("✓"),
                        room_id,
                        if v { "设为" } else { "取消" }
                    )),
                    Err(e) => self.out(format!("  {} {}", c::red("✗"), e)),
                }
            }
            "persistent" => {
                let v = parse_cli_bool(value);
                match self.state.set_room_persistent_empty(room_id, v).await {
                    Ok(_) => self.out(format!(
                        "  {} 房间 {} 已{}无人保留",
                        c::green("✓"),
                        room_id,
                        if v { "开启" } else { "关闭" }
                    )),
                    Err(e) => self.out(format!("  {} {}", c::red("✗"), e)),
                }
            }
            "phira_api_endpoint" => match crate::server::parse_room_endpoint_value(value) {
                Ok(endpoint) => match self
                    .state
                    .room_commands
                    .set_phira_api_endpoint(&self.state, room_id, endpoint)
                    .await
                {
                    Ok(res) => {
                        let effective = res
                            .get("phira_api_endpoint")
                            .and_then(|v| v.as_str())
                            .unwrap_or("");
                        let using_override = res
                            .get("using_room_override")
                            .and_then(|v| v.as_bool())
                            .unwrap_or(false);
                        if using_override {
                            self.out(format!(
                                "  {} 房间 {} 的 Phira API 已切换为 {}，立即生效",
                                c::green("✓"),
                                room_id,
                                effective
                            ));
                        } else {
                            self.out(format!(
                                "  {} 房间 {} 已恢复使用全局 Phira API {}，立即生效",
                                c::green("✓"),
                                room_id,
                                effective
                            ));
                        }
                    }
                    Err(e) => self.out(format!("  {} {}", c::red("✗"), e)),
                },
                Err(e) => self.out(format!("  {} {}", c::red("✗"), e)),
            },
            "host" => {
                let target = match parse_room_host_target(value) {
                    Ok(target) => target,
                    Err(_) => {
                        self.out(format!(
                            "  {} 无效的房主目标：请使用用户ID或 ?",
                            c::red("✗")
                        ));
                        return;
                    }
                };
                self.room_set_host(room_id, target).await;
            }
            "chart-id" => {
                let room_state = if let Some(server) = room.server.upgrade() {
                    server.room_snapshot(&room.id.to_string())
                        .map(|s| s.stripped)
                } else {
                    None
                };
                if !matches!(room_state, Some(phira_mp_common::StrippedRoomState::SelectingChart) | None) {
                    self.out(format!("  {} 只能在选曲阶段更换谱面", c::yellow("!")));
                    return;
                }
                let cid: i32 = match value.parse() {
                    Ok(id) => id,
                    Err(_) => {
                        self.out(format!("  {} 无效的谱面ID", c::red("✗")));
                        return;
                    }
                };
                let chart_name = match self
                    .state
                    .phira_client
                    .get_json::<crate::server::Chart>(
                        &self.state.config.phira_api_endpoint,
                        None,
                        &format!("/chart/{cid}"),
                        None,
                        crate::phira_client::PhiraRetryNoticeTarget::Silent,
                    )
                    .await
                {
                    Ok(chart) => chart.name,
                    Err(_) => format!("chart_{cid}"),
                };
                // Route chart selection through the gateway.
                self.state
                    .room_commands
                    .set_chart(&self.state, room_id, cid, &chart_name)
                    .await
                    .ok();
                self.out(format!("  {} 谱面已切换为 ID {}", c::green("✓"), cid));
            }
            _ => {
                self.out(format!("  {} 未知字段: {}", c::red("✗"), field));
                self.out(format!("  {} 支持: lock, cycle, hidden, persistent, host, chart-id, phira_api_endpoint", c::dim("▸")));
            }
        }
    }

    pub(crate) async fn room_history(&self, room_id: &str) {
        let room = match self.find_room(room_id).await {
            Some(r) => r,
            None => {
                self.out(format!("  {} 未找到房间 {}", c::red("✗"), room_id));
                return;
            }
        };
        let history = room.play_history.all().await;
        if history.is_empty() {
            self.out(format!("  {} 该房间暂无游玩记录", c::dim("·")));
            return;
        }
        self.out(format!(
            "  {} 房间 {} 游玩记录 ({} 轮)",
            c::green("◆"),
            room_id,
            history.len()
        ));
        self.out(format!(
            "  {}",
            c::dim("  ────────────────────────────────────────────")
        ));
        for (i, round) in history.iter().enumerate() {
            let round_num = i + 1;
            self.out(format!(
                "  {} 第{}轮: {} (id={})",
                c::dim("┏"),
                round_num,
                c::bold(&round.chart_name),
                round.chart_id
            ));
            for r in &round.results {
                let status = if r.aborted {
                    c::yellow(" (放弃)")
                } else {
                    String::new()
                };
                self.out(format!(
                    "  {} {:<6}  得分:{:<8} 准确率:{:<6.2}%  FC:{}{}",
                    c::dim("┃"),
                    format!("{}({})", r.user_name, r.user_id),
                    r.score,
                    r.accuracy * 100.0,
                    if r.full_combo {
                        c::green("✓")
                    } else {
                        c::dim("✗")
                    },
                    status,
                ));
            }
            self.out(format!("  {}", c::dim("  ─ ─ ─ ─ ─ ─")));
        }
    }

    /// 显示房间 UUID
    pub(crate) async fn room_show_uuid(&self, room_id: &str) {
        let room = match self.find_room(room_id).await {
            Some(r) => r,
            None => {
                self.out(format!("  ✗ 未找到房间 {}", room_id));
                return;
            }
        };
        self.out(format!("  ◆ 房间 {}  UUID: {}", room_id, room.uuid));
    }

    /// 列出房间历史轮次
    pub(crate) async fn room_rounds(&self, room_id: &str) {
        let room = match self.find_room(room_id).await {
            Some(r) => r,
            None => {
                self.out(format!("  ✗ 未找到房间 {}", room_id));
                return;
            }
        };
        let history = room.play_history.all().await;
        if history.is_empty() {
            self.out(format!("  · 该房间暂无轮次记录"));
            return;
        }
        self.out(format!("  ◆ 房间 {} 轮次记录 ({})", room_id, history.len()));
        for (i, r) in history.iter().enumerate() {
            self.out(format!(
                "  │ [{i}] 轮次 {}  谱面 {} (id={})  玩家 {}",
                r.round_id,
                r.chart_name,
                r.chart_id,
                r.results.len()
            ));
        }
    }

    /// 按轮次 UUID 查询结算详情
    pub(crate) async fn room_round_info(&self, round_uuid: &str) {
        let rooms = self.state.rooms.read().await;
        for room in rooms.values() {
            let history = room.play_history.all().await;
            if let Some(round) = history
                .iter()
                .find(|r| r.round_id.to_string() == round_uuid)
            {
                self.out(format!("  ◆ 轮次 {round_uuid}"));
                self.out(format!("  │ 房间: {}  UUID: {}", room.id, room.uuid));
                self.out(format!(
                    "  │ 谱面: {} (id={})",
                    round.chart_name, round.chart_id
                ));
                self.out(format!("  │ 玩家: {}", round.results.len()));
                let mut sorted = round.results.clone();
                sorted.sort_by(|a, b| b.score.cmp(&a.score));
                for (i, r) in sorted.iter().enumerate() {
                    let fc = if r.full_combo { " FC" } else { "" };
                    let ab = if r.aborted { " 放弃" } else { "" };
                    self.out(format!(
                        "  │ #{} {}  {}分  {:.1}%{}{}",
                        i + 1,
                        r.user_name,
                        r.score,
                        r.accuracy * 100.0,
                        fc,
                        ab
                    ));
                }
                return;
            }
        }
        self.out(format!("  ✗ 未找到轮次 {round_uuid}"));
    }

    pub(crate) async fn room_ban_list(&self, room_uuid: &str, room_name: &str) {
        let list = self.state.ban_manager.list_room_bans(room_uuid).await;
        if list.is_empty() {
            self.out(format!("  {} 房间 {} 的黑名单为空", c::dim("·"), room_name));
            return;
        }
        self.out(format!(
            "  {} 房间 {} 黑名单 ({})",
            c::green("◆"),
            room_name,
            &room_uuid[..8]
        ));
        for entry in &list {
            self.out(format!(
                "  {} 用户 {}  原因: {}",
                c::dim("│"),
                entry.user_id,
                entry.reason
            ));
        }
    }
}
