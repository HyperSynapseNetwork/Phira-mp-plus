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
///
/// Uses `std::net::IpAddr` parsing for IP-based SSRF protection. Hostnames
/// are resolved via the system DNS resolver (blocking, with 5-second timeout)
/// and each resolved address is checked against private/reserved ranges.
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
    // Extract the host portion
    let after_scheme = value
        .trim_start_matches("https://")
        .trim_start_matches("http://");
    let host = after_scheme.split('/').next().unwrap_or(after_scheme);
    let host = host.split(':').next().unwrap_or(host); // strip port
    let host = host.trim_matches(|c| c == '[' || c == ']'); // strip IPv6 brackets

    // Reject known private hostnames
    if host.eq_ignore_ascii_case("localhost") {
        return Err(format!("private network address not allowed: {host}"));
    }

    // Try parsing as IP for comprehensive SSRF checks
    if let Ok(ip) = host.parse::<std::net::IpAddr>() {
        let is_private = match ip {
            std::net::IpAddr::V4(v4) => {
                v4.is_private()
                    || v4.is_loopback()
                    || v4.is_link_local()
                    || v4.is_broadcast()
                    || v4.is_documentation()
                    || v4.is_unspecified()
                    || v4.is_multicast()
            }
            std::net::IpAddr::V6(v6) => {
                v6.is_loopback()
                    || v6.is_unspecified()
                    || v6.is_multicast()
                    || v6.is_unique_local()
                    || v6.is_unicast_link_local()
            }
        };
        if is_private {
            return Err(format!("private network address not allowed: {host}"));
        }
        return Ok(());
    }

    // Resolve hostname via DNS and check each address.
    // Uses a blocking thread with timeout to avoid hanging the caller.
    use std::net::ToSocketAddrs;
    let host_for_dns = host.to_string();
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(
            (host_for_dns.as_str(), 0u16)
                .to_socket_addrs()
                .map(|iter| iter.collect::<Vec<std::net::SocketAddr>>()),
        );
    });
    let addresses: Vec<std::net::SocketAddr> = rx
        .recv_timeout(std::time::Duration::from_secs(5))
        .map_err(|_| format!("DNS resolution timed out for {host}"))?
        .map_err(|e| format!("DNS resolution failed for {host}: {e}"))?;
    for addr in &addresses {
        let ip = addr.ip();
        let is_private = match ip {
            std::net::IpAddr::V4(v4) => {
                v4.is_private()
                    || v4.is_loopback()
                    || v4.is_link_local()
                    || v4.is_broadcast()
                    || v4.is_documentation()
                    || v4.is_unspecified()
                    || v4.is_multicast()
            }
            std::net::IpAddr::V6(v6) => {
                v6.is_loopback()
                    || v6.is_unspecified()
                    || v6.is_multicast()
                    || v6.is_unique_local()
                    || v6.is_unicast_link_local()
            }
        };
        if is_private {
            return Err(format!(
                "hostname '{host}' resolves to private network address: {ip}"
            ));
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn truncate_string_shortens_correctly() {
        assert_eq!(truncate_string("hello", 10), "hello");
        assert_eq!(truncate_string("hello world", 5), "hello");
        assert_eq!(truncate_string("你好世界", 2), "你好");
    }

    #[test]
    fn validate_display_name_rejects_empty() {
        assert!(validate_display_name("").is_err());
        assert!(validate_display_name("   ").is_err());
    }

    #[test]
    fn validate_display_name_rejects_control_chars() {
        assert!(validate_display_name("test\x00name").is_err());
    }

    #[test]
    fn validate_display_name_accepts_valid() {
        assert!(validate_display_name("My Plugin v1.0").is_ok());
    }

    #[test]
    fn validate_identifier_rejects_invalid() {
        assert!(validate_identifier("").is_err());
        assert!(validate_identifier("hello world").is_err());
    }

    #[test]
    fn validate_identifier_accepts_valid() {
        assert!(validate_identifier("my-plugin").is_ok());
        assert!(validate_identifier("abc123").is_ok());
    }

    #[test]
    fn validate_config_key_rejects_empty_or_long() {
        assert!(validate_config_key("").is_err());
        assert!(validate_config_key(&"a".repeat(257)).is_err());
    }

    #[test]
    fn validate_config_key_rejects_control_chars() {
        assert!(validate_config_key("key\x00name").is_err());
    }

    #[test]
    fn validate_config_key_accepts_valid() {
        assert!(validate_config_key("api.timeout").is_ok());
    }

    #[test]
    fn reject_symlink_components_works() {
        assert!(reject_symlink_components(Path::new("/safe/path")).is_ok());
        assert!(reject_symlink_components(Path::new("/unsafe/../path")).is_err());
    }

    #[test]
    fn config_path_returns_expected() {
        let p = config_path("test-plugin");
        assert!(p.ends_with("config.json"));
        assert!(p.to_string_lossy().contains("test-plugin"));
    }

    #[test]
    fn validate_http_url_rejects_long_url() {
        let long = "http://".to_string() + &"a".repeat(8192);
        assert!(validate_http_url(&long, false).is_err());
    }

    #[test]
    fn validate_http_url_rejects_non_http() {
        assert!(validate_http_url("ftp://example.com", false).is_err());
    }

    #[test]
    fn validate_http_url_allow_private_skips_all_checks() {
        assert!(validate_http_url("http://localhost", true).is_ok());
        assert!(validate_http_url("http://192.168.1.1", true).is_ok());
    }

    #[test]
    fn validate_http_url_rejects_private_ip() {
        assert!(validate_http_url("http://127.0.0.1", false).is_err());
        assert!(validate_http_url("http://10.0.0.1", false).is_err());
        assert!(validate_http_url("http://192.168.1.1", false).is_err());
    }

    #[test]
    fn validate_http_url_rejects_localhost() {
        assert!(validate_http_url("http://localhost", false).is_err());
    }

    #[test]
    fn validate_http_url_rejects_documentation_ip() {
        assert!(validate_http_url("http://192.0.2.1", false).is_err());
    }

    #[test]
    fn validate_http_url_accepts_public_ip() {
        assert!(validate_http_url("http://8.8.8.8", false).is_ok());
        assert!(validate_http_url("http://1.1.1.1", false).is_ok());
    }

    #[test]
    fn validate_http_url_accepts_public_hostname() {
        assert!(validate_http_url("http://example.com", false).is_ok());
        assert!(validate_http_url("https://github.com", false).is_ok());
    }

    #[test]
    fn validate_http_url_with_port_strips_correctly() {
        assert!(validate_http_url("http://127.0.0.1:8080/path", false).is_err());
        assert!(validate_http_url("http://example.com:8080/path", false).is_ok());
    }

    #[test]
    fn validate_http_url_with_ipv6() {
        assert!(validate_http_url("http://[::1]/path", false).is_err());
    }

    #[test]
    fn required_capability_maps_correctly() {
        assert_eq!(required_capability("uuid.v4"), None);
        assert_eq!(required_capability("admin.list"), Some("admin"));
        assert_eq!(required_capability("room.set_lock"), Some("room.manage"));
    }

    #[test]
    fn atomic_write_creates_file() {
        let dir = std::env::temp_dir().join(format!("atomic_test_{}", std::process::id()));
        let path = dir.join("test.txt");
        assert!(atomic_write(&path, b"hello world").is_ok());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "hello world");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn atomic_write_rejects_symlink_traversal() {
        assert!(atomic_write(Path::new("/../tmp/evil.txt"), b"data").is_err());
    }
}

