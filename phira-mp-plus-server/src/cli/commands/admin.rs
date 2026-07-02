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

    pub(in crate::cli) async fn dispatch_extension_get_command(&self, args: &[&str]) {
        if args.len() < 2 {
            self.out(format!(
                "  {} {} <用户ID|房间ID> <key>",
                c::yellow("?"),
                c::bold("ext-get")
            ));
        } else {
            self.get_extension(args[0], args[1]).await;
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
}
