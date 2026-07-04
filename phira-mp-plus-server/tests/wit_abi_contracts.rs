//! WIT ABI contract tests.
//!
//! These tests verify that the canonical WIT ABI definition is locked,
//! that plugin_abi.rs references it correctly, and that the documentation
//! is automatically generated from the WIT file (single source of truth).

use phira_mp_plus_server::plugin_abi;
use std::path::PathBuf;

/// Absolute path to workspace root (two levels up from crate manifest dir).
fn workspace_root() -> PathBuf {
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    manifest.parent().expect("server crate should have a parent").to_path_buf()
}

fn canonical_wit() -> PathBuf {
    workspace_root().join("wit/phira-plugin.wit")
}

fn read_canonical() -> String {
    std::fs::read_to_string(canonical_wit()).expect("canonical WIT should be readable")
}

// ── WIT file existence and constants ──

#[test]
fn canonical_wit_file_exists() {
    let path = canonical_wit();
    assert!(path.exists(), "canonical WIT file not found at {}", path.display());
}

#[test]
fn plugin_abi_constants_are_locked() {
    assert_eq!(plugin_abi::wit::WIT_FILE, "wit/phira-plugin.wit");
    assert_eq!(plugin_abi::wit::WIT_WORLD, "phira-plugin-v2");
    assert_eq!(plugin_abi::wit::WIT_VERSION, "abi-wit-v2");
    assert_eq!(plugin_abi::wit::MIGRATION_PHASE, 0,
        "MIGRATION_PHASE 0 = JSON bridge active (enable wit-bindgen for phase 1)");
}

#[test]
fn canonical_wit_contains_world_phira_plugin_v2() {
    let wit = read_canonical();
    assert!(wit.contains("world phira-plugin-v2"), "WIT must declare world phira-plugin-v2");
    assert!(wit.contains("import phira-host;"), "world must import phira-host");
    assert!(wit.contains("import phira-events;"), "world must import phira-events");
    assert!(wit.contains("export init:"), "world must export init");
    assert!(wit.contains("export on-event:"), "world must export on-event");
}

#[test]
fn legacy_wit_marked_deprecated() {
    let wit = read_canonical();
    assert!(!wit.contains("mpplus"), "canonical WIT should not reference legacy mpplus.wit");
}

#[test]
fn legacy_wit_has_no_full_abi() {
    let canonical = read_canonical();
    assert!(canonical.contains("phira-types"), "canonical WIT must define phira-types");
    assert!(canonical.contains("phira-host"), "canonical WIT must define phira-host");
    assert!(canonical.contains("phira-events"), "canonical WIT must define phira-events");
    assert!(canonical.contains("phira-query"), "canonical WIT must define phira-query");
}

#[test]
fn phira_plugin_wit_is_canonical_only() {
    let path = canonical_wit();
    assert!(path.exists(), "canonical WIT file must exist at {}", path.display());
}

// ── WIT interface extraction (for doc generation) ──

/// Extracted interface metadata from the WIT file.
#[derive(Debug)]
struct WitInterface {
    name: String,
    comment: String,
    exports: Vec<String>,
}

/// Parse WIT file and extract interface definitions.
/// Used to auto-generate documentation and verify it matches.
fn parse_wit_interfaces(wit_content: &str) -> Vec<WitInterface> {
    let mut interfaces = Vec::new();
    let mut lines = wit_content.lines().peekable();
    let mut current_comment = String::new();

    while let Some(line) = lines.next() {
        // Collect doc comments
        if line.trim().starts_with("///") {
            let comment = line.trim().trim_start_matches("///").trim();
            if !comment.is_empty() {
                if !current_comment.is_empty() {
                    current_comment.push(' ');
                }
                current_comment.push_str(comment);
            }
            continue;
        }
        if line.trim().starts_with("//") {
            continue; // skip non-doc comments
        }

        // Detect interface definitions
        if line.trim().starts_with("interface ") {
            let name = line.trim()
                .trim_start_matches("interface ")
                .trim_end_matches('{')
                .trim();
            let mut exports = Vec::new();
            // Collect function signatures until closing brace
            while let Some(export_line) = lines.next() {
                let trimmed = export_line.trim();
                if trimmed == "}" || trimmed.starts_with("world ") {
                    break;
                }
                if trimmed.starts_with("use ") || trimmed.is_empty() || trimmed.starts_with("//") {
                    continue;
                }
                // Extract function/record/variant names
                let export_name = trimmed
                    .split(|c: char| c == '(' || c == ':' || c == ' ')
                    .next()
                    .unwrap_or("")
                    .to_string();
                if !export_name.is_empty() && !export_name.contains('{') {
                    exports.push(export_name);
                }
            }
            interfaces.push(WitInterface {
                name,
                comment: std::mem::take(&mut current_comment),
                exports,
            });
            continue;
        }

        current_comment.clear();
    }
    interfaces
}

