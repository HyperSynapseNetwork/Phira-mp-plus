use super::super::*;

impl CliHandler {
    pub(in crate::cli) async fn dispatch_broadcast_command(&self, args: &[&str]) {
        if args.is_empty() {
            self.out(format!("  {} {} all <消息>          广播给所有用户", c::yellow("?"), c::bold("broadcast")));
            self.out(format!("  {} {} room <房间ID> <消息>  广播给指定房间", c::dim("▸"), c::bold("broadcast")));
            self.out(format!("  {} {} user <用户ID> <消息>  发送给指定用户", c::dim("▸"), c::bold("broadcast")));
            return;
        }

        let scope = args[0];
        let rest = &args[1..];
        match scope {
            "all" => self.broadcast_all(&rest.join(" ")).await,
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
                self.out(format!("  {} 未知 broadcast 范围: {}", c::red("✗"), c::yellow(scope)));
                self.out(format!("  {} 可用: broadcast all|room|user", c::dim("▸")));
            }
        }
    }
}
