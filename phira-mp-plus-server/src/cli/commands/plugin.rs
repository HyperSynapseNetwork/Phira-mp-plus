use super::super::*;

impl CliHandler {
    pub(in crate::cli) async fn dispatch_plugin_command(&self, args: &[&str]) {
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
}
