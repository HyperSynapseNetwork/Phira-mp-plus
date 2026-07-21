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
    if value.trim().is_empty() || value.chars().count() > 128 || value.chars().any(char::is_control)
    {
        return Err("invalid plugin display name".to_string());
    }
    Ok(())
}

/// Validate a plugin/method identifier (ASCII alphanumeric + _ - .).
pub fn validate_identifier(value: &str) -> Result<(), String> {
    if value.is_empty()
        || value.len() > 96
        || !value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-' | b'.'))
    {
        return Err(format!("invalid identifier '{value}'"));
    }
    Ok(())
}

/// Validate a config key path.
pub fn validate_config_key(value: &str) -> Result<(), String> {
    if value.is_empty()
        || value.len() > 256
        || value.chars().any(char::is_control)
        || value.split('.').any(|part| {
            part.is_empty()
                || !part
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
        })
    {
        return Err("invalid config key".to_string());
    }
    Ok(())
}

/// Default set of capabilities for plugins without a manifest.
pub fn default_capabilities() -> HashSet<String> {
    [
        "state.read",
        "send",
        "ext",
        "config",
        "file.read",
        "file.write",
        "plugin.call",
        "plugin.register",
    ]
    .into_iter()
    .map(str::to_string)
    .collect()
}

/// Load the capability grant for a plugin.
///
/// A sidecar named `<plugin>.capabilities.json` may contain either a JSON array
/// or `{ "capabilities": [...] }`. Missing sidecars receive only the default,
/// non-privileged set. Unknown capability names are rejected rather than
/// silently granted.
pub fn load_manifest_capabilities(plugin_path: &str) -> Result<HashSet<String>, String> {
    let path = Path::new(plugin_path);
    let sidecar = path.with_extension("capabilities.json");
    if !sidecar.exists() {
        return Ok(default_capabilities());
    }

    let raw = std::fs::read_to_string(&sidecar)
        .map_err(|e| format!("read capability manifest '{}': {e}", sidecar.display()))?;
    let value: serde_json::Value = serde_json::from_str(&raw)
        .map_err(|e| format!("parse capability manifest '{}': {e}", sidecar.display()))?;
    let entries = match value {
        serde_json::Value::Array(entries) => entries,
        serde_json::Value::Object(mut object) => object
            .remove("capabilities")
            .and_then(|value| value.as_array().cloned())
            .ok_or_else(|| "capability manifest must contain a 'capabilities' array".to_string())?,
        _ => return Err("capability manifest must be an array or object".to_string()),
    };

    let allowed: HashSet<&'static str> = [
        "state.read",
        "send",
        "ext",
        "config",
        "file.read",
        "file.write",
        "plugin.call",
        "plugin.register",
        "http",
        "room.manage",
        "admin",
        "simulation",
    ]
    .into_iter()
    .collect();
    let mut capabilities = HashSet::new();
    for entry in entries {
        let capability = entry
            .as_str()
            .ok_or_else(|| "capability entries must be strings".to_string())?;
        validate_identifier(capability)?;
        if !allowed.contains(capability) {
            return Err(format!("unknown capability '{capability}'"));
        }
        capabilities.insert(capability.to_string());
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
        value
            if value.starts_with("admin.") || value.starts_with("ban.") || value == "user.kick" =>
        {
            Some("admin")
        }
        "room.create_empty"
        | "room.kick"
        | "room.set_host"
        | "room.clear_host"
        | "room.set_lock"
        | "room.force_move"
        | "room.set_hidden"
        | "room.set_persistent_empty"
        | "room.set_phira_api_endpoint"
        | "room.clear_phira_api_endpoint"
        | "room.close" => Some("room.manage"),
        value
            if value.starts_with("room.")
                || value.starts_with("player.")
                || value.starts_with("round.")
                || value.starts_with("user.")
                || value.starts_with("persist.")
                || value.starts_with("runtime.")
                || value.starts_with("benchmark.")
                || value == "state.query"
                || value == "rooms.list"
                || value == "rooms.by_name"
                || value == "rooms.by_user"
                || value == "rooms.history"
                || value == "auth.visited_count"
                || value == "user_name"
                || value == "users.list"
                || value == "user.is_online"
                || value == "playtime.leaderboard" =>
        {
            Some("state.read")
        }
        value if value.starts_with("send.") || value == "send_room_chat" => Some("send"),
        value if value.starts_with("ext.") => Some("ext"),
        value if value.starts_with("config.") => Some("config"),
        value if value.starts_with("http.") || value.starts_with("sse.") => Some("http"),
        value if value.starts_with("simulation.") => Some("simulation"),
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

/// Return whether an address must never be reached by an unprivileged plugin.
fn is_disallowed_plugin_ip(ip: std::net::IpAddr) -> bool {
    match ip {
        std::net::IpAddr::V4(v4) => {
            let [a, b, c, d] = v4.octets();
            let reserved = a == 0
                || a == 10
                || a == 127
                || (a == 100 && (64..=127).contains(&b))
                || (a == 169 && b == 254)
                || (a == 172 && (16..=31).contains(&b))
                || (a == 192 && b == 0 && c == 0)
                || (a == 192 && b == 0 && c == 2)
                || (a == 192 && b == 88 && c == 99)
                || (a == 192 && b == 168)
                || (a == 198 && (b == 18 || b == 19))
                || (a == 198 && b == 51 && c == 100)
                || (a == 203 && b == 0 && c == 113)
                || a >= 224
                || (a == 255 && b == 255 && c == 255 && d == 255);
            reserved
                || v4.is_private()
                || v4.is_loopback()
                || v4.is_link_local()
                || v4.is_broadcast()
                || v4.is_documentation()
                || v4.is_unspecified()
                || v4.is_multicast()
        }
        std::net::IpAddr::V6(v6) => {
            if let Some(v4) = v6.to_ipv4() {
                return is_disallowed_plugin_ip(std::net::IpAddr::V4(v4));
            }
            let segments = v6.segments();
            let documentation = segments[0] == 0x2001 && segments[1] == 0x0db8;
            v6.is_loopback()
                || v6.is_unspecified()
                || v6.is_multicast()
                || v6.is_unique_local()
                || v6.is_unicast_link_local()
                || documentation
        }
    }
}

/// Validate an HTTP(S) URL for plugin HTTP requests.
///
/// Parsing is delegated to `reqwest::Url`, which correctly handles credentials,
/// ports and bracketed IPv6 literals. Hostnames are resolved before dispatch and
/// every returned address is checked. Redirects are disabled by the caller, so a
/// validated public URL cannot redirect into a private network. This validation
/// reduces SSRF exposure but does not provide a DNS pin; fully untrusted plugins
/// still require process/network namespace isolation.
pub fn validate_http_url(value: &str, allow_private: bool) -> Result<(), String> {
    if value.len() > 8192 {
        return Err("HTTP URL too long".to_string());
    }

    let parsed =
        reqwest::Url::parse(value).map_err(|error| format!("invalid HTTP URL: {error}"))?;
    if !matches!(parsed.scheme(), "http" | "https") {
        return Err("only http/https URLs are allowed".to_string());
    }
    if parsed.username() != "" || parsed.password().is_some() {
        return Err("credentials in plugin HTTP URLs are not allowed".to_string());
    }
    let host = parsed
        .host_str()
        .ok_or_else(|| "HTTP URL must contain a host".to_string())?;
    if allow_private {
        return Ok(());
    }
    if host.eq_ignore_ascii_case("localhost") || host.ends_with(".localhost") {
        return Err(format!("private network address not allowed: {host}"));
    }

    if let Ok(ip) = host.parse::<std::net::IpAddr>() {
        return if is_disallowed_plugin_ip(ip) {
            Err(format!(
                "private or reserved network address not allowed: {host}"
            ))
        } else {
            Ok(())
        };
    }

    use std::net::ToSocketAddrs;
    let host_for_dns = host.to_string();
    let port = parsed
        .port_or_known_default()
        .ok_or_else(|| "HTTP URL has no usable port".to_string())?;
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let result = (host_for_dns.as_str(), port)
            .to_socket_addrs()
            .map(|iter| iter.collect::<Vec<std::net::SocketAddr>>());
        let _ = tx.send(result);
    });
    let addresses = rx
        .recv_timeout(std::time::Duration::from_secs(5))
        .map_err(|_| format!("DNS resolution timed out for {host}"))?
        .map_err(|error| format!("DNS resolution failed for {host}: {error}"))?;
    if addresses.is_empty() {
        return Err(format!("DNS resolution returned no address for {host}"));
    }
    if let Some(address) = addresses
        .iter()
        .find(|address| is_disallowed_plugin_ip(address.ip()))
    {
        return Err(format!(
            "hostname '{host}' resolves to private or reserved address: {}",
            address.ip()
        ));
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
    #[test]
    fn reject_traversal_with_dotdot() {
        assert!(reject_symlink_components(Path::new("/safe/../etc/passwd")).is_err());
        assert!(reject_symlink_components(Path::new("/safe/../../etc")).is_err());
        assert!(reject_symlink_components(Path::new("/safe/..")).is_err());
        assert!(reject_symlink_components(Path::new("/safe/./../etc")).is_err());
    }

    #[test]
    fn reject_absolute_in_relative_chain() {
        assert!(reject_symlink_components(Path::new("/safe/../../../tmp/evil")).is_err());
    }

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
    fn validate_http_url_rejects_cgnat_and_benchmark_ranges() {
        assert!(validate_http_url("http://100.64.0.1", false).is_err());
        assert!(validate_http_url("http://198.18.0.1", false).is_err());
    }

    #[test]
    fn validate_http_url_accepts_public_ip() {
        assert!(validate_http_url("http://8.8.8.8", false).is_ok());
        assert!(validate_http_url("http://1.1.1.1", false).is_ok());
    }

    #[test]
    fn validate_http_url_rejects_embedded_credentials() {
        assert!(validate_http_url("http://user:pass@8.8.8.8", false).is_err());
    }

    #[test]
    fn validate_http_url_with_port_strips_correctly() {
        assert!(validate_http_url("http://127.0.0.1:8080/path", false).is_err());
        assert!(validate_http_url("http://8.8.8.8:8080/path", false).is_ok());
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
        assert_eq!(required_capability("simulation.start"), Some("simulation"));
        assert_eq!(required_capability("ban.check"), Some("admin"));
        assert_eq!(required_capability("runtime.status"), Some("state.read"));
        assert_eq!(required_capability("not.a.real.method"), Some("unknown"));
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
