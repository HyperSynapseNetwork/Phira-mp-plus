use super::super::*;

impl CliHandler {
    pub(in crate::cli) async fn dispatch_broadcast_command(&self, args: &[&str]) {
        if args.is_empty() {
            self.out(format!(
                "  {} {} all <消息>          广播给所有用户",
                c::yellow("?"),
                c::bold("broadcast")
            ));
            self.out(format!(
                "  {} {} room <房间ID> <消息>  广播给指定房间",
                c::dim("▸"),
                c::bold("broadcast")
            ));
            self.out(format!(
                "  {} {} user <用户ID> <消息>  发送给指定用户",
                c::dim("▸"),
                c::bold("broadcast")
            ));
            return;
        }

        let scope = args[0];
        let rest = &args[1..];
        match scope {
            "all" => self.broadcast_all(&rest.join(" ")).await,
            "room" => {
                if rest.len() < 2 {
                    self.out(format!(
                        "  {} {} room <房间ID> <消息>",
                        c::yellow("?"),
                        c::bold("broadcast")
                    ));
                } else {
                    self.broadcast_room(rest[0], &rest[1..].join(" ")).await;
                }
            }
            "user" => {
                if rest.len() < 2 {
                    self.out(format!(
                        "  {} {} user <用户ID> <消息>",
                        c::yellow("?"),
                        c::bold("broadcast")
                    ));
                } else {
                    let uid: i32 = match rest[0].parse() {
                        Ok(id) => id,
                        Err(_) => {
                            self.out(format!("  {} 无效的用户ID", c::red("✗")));
                            return;
                        }
                    };
                    self.broadcast_user(uid, &rest[1..].join(" ")).await;
                }
            }
            _ => {
                self.out(format!(
                    "  {} 未知 broadcast 范围: {}",
                    c::red("✗"),
                    c::yellow(scope)
                ));
                self.out(format!("  {} 可用: broadcast all|room|user", c::dim("▸")));
            }
        }
    }
}

impl CliHandler {
    pub(crate) async fn broadcast_all(&self, message: &str) {
        let users = {
            let users = self.state.users.read().await;
            users.values().cloned().collect::<Vec<_>>()
        };
        let content = format!("[系统广播] {}", message);
        let msg = phira_mp_common::ServerCommand::Message(phira_mp_common::Message::Chat {
            user: 0,
            content,
        });
        let mut sent = 0usize;
        for user in &users {
            user.try_send(msg.clone()).await;
            sent += 1;
        }
        info!(sent, message = %message, "broadcast to all");
        self.state
            .plugin_manager
            .trigger(&PluginEvent::RoomModify {
                user_id: 0,
                room_id: "*broadcast*".to_string(),
                data: serde_json::json!({
                    "action": "broadcast",
                    "scope": "all",
                    "message": message,
                })
                .to_string(),
            })
            .await;
        self.out(format!("  {} 已广播给 {} 个用户", c::green("✓"), sent));
    }

    /// 广播给指定房间
    pub(crate) async fn broadcast_room(&self, room_id: &str, message: &str) {
        let room = match self.find_room(room_id).await {
            Some(r) => r,
            None => {
                self.out(format!("  {} 未找到房间 {}", c::red("✗"), room_id));
                return;
            }
        };
        let content = format!("[房间广播] {}", message);
        room.send(phira_mp_common::Message::Chat { user: 0, content })
            .await;
        let users = room.users().await.len();
        info!(room = room_id, message = %message, "broadcast to room");
        self.state
            .plugin_manager
            .trigger(&PluginEvent::RoomModify {
                user_id: 0,
                room_id: room_id.to_string(),
                data: serde_json::json!({
                    "action": "broadcast",
                    "scope": "room",
                    "message": message,
                })
                .to_string(),
            })
            .await;
        self.out(format!("  {} 已发送房间广播 ({} 人)", c::green("✓"), users));
    }

    /// 发送给指定用户
    pub(crate) async fn broadcast_user(&self, user_id: i32, message: &str) {
        let user = {
            let users = self.state.users.read().await;
            users.get(&user_id).cloned()
        };
        if let Some(user) = user {
            let content = format!("[管理员消息] {}", message);
            user.try_send(phira_mp_common::ServerCommand::Message(
                phira_mp_common::Message::Chat { user: 0, content },
            ))
            .await;
            info!(user = user_id, message = %message, "message to user");
            self.out(format!("  {} 已发送给用户 {}", c::green("✓"), user_id));
        } else {
            self.out(format!("  {} 未找到用户 {}", c::red("✗"), user_id));
        }
    }
}
