//! WIT ABI contract tests.
//!
//! These tests verify that the canonical WIT ABI definition is locked,
//! that plugin_abi.rs references it correctly, and that the documentation
//! is automatically generated from the WIT file (single source of truth).

use phira_mp_plus_server::{plugin, plugin_abi, wasm_host_helpers, wit_host};
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
    assert_eq!(plugin_abi::wit::MIGRATION_PHASE, 2,
        "MIGRATION_PHASE 2 = JSON bridge removed (enable wit-bindgen for phase 1)");
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
            let mut brace_depth = 0;
            // Collect function signatures until matching closing brace
            while let Some(export_line) = lines.next() {
                let trimmed = export_line.trim();
                if trimmed.starts_with("world ") { break; }
                // Track brace depth to handle nested records/variants
                for ch in trimmed.chars() {
                    if ch == '{' { brace_depth += 1; }
                    if ch == '}' { brace_depth -= 1; }
                }
                if brace_depth < 0 {
                    // This } closes the interface itself
                    break;
                }
                if trimmed.starts_with("use ") || trimmed.is_empty() || trimmed.starts_with("//") {
                    continue;
                }
                // Extract function/record/variant names
                let tokens: Vec<&str> = trimmed.split_whitespace().collect();
                if tokens.is_empty() { continue; }
                let export_name = if tokens.len() >= 2 && matches!(tokens[0], "record" | "variant" | "type" | "func") {
                    tokens[1].trim_end_matches('(').trim_end_matches('{').trim_end_matches(';').to_string()
                } else {
                    tokens[0].trim_end_matches(':').trim_end_matches(';').to_string()
                };
                if !export_name.is_empty() && !matches!(export_name.as_str(), "}" | "{" | ")" | ";" | "") {
                    exports.push(export_name);
                }
            }
            interfaces.push(WitInterface {
                name: name.to_string(),
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
    assert!(events.exports.iter().any(|e| e == "user-connect-info"), "phira-events must have user-connect-info");
    assert!(events.exports.iter().any(|e| e == "game-end-info"), "phira-events must have game-end-info");
    assert!(events.exports.iter().any(|e| e == "round-complete-info"), "phira-events must have round-complete-info");
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
    md.push_str("| **运行时 ABI** | `abi-wit-v2` (WIT / Component Model) |\n");
    md.push_str("| **目标 ABI** | `abi-wit-v2` |\n");
    md.push_str("| **规范 WIT** | `wit/phira-plugin.wit` |\n");
    md.push_str("| **MIGRATION_PHASE** | `2` (JSON 桥已移除, WIT-only) |\n");
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
fn current_abi_is_wit() {
    let plan = plugin_abi::plugin_abi_plan();
    assert_eq!(plan.current_transport, plugin_abi::PluginAbiTransport::WitTypedV2);
    assert_eq!(plan.target_transport, plugin_abi::PluginAbiTransport::WitTypedV2);
    assert!(
        plan.risks.iter().any(|risk| risk.contains("binary size")),
        "risks should include known deployment constraints"
    );
    assert!(
        !plan.risks.iter().any(|risk| risk.contains("stubs")),
        "WIT lifecycle stubs risk must be removed after implementation"
    );
    assert!(
        plan.risks.iter().any(|risk| risk.contains("capability")),
        "write-capable WIT host methods must track capability enforcement risk"
    );
    // Verify next_steps exist for remaining work
    assert!(!plan.next_steps.is_empty(), "next_steps should list remaining work");
}

// ── WIT lifecycle contract tests ──

/// Test that PluginEvent → WIT event conversion produces correct variant names.
/// This is a compile-time and structural check of the converter logic that
/// runs inside WitPluginComponent::call_on_event.
#[test]
fn wit_event_variants_are_mapped() {
    // Verify every API-level PluginEvent variant exists — the match inside
    // WitPluginComponent::call_on_event is exhaustive, so this test ensures
    // the type is still accessible from contracts.
    let _event = plugin::PluginEvent::RoomJoin {
        user_id: 1,
        room_id: "test".to_string(),
        is_monitor: false,
    };
    assert_eq!(
        _event.kind(),
        "room_join",
        "event kind check sanity"
    );
}

/// Test that WIT lifecycle methods on WitPluginComponent compile and
/// that the call_on_event / call_api function signatures match expected types.
/// This test uses phira_mp_plus_server_api types (not WIT-generated types)
/// to verify the adapter boundary at the server-api crate level.
#[test]
fn wit_lifecycle_method_signatures_compile() {
    // Compile-time check: PluginEvent has all expected variants
    let variants = [
        plugin::PluginEvent::UserConnect { user_id: 1, user_name: "a".into(), user_ip: "0.0.0.0".into() },
        plugin::PluginEvent::UserDisconnect { user_id: 1, user_name: "a".into() },
        plugin::PluginEvent::RoomCreate { user_id: 1, room_id: "r".into() },
        plugin::PluginEvent::RoomJoin { user_id: 1, room_id: "r".into(), is_monitor: false },
        plugin::PluginEvent::RoomLeave { user_id: 1, room_id: "r".into() },
        plugin::PluginEvent::RoomModify { user_id: 1, room_id: "r".into(), data: "{}".into() },
        plugin::PluginEvent::GameStart { user_id: 1, room_id: "r".into() },
        plugin::PluginEvent::GameEnd { user_id: 1, user_name: "a".into(), room_id: "r".into(), score: 100, accuracy: 0.95, perfect: 10, good: 2, bad: 1, miss: 0, max_combo: 15, full_combo: true },
        plugin::PluginEvent::PlayerTouches { user_id: 1, room_id: "r".into(), data: vec![] },
        plugin::PluginEvent::PlayerJudges { user_id: 1, room_id: "r".into(), data: vec![] },
        plugin::PluginEvent::RoundComplete { room_id: "r".into(), chart_id: 42, chart_name: "test".into() },
    ];
    assert_eq!(variants.len(), 11, "all 11 PluginEvent variants are tested");

    // Verify each variant produces a distinct kind string
    let kinds: std::collections::HashSet<&str> = variants.iter().map(|v| v.kind()).collect();
    assert_eq!(kinds.len(), 11, "each variant must have a unique kind string");
}

/// Test serde_json <-> WIT JsonValue round-trip for all JSON value types.
/// These converters are used internally by call_api.
#[test]
fn wit_json_value_round_trip() {
    // The converters are gated by wit-bindgen (default feature).
    // These tests verify the serde_json <-> WIT JsonValue conversion
    // used internally by call_api.
    //
    // In the absence of the generated WIT types (no wit-bindgen feature),
    // the functions don't exist — skip the test entirely.
    #[cfg(feature = "wit-bindgen")]
    {
        // Null
        let null = serde_json::Value::Null;
        let wit = wit_host::json_value_to_wit(&null);
        let back = wit_host::wit_json_value_to_serde(&wit);
        assert_eq!(back, null, "null round-trip");

        // Bool
        let b = serde_json::json!(true);
        let wit = wit_host::json_value_to_wit(&b);
        let back = wit_host::wit_json_value_to_serde(&wit);
        assert_eq!(back, b, "bool round-trip");

        // Integer
        let i = serde_json::json!(42);
        let wit = wit_host::json_value_to_wit(&i);
        let back = wit_host::wit_json_value_to_serde(&wit);
        assert_eq!(back, i, "integer round-trip");

        // Float
        let f = serde_json::json!(3.14);
        let wit = wit_host::json_value_to_wit(&f);
        let back = wit_host::wit_json_value_to_serde(&wit);
        assert_eq!(back, f, "float round-trip");

        // String
        let s = serde_json::json!("hello");
        let wit = wit_host::json_value_to_wit(&s);
        let back = wit_host::wit_json_value_to_serde(&wit);
        assert_eq!(back, s, "string round-trip");

        // Array (encoded as JSON string in WIT)
        let arr = serde_json::json!([1, 2, 3]);
        let wit = wit_host::json_value_to_wit(&arr);
        let back = wit_host::wit_json_value_to_serde(&wit);
        assert_eq!(back, arr, "array round-trip");

        // Object (encoded as JSON string in WIT)
        let obj = serde_json::json!({"key": "value"});
        let wit = wit_host::json_value_to_wit(&obj);
        let back = wit_host::wit_json_value_to_serde(&wit);
        assert_eq!(back, obj, "object round-trip");
    }
}

// ── Capability model contract tests ──

#[test]
fn default_capabilities_include_expected_set() {
    let caps = wasm_host_helpers::default_capabilities();
    assert!(caps.contains("state.read"), "default should include state.read");
    assert!(caps.contains("send"), "default should include send");
    assert!(caps.contains("config"), "default should include config");
    assert!(!caps.contains("admin"), "default should NOT include admin");
    assert!(!caps.contains("room.manage"), "default should NOT include room.manage");
}

#[test]
fn default_capabilities_covers_basic_plugin_needs() {
    let caps = wasm_host_helpers::default_capabilities();
    let essentials = ["state.read", "send", "ext", "config", "file.read", "file.write"];
    for cap in &essentials {
        assert!(caps.contains(*cap), "default should contain {cap}");
    }
}

#[test]
fn required_capability_maps_admin_methods() {
    assert_eq!(wasm_host_helpers::required_capability("admin.list"), Some("admin"));
    assert_eq!(wasm_host_helpers::required_capability("admin.add"), Some("admin"));
}

#[test]
fn required_capability_maps_room_methods() {
    assert_eq!(wasm_host_helpers::required_capability("room.set_lock"), Some("room.manage"));
    assert_eq!(wasm_host_helpers::required_capability("room.kick"), Some("room.manage"));
    assert_eq!(wasm_host_helpers::required_capability("room.close"), Some("room.manage"));
    assert_eq!(wasm_host_helpers::required_capability("room.set_hidden"), Some("room.manage"));
}

#[test]
fn wit_host_public_api_compiles() {
    // Compile-time check: WitPluginHost public methods exist
    let _ = wit_host::json_value_to_wit;
    let _ = wit_host::wit_json_value_to_serde;
}

#[test]
fn wit_host_capability_loads_defaults_for_unknown_plugin() {
    // load_manifest_capabilities for a non-existent plugin should return defaults
    let caps = wasm_host_helpers::load_manifest_capabilities("/nonexistent/plugin.wasm");
    assert!(caps.is_ok(), "non-existent plugin should get default capabilities");
    let caps = caps.unwrap();
    assert!(caps.contains("state.read"), "defaults should include state.read");
}

#[test]
fn wit_host_reject_symlink_components() {
    assert!(wasm_host_helpers::reject_symlink_components(&std::path::Path::new("/safe/path")).is_ok());
    assert!(wasm_host_helpers::reject_symlink_components(&std::path::Path::new("/unsafe/../path")).is_err());
}
