//! Server configuration types.
//!
//! Extracted from the original `server.rs` — see the parent module for
//! the re-exported type list.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use tokio::sync::RwLock;
use uuid::Uuid;

/// Chart information from the Phira API
#[derive(Debug, Deserialize, Clone)]
pub struct Chart {
    pub id: i32,
    pub name: String,
}

/// Record information from the Phira API
#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct Record {
    pub id: i32,
    pub player: i32,
    pub score: i32,
    pub perfect: i32,
    pub good: i32,
    pub bad: i32,
    pub miss: i32,
    pub max_combo: i32,
    pub accuracy: f32,
    pub full_combo: bool,
    pub std: f32,
    pub std_score: f32,
}

/// Convenience alias for a tokio RwLock-wrapped HashMap.
pub type SafeMap<K, V> = RwLock<HashMap<K, V>>;

/// Convenience alias for a UUID-keyed SafeMap.
pub type IdMap<V> = SafeMap<Uuid, V>;

pub(crate) fn normalize_phira_api_endpoint(value: &str) -> Result<String, String> {
    let endpoint = value.trim().trim_end_matches('/').to_string();
    if endpoint.is_empty() {
        return Err("phira_api_endpoint cannot be empty".to_string());
    }
    let url =
        reqwest::Url::parse(&endpoint).map_err(|e| format!("invalid phira_api_endpoint: {e}"))?;
    match url.scheme() {
        "http" | "https" => Ok(endpoint),
        other => Err(format!("unsupported phira_api_endpoint scheme: {other}")),
    }
}

pub(crate) fn parse_room_endpoint_value(value: &str) -> Result<Option<String>, String> {
    let raw = value.trim();
    if raw.is_empty()
        || raw.eq_ignore_ascii_case("default")
        || raw.eq_ignore_ascii_case("global")
        || raw.eq_ignore_ascii_case("none")
        || raw.eq_ignore_ascii_case("null")
        || raw.eq_ignore_ascii_case("clear")
        || raw == "全局"
        || raw == "默认"
        || raw == "清除"
    {
        Ok(None)
    } else {
        normalize_phira_api_endpoint(raw).map(Some)
    }
}
