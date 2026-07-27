use super::super::*;

impl CliHandler {
    pub(in crate::cli) async fn dispatch_user_kick_command(&self, args: &[&str]) {
        if args.len() == 1 {
            self.kick_user(args[0]).await;
        } else {
            self.out(format!("  {} {} <用户ID>", c::yellow("?"), c::bold("kick")));
            self.out(format!(
                "  {} 房间踢人请使用: room kick <房间ID> <用户ID>",
                c::dim("▸")
            ));
        }
    }

    pub(in crate::cli) async fn dispatch_extension_command(&self, args: &[&str]) {
        match args.first().copied() {
            Some("list") => {
                self.list_extensions().await;
            }
            Some("get") => {
                let get_args = if args.len() > 2 { &args[1..] } else { &[] };
                if get_args.len() < 2 {
                    self.out(format!(
                        "  {} {} <用户ID|房间ID> <key>",
                        c::yellow("?"),
                        c::bold("extension get")
                    ));
                } else {
                    self.get_extension(get_args[0], get_args[1]).await;
                }
            }
            _ => {
                self.out("  extension list — 查看扩展字段列表".to_string());
                self.out("  extension get <target> <key> — 查看扩展数据".to_string());
            }
        }
    }

    pub(in crate::cli) async fn dispatch_global_ban_command(&self, args: &[&str]) {
        if args.is_empty() {
            self.out(format!(
                "  {} {} <用户ID> [原因]",
                c::yellow("?"),
                c::bold("ban")
            ));
        } else {
            let reason = if args.len() >= 2 {
                args[1..].join(" ")
            } else {
                "违规行为".to_string()
            };
            self.ban_user(args[0], &reason).await;
        }
    }

    pub(in crate::cli) async fn dispatch_global_unban_command(&self, args: &[&str]) {
        if args.is_empty() {
            self.out(format!(
                "  {} {} <用户ID>",
                c::yellow("?"),
                c::bold("unban")
            ));
        } else {
            self.unban_user(args[0]).await;
        }
    }

    // ── IP 封禁 ───────────────────────────────────────────────────────

    pub(in crate::cli) async fn dispatch_ban_ip_command(&self, args: &[&str]) {
        if args.len() < 1 {
            self.out(format!("  {} {} <Phira ID|IP> [原因]", c::yellow("?"), c::bold("ban ip")));
            return;
        }
        let reason = if args.len() >= 2 {
            args[1..].join(" ")
        } else {
            "违规行为".to_string()
        };

        // 先试试当 IP 处理
        if let Ok(ip) = args[0].parse::<std::net::IpAddr>() {
            match self.state.ban_manager.ban_ip(ip, &reason).await {
                Ok(()) => self.out(format!("  {} IP {} 已封禁", c::green("✓"), ip)),
                Err(e) => self.out(format!("  {} {}", c::red("✗"), e)),
            }
            return;
        }

        // 不是 IP 就当用户 ID 处理——查他所有 IP 全封了
        let uid: i32 = match args[0].parse() {
            Ok(id) => id,
            Err(_) => {
                self.out(format!("  {} '{}' 既不是 IP 也不是用户 ID", c::red("✗"), args[0]));
                return;
            }
        };
        let crate::db::DbManager::Pg(pool) = &self.state.db_manager;
        let count = self.state.ban_manager.ban_user_ips(uid, &reason, pool).await;
            self.out(format!(
                "  {} 用户 #{} 名下 {} 个 IP 已封禁",
                c::green("✓"), uid, count
            ));
        }
    }

    pub(in crate::cli) async fn dispatch_unban_ip_command(&self, args: &[&str]) {
        if args.is_empty() {
            self.out(format!("  {} {} <IP>", c::yellow("?"), c::bold("unban ip")));
            return;
        }
        let ip: std::net::IpAddr = match args[0].parse() {
            Ok(ip) => ip,
            Err(_) => {
                self.out(format!("  {} 无效的 IP 地址: {}", c::red("✗"), args[0]));
                return;
            }
        };
        match self.state.ban_manager.unban_ip(ip).await {
            Ok(()) => self.out(format!("  {} IP {} 已解封", c::green("✓"), ip)),
            Err(e) => self.out(format!("  {} {}", c::red("✗"), e)),
        }
    }

    pub(in crate::cli) async fn dispatch_user_ip_history(&self, args: &[&str]) {
        if args.is_empty() {
            self.out(format!("  {} {} <用户ID>", c::yellow("?"), c::bold("ip-history")));
            return;
        }
        let uid: i32 = match args[0].parse() {
            Ok(id) => id,
            Err(_) => {
                self.out(format!("  {} 无效的用户 ID: {}", c::red("✗"), args[0]));
                return;
            }
        };
        let crate::db::DbManager::Pg(pool) = &self.state.db_manager;
        let records = self.state.ban_manager.user_ip_history(uid, pool).await;
            if records.is_empty() {
                self.out(format!("  ○ 用户 #{} 没有 IP 记录", uid));
                return;
            }
            self.out(format!("  ◆ 用户 #{} 使用过的 IP：", uid));
            for r in &records {
                let seen = chrono::DateTime::from_timestamp_millis(r.last_seen_at)
                    .map(|t| t.format("%Y-%m-%d").to_string())
                    .unwrap_or_else(|| "?".to_string());
                self.out(format!(
                    "    {:<16}  {} 次  ·  最近 {}",
                    r.ip, r.use_count, seen
                ));
            }
        }
    }

    pub(in crate::cli) async fn dispatch_ip_banlist(&self) {
        let bans = self.state.ban_manager.list_ip_bans().await;
        if bans.is_empty() {
            self.out("  ○ 没有 IP 被封禁".to_string());
            return;
        }
        self.out(format!("  ◆ 被封禁的 IP ({} 个)：", bans.len()));
        // 取前 50 条
        for entry in bans.iter().take(50) {
            self.out(format!("    {}  ·  {}", entry.ip, entry.reason));
        }
    }
}