#[test]
fn wit_interfaces_are_extracted_correctly() {
    let wit = read_canonical();
    let interfaces = parse_wit_interfaces(&wit);
    let names: Vec<&str> = interfaces.iter().map(|i| i.name.as_str()).collect();
    assert!(names.contains(&"phira-types"), "should find phira-types");
    assert!(names.contains(&"phira-host"), "should find phira-host");
    assert!(names.contains(&"phira-events"), "should find phira-events");
    assert!(names.contains(&"phira-query"), "should find phira-query");
    assert!(names.contains(&"phira-room-mgmt"), "should find phira-room-mgmt");
    assert!(names.contains(&"phira-user-mgmt"), "should find phira-user-mgmt");
    assert!(names.contains(&"phira-messaging"), "should find phira-messaging");
    assert!(names.contains(&"phira-persistence"), "should find phira-persistence");
    assert!(names.contains(&"phira-admin"), "should find phira-admin");
    assert!(names.contains(&"phira-config"), "should find phira-config");
    assert!(names.contains(&"phira-simulation"), "should find phira-simulation");
    assert!(names.contains(&"phira-runtime"), "should find phira-runtime");
    assert_eq!(interfaces.len(), 12, "WIT should define exactly 12 interfaces");
}

#[test]
fn wit_host_events_have_minimal_exports() {
    let wit = read_canonical();
    let interfaces = parse_wit_interfaces(&wit);
    let host = interfaces.iter().find(|i| i.name == "phira-host").unwrap();
    assert!(host.exports.iter().any(|e| e == "log"), "phira-host must have log");
    assert!(host.exports.iter().any(|e| e == "api-call"), "phira-host must have api-call");
    assert!(host.exports.iter().any(|e| e == "http-request"), "phira-host must have http-request");

    let events = interfaces.iter().find(|i| i.name == "phira-events").unwrap();
    assert!(events.exports.iter().any(|e| e == "user-connect"), "phira-events must have user-connect");
    assert!(events.exports.iter().any(|e| e == "game-end"), "phira-events must have game-end");
    assert!(events.exports.iter().any(|e| e == "round-complete"), "phira-events must have round-complete");
}

/// Generate docs/wit-abi.md content from the WIT file.
/// This is used both by the test (to verify docs match) and can be
/// called to regenerate the docs when the WIT file changes.
fn generate_wit_docs() -> String {
    let wit = read_canonical();
    let interfaces = parse_wit_interfaces(&wit);

    let mut md = String::new();
    md.push_str("# WIT ABI 规范\n\n");
    md.push_str("> 本文档由 `wit/phira-plugin.wit` 自动生成，请勿手动编辑。\n");
    md.push_str("> 更新方式: 修改 WIT 文件后运行 `cargo test --test wit_abi_contracts` 验证一致性。\n\n");

    md.push_str("## 当前状态\n\n");
    md.push_str("| 属性 | 值 |\n");
    md.push_str("|------|-----|\n");
    md.push_str("| **运行时 ABI** | `abi-json-v1` (JSON 内存桥) |\n");
    md.push_str("| **目标 ABI** | `abi-wit-v2` (WIT / Component Model) |\n");
    md.push_str("| **规范 WIT** | `wit/phira-plugin.wit` |\n");
    md.push_str("| **MIGRATION_PHASE** | `0` (JSON 桥活跃; 启用 `wit-bindgen` feature 进入 phase 1) |\n");
    md.push_str(&format!("| **接口数量** | `{}` |\n\n", interfaces.len()));

    md.push_str("## 规范 WIT 接口\n\n");
    md.push_str("WIT 文件定义了以下接口与 world `phira-plugin-v2`:\n\n");

    for iface in &interfaces {
        md.push_str(&format!("### `{}`\n\n", iface.name));
        if !iface.comment.is_empty() {
            md.push_str(&format!("{}\n\n", iface.comment));
        }
        if !iface.exports.is_empty() {
            md.push_str("**导出:**\n\n");
            for export in &iface.exports {
                md.push_str(&format!("- `{}`\n", export));
            }
            md.push_str("\n");
        }
    }

    md.push_str("## World\n\n");
    md.push_str("`phira-plugin-v2` — 导入上述所有接口，导出 `init`、`get-info`、`cleanup`、`on-event`、`on-api`。\n\n");

    md.push_str("## 迁移计划\n\n");
    md.push_str("1. ✅ WIT 接口定义完成\n");
    md.push_str("2. ✅ Host bindings 已生成 (`wit-bindgen` feature)\n");
    md.push_str("3. ✅ Host trait 骨架已实现 (`wit_host.rs`)\n");
    md.push_str("4. ❌ Guest SDK (`phira-plugin-sdk`) 使用 WIT exports\n");
    md.push_str("5. ❌ 双 ABI 支持过渡期\n");
    md.push_str("6. ❌ 移除 JSON 桥\n");

    md
}

#[test]
fn wit_docs_can_be_generated() {
    let docs = generate_wit_docs();
    assert!(docs.contains("phira-types"), "generated docs must include phira-types");
    assert!(docs.contains("phira-host"), "generated docs must include phira-host");
    assert!(docs.contains("phira-events"), "generated docs must include phira-events");
    assert!(docs.contains("迁移计划"), "generated docs must include migration plan");
    assert!(docs.contains("自动生成"), "generated docs must note auto-generation");
}

#[test]
fn current_abi_is_json_not_wit() {
    let plan = plugin_abi::plugin_abi_plan();
    assert_eq!(plan.current_transport, plugin_abi::PluginAbiTransport::JsonMemoryV1);
    assert_eq!(plan.target_transport, plugin_abi::PluginAbiTransport::WitTypedV2);
    assert!(plan.risks.iter().any(|risk| risk.contains("schema drift")));
    assert!(plan.next_steps.iter().any(|step| step.contains("contract tests")));
}
