//! Runtime diagnostic command specifications.
//!
//! D3 收敛：runtime 合并为单命令，一次打印全部诊断分区
//!（command registry / phira / events / schema / persistence / latency）。

use crate::command_registry::CommandSpec;

use super::with_args;

pub fn specs() -> Vec<CommandSpec> {
    vec![CommandSpec::new(
        "runtime",
        "runtime",
        "查看 Runtime 诊断（command registry / phira / events / schema / persistence / latency 一次出完）。",
        "runtime",
    )
    .advanced()
    .handler(with_args(|h, _args| {
        Box::pin(async move { h.print_runtime_all().await })
    }))]
}
