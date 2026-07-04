//! Helper utilities extracted from wasm_host.rs to reduce file size.
//!
//! These functions are used by the WASM plugin host for capability
//! checking, path validation, config management, and state query dispatch.

use std::collections::HashSet;
use std::path::Path;

/// Truncate a string to at most `max` characters.
pub fn truncate_string(value: &str, max: usize) -> String {
    value.chars().take(max).collect()
}

/// Validate a plugin display name.
pub fn validate_display_name(value: &str) -> Result<(), String> {
    if value.trim().is_empty() || value.chars().count() > 128 || value.chars().any(char::is_control) {
        return Err("invalid plugin display name".to_string());
    }
    Ok(())
}

/// Validate a plugin/method identifier (ASCII alphanumeric + _ - .).
pub fn validate_identifier(value: &str) -> Result<(), String> {
    if value.is_empty()
        || value.len() > 96
        || !value.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-' | b'.'))
    {
        return Err(format!("invalid identifier '{value}'"));
    }
    Ok(())
}

/// Validate a config key path.
pub fn validate_config_key(value: &str) -> Result<(), String> {
    if value.is_empty() || value.len() > 256 || value.chars().any(char::is_control) {
        return Err("invalid config key".to_string());
    }
    Ok(())
}

/// Default set of capabilities for plugins without a manifest.
pub fn default_capabilities() -> HashSet<String> {
    [
        "state.read", "send", "ext", "config",
        "file.read", "file.write", "plugin.call", "plugin.register",
    ]
    .into_iter()
    .map(str::to_string)
    .collect()
}

/// Load capabilities from a plugin manifest JSON file.
pub fn load_manifest_capabilities(plugin_path: &str) -> Result<HashSet<String>, String> {
    let manifest = Path::new(plugin_path).with_extension("json");
    if !manifest.exists() {
        return Ok(default_capabilities());
    }
    let bytes = std::fs::read(&manifest)
        .map_err(|e| format!("read manifest '{}': {e}", manifest.display()))?;
    if bytes.len() > 64 * 1024 {
        return Err("plugin manifest is too large".to_string());
    }
    let value: serde_json::Value =
        serde_json::from_slice(&bytes).map_err(|e| format!("invalid plugin manifest: {e}"))?;
    let array = value
        .get("capabilities")
        .and_then(|v| v.as_array())
        .ok_or("manifest must contain a capabilities array")?;
    let mut capabilities = HashSet::new();
    for item in array {
        if let Some(cap) = item.as_str() {
            capabilities.insert(cap.to_string());
        }
    }
    if capabilities.is_empty() {
        return Err("plugin manifest capabilities array is empty".to_string());
    }
    Ok(capabilities)
}

/// Reject path components that look like symlink traversal (..).
pub fn reject_symlink_components(path: &Path) -> Result<(), String> {
    for component in path.components() {
        if let std::path::Component::ParentDir = component {
            return Err("path must not contain '..'".to_string());
        }
    }
    Ok(())
}

/// Atomic file write: write to a temp file, then rename.
pub fn atomic_write(path: &Path, bytes: &[u8]) -> Result<(), String> {
    let parent = path.parent().ok_or("path has no parent")?;
    std::fs::create_dir_all(parent).map_err(|e| format!("create directory: {e}"))?;
    reject_symlink_components(parent)?;
    let temp = parent.join(format!(
        ".{}.{}.tmp",
        path.file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("unknown"),
        std::process::id()
    ));
    std::fs::write(&temp, bytes).map_err(|e| format!("write temp: {e}"))?;
    std::fs::rename(&temp, path).map_err(|e| format!("rename: {e}"))?;
    Ok(())
}

/// Map a method name to its required capability.
pub fn required_capability(method: &str) -> Option<&'static str> {
    match method {
        "uuid.v4" | "time.now" => None,
        value if value.starts_with("admin.") => Some("admin"),
        "room.create_empty" | "room.kick" | "room.set_host" | "room.clear_host"
        | "room.set_lock" | "room.force_move" | "room.set_hidden" | "room.set_persistent_empty"
        | "room.set_phira_api_endpoint" | "room.clear_phira_api_endpoint" | "room.close" => Some("room.manage"),
        value if value.starts_with("room.") || value.starts_with("player.")
            || value.starts_with("round.") || value.starts_with("user.")
            || value.starts_with("persist.") || value == "state.query" => Some("state.read"),
        value if value.starts_with("send.") => Some("send"),
        value if value.starts_with("ext.") => Some("ext"),
        value if value.starts_with("config.") => Some("config"),
        value if value.starts_with("http.") => Some("http"),
        "file.read" => Some("file.read"),
        "file.write" => Some("file.write"),
        "plugin.api_call" => Some("plugin.call"),
        "plugin.api_register" => Some("plugin.register"),
        _ => Some("unknown"),
    }
}

/// Config file path for a plugin.
pub fn config_path(plugin: &str) -> std::path::PathBuf {
    Path::new("data/plugins").join(plugin).join("config.json")
}

/// Validate an HTTP(S) URL for plugin HTTP requests.
pub fn validate_http_url(value: &str, allow_private: bool) -> Result<(), String> {
    if value.len() > 8192 {
        return Err("HTTP URL too long".to_string());
    }
    if !value.starts_with("http://") && !value.starts_with("https://") {
        return Err("only http/https URLs are allowed".to_string());
    }
    if allow_private {
        return Ok(());
    }
    // Simple SSRF check: reject private IP patterns
    let after_scheme = value
        .trim_start_matches("https://")
        .trim_start_matches("http://");
    let host = after_scheme.split('/').next().unwrap_or(after_scheme);
    let host = host.split(':').next().unwrap_or(host); // strip port
    // Reject private/loopback/unspecified addresses
    let private_patterns = ["127.", "10.", "192.168.", "172.16.", "169.254.", "0.0.0.0", "localhost", "[::1]", "[::]", "[0:0:0:0:0:0:0:1]"];
    for pattern in &private_patterns {
        if host.starts_with(pattern) {
            return Err(format!("private network address not allowed: {host}"));
        }
    }
    Ok(())
}

/// Map a method name to its required capability.
