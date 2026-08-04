//! WIT/component-model host trait implementations.
//!
//! Decoupled from PlusServerState — the host depends only on
//! WitHostContext (an explicit bundle of the subsystems it needs)
//! and the generic ServerStateQuery from the api crate.
//!
//! The core `WitPluginHost` skeleton is available with `plugin-system`.
//! The generated trait impls require `wit-bindgen` (default feature).

#![allow(clippy::type_complexity)]

pub mod host;

// Re-export all public items from host (WitHostContext, RegisteredHandler,
// WitPluginHost, json_value_to_wit, wit_json_value_to_serde).
pub use host::*;

// Re-export items needed by the wit_trait_impls inline module.
#[cfg(feature = "wit-bindgen")]
pub(crate) use host::normalize_plugin_scoped_api_args;

// Room state query helpers depend on wit_abi types, so they are only
// compiled when wit-bindgen is enabled.
#[cfg(feature = "wit-bindgen")]
pub mod query;

#[cfg(feature = "wit-bindgen")]
pub(crate) use query::{build_room_players, extract_current_round, extract_snapshot_data};

// ══════════════════════════════════════════════════════════════════════
// Generated trait implementations — only with wit-bindgen feature.
// ══════════════════════════════════════════════════════════════════════

#[cfg(feature = "wit-bindgen")]
mod wit_trait_impls {
    use super::{
        build_room_players, extract_current_round, extract_snapshot_data,
        normalize_plugin_scoped_api_args, WitPluginHost,
    };
    use crate::plugin_abi::wit_abi as wit;
    use phira_mp_plus_server_api as api;
    use std::sync::Arc;
    use std::time::Duration;
    use wit::phira::plugin::phira_types as types;

    /// Helper: call ServerStateQuery and convert to ApiResult.
    fn query_api_result(
        host: &WitPluginHost,
        method: &str,
        args: &[serde_json::Value],
    ) -> types::ApiResult {
        match host.ctx.state_query.call(method, args) {
            Ok(value) => types::ApiResult::Ok(json_to_wit_json(&value)),
            Err(e) => types::ApiResult::Error(e),
        }
    }

    // ── phira-types (data-only, no methods) ──
    impl types::Host for WitPluginHost {}

    // ── phira-events (data-only, no methods) ──
    impl wit::phira::plugin::phira_events::Host for WitPluginHost {}

    // ── phira-host ──
    impl wit::phira::plugin::phira_host::Host for WitPluginHost {
        fn log(&mut self, level: String, message: String) {
            match level.as_str() {
                "error" => tracing::error!("[plugin:{}] {message}", self.plugin_name),
                "warn" => tracing::warn!("[plugin:{}] {message}", self.plugin_name),
                "info" => tracing::info!("[plugin:{}] {message}", self.plugin_name),
                "debug" => tracing::debug!("[plugin:{}] {message}", self.plugin_name),
                "trace" => tracing::trace!("[plugin:{}] {message}", self.plugin_name),
                _ => tracing::info!("[plugin:{}] {message}", self.plugin_name),
            }
        }

        fn generate_uuid(&mut self) -> String {
            uuid::Uuid::new_v4().to_string()
        }

        fn current_time_ms(&mut self) -> u64 {
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_millis() as u64)
                .unwrap_or(0)
        }

        fn api_call(&mut self, method: String, args: Vec<types::JsonValue>) -> types::ApiResult {
            tracing::trace!(plugin = %self.plugin_name, %method, "api_call");
            let args_serde: Vec<serde_json::Value> = args.iter().map(super::host::wit_json_value_to_serde).collect();
            let args_serde =
                match normalize_plugin_scoped_api_args(&method, &self.plugin_name, args_serde) {
                    Ok(args) => args,
                    Err(error) => return types::ApiResult::Error(error),
                };
            match self.ctx.state_query.call(&method, &args_serde) {
                Ok(value) => types::ApiResult::Ok(json_to_wit_json(&value)),
                Err(e) => types::ApiResult::Error(e),
            }
        }

        fn send_chat(&mut self, user_id: u32, message: String) {
            if let Err(error) = self.require_capability("send") {
                tracing::warn!(plugin = %self.plugin_name, %error, "plugin send_chat denied");
                return;
            }
            if let Some(send_chat) = &self.ctx.send_chat {
                send_chat(user_id as i32, message);
            } else {
                tracing::warn!(plugin = %self.plugin_name, "plugin send_chat unavailable");
            }
        }

        fn http_request(
            &mut self,
            url: String,
            method: String,
            headers: Vec<(String, String)>,
            body: Vec<u8>,
        ) -> Result<types::HttpResponse, String> {
            self.require_capability("http")?;
            crate::wasm_host_helpers::validate_http_url(&url, self.ctx.http_allow_private_network)?;

            let timeout_secs = self.ctx.http_timeout_secs.max(5);
            let max_body = self.ctx.http_max_body.max(1);

            let client = reqwest::blocking::Client::builder()
                .timeout(std::time::Duration::from_secs(timeout_secs))
                .redirect(reqwest::redirect::Policy::none())
                .build()
                .map_err(|e| format!("create HTTP client: {e}"))?;

            let req = match method.to_uppercase().as_str() {
                "GET" => client.get(&url),
                "POST" => client.post(&url),
                "PUT" => client.put(&url),
                "DELETE" => client.delete(&url),
                "PATCH" => client.patch(&url),
                "HEAD" => client.head(&url),
                other => return Err(format!("unsupported HTTP method: {other}")),
            };

            let req = headers.into_iter().fold(req, |r, (k, v)| r.header(&k, &v));
            let req = if !body.is_empty() {
                req.body(body)
            } else {
                req
            };

            let response = req
                .send()
                .map_err(|e| format!("HTTP request failed: {e}"))?;

            let status = response.status().as_u16();
            let resp_headers: Vec<(String, String)> = response
                .headers()
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_str().unwrap_or("").to_string()))
                .collect();

            if response
                .content_length()
                .is_some_and(|length| length > max_body as u64)
            {
                return Err(format!(
                    "HTTP response exceeds configured limit of {max_body} bytes"
                ));
            }
            let mut limited = std::io::Read::take(response, (max_body as u64).saturating_add(1));
            let mut resp_body = Vec::with_capacity(max_body.min(64 * 1024));
            std::io::Read::read_to_end(&mut limited, &mut resp_body)
                .map_err(|e| format!("read response body: {e}"))?;
            if resp_body.len() > max_body {
                return Err(format!(
                    "HTTP response exceeds configured limit of {max_body} bytes"
                ));
            }

            Ok(types::HttpResponse {
                status,
                headers: resp_headers,
                body: resp_body,
            })
        }
    }

    // ── phira-query ──
    impl wit::phira::plugin::phira_query::Host for WitPluginHost {
        fn get_user(&mut self, user_id: u32) -> types::ApiResult {
            query_api_result(self, "user_name", &[serde_json::json!(user_id as i32)])
        }
        fn get_user_extra(&mut self, user_id: u32, key: String) -> types::ApiResult {
            if let Err(error) = self.require_api_capability("ext") {
                return error;
            }
            match self.block_on_async(move |ctx| async move {
                ctx.extensions.get_user_extra(user_id as i32, &key).await
            }) {
                Ok(Some(value)) => types::ApiResult::Ok(types::JsonValue::Text(value)),
                Ok(None) => types::ApiResult::Ok(types::JsonValue::Null),
                Err(e) => types::ApiResult::Error(e),
            }
        }
        fn set_user_extra(&mut self, user_id: u32, key: String, value: String) -> types::ApiResult {
            if let Err(error) = self.require_api_capability("ext") {
                return error;
            }
            match self.block_on_async(move |ctx| async move {
                ctx.extensions.set_user_extra(user_id as i32, &key, value).await
            }) {
                Ok(Ok(())) => types::ApiResult::Ok(types::JsonValue::Null),
                Ok(Err(e)) | Err(e) => types::ApiResult::Error(e),
            }
        }
        fn get_room(&mut self, room_id: String) -> types::ApiResult {
            query_api_result(self, "rooms.by_name", &[serde_json::json!(room_id)])
        }
        fn get_room_extra(&mut self, room_id: String, key: String) -> types::ApiResult {
            if let Err(error) = self.require_api_capability("ext") {
                return error;
            }
            match self.block_on_async(move |ctx| async move {
                ctx.extensions.get_room_extra(&room_id, &key).await
            }) {
                Ok(Some(value)) => types::ApiResult::Ok(types::JsonValue::Text(value)),
                Ok(None) => types::ApiResult::Ok(types::JsonValue::Null),
                Err(e) => types::ApiResult::Error(e),
            }
        }
        fn list_rooms(&mut self) -> types::ApiResult {
            query_api_result(self, "rooms.list", &[])
        }
        fn list_online_users(&mut self) -> types::ApiResult {
            query_api_result(self, "users.list", &[])
        }
        fn is_user_online(&mut self, user_id: u32) -> bool {
            matches!(
                query_api_result(self, "user.is_online", &[serde_json::json!(user_id as i32)]),
                types::ApiResult::Ok(types::JsonValue::Flag(true))
            )
        }
    }

    // ── phira-room-mgmt ──
    impl wit::phira::plugin::phira_room_mgmt::Host for WitPluginHost {
        fn create_empty_room(
            &mut self,
            room_id: String,
            endpoint: Option<String>,
        ) -> types::ApiResult {
            let mut args = vec![serde_json::json!(room_id)];
            if let Some(ep) = endpoint {
                args.push(serde_json::json!(ep));
            }
            query_api_result(self, "room.create_empty", &args)
        }
        fn kick_from_room(&mut self, room_id: String, target_id: u32) -> types::ApiResult {
            query_api_result(
                self,
                "room.kick",
                &[serde_json::json!(room_id), serde_json::json!(target_id)],
            )
        }
        fn transfer_host(&mut self, room_id: String, target_id: u32) -> types::ApiResult {
            query_api_result(
                self,
                "room.set_host",
                &[serde_json::json!(room_id), serde_json::json!(target_id)],
            )
        }
        fn set_host(&mut self, room_id: String, target_id: Option<u32>) -> types::ApiResult {
            query_api_result(
                self,
                "room.set_host",
                &[
                    serde_json::json!(room_id),
                    serde_json::json!(target_id.map(|id| id as i32)),
                ],
            )
        }
        fn set_room_lock(&mut self, room_id: String, locked: bool) -> types::ApiResult {
            query_api_result(
                self,
                "room.set_lock",
                &[serde_json::json!(room_id), serde_json::json!(locked)],
            )
        }
        fn set_room_hidden(&mut self, room_id: String, hidden: bool) -> types::ApiResult {
            query_api_result(
                self,
                "room.set_hidden",
                &[serde_json::json!(room_id), serde_json::json!(hidden)],
            )
        }
        fn close_room(&mut self, room_id: String) -> types::ApiResult {
            query_api_result(self, "room.close", &[serde_json::json!(room_id)])
        }
        fn set_room_phira_api_endpoint(
            &mut self,
            room_id: String,
            endpoint: Option<String>,
        ) -> types::ApiResult {
            let method = if endpoint.is_some() {
                "room.set_phira_api_endpoint"
            } else {
                "room.clear_phira_api_endpoint"
            };
            let mut args = vec![serde_json::json!(room_id)];
            if let Some(endpoint) = endpoint {
                args.push(serde_json::json!(endpoint));
            }
            query_api_result(self, method, &args)
        }
        fn add_remote_player(
            &mut self,
            room_id: String,
            player_id: u32,
            player_name: String,
        ) -> types::ApiResult {
            query_api_result(
                self,
                "room.add_remote_player",
                &[
                    serde_json::json!(room_id),
                    serde_json::json!(player_id as i32),
                    serde_json::json!(player_name),
                ],
            )
        }
        fn remote_ready(&mut self, room_id: String, player_id: u32) -> types::ApiResult {
            query_api_result(
                self,
                "room.remote_ready",
                &[serde_json::json!(room_id), serde_json::json!(player_id as i32)],
            )
        }
        fn remote_abort(&mut self, room_id: String, player_id: u32) -> types::ApiResult {
            query_api_result(
                self,
                "room.remote_abort",
                &[serde_json::json!(room_id), serde_json::json!(player_id as i32)],
            )
        }
        fn remote_leave(&mut self, room_id: String, player_id: u32) -> types::ApiResult {
            query_api_result(
                self,
                "room.remote_leave",
                &[serde_json::json!(room_id), serde_json::json!(player_id as i32)],
            )
        }
    }

    // ── phira-user-mgmt ──
    impl wit::phira::plugin::phira_user_mgmt::Host for WitPluginHost {
        fn kick_user(&mut self, user_id: u32, reason: String) -> types::ApiResult {
            query_api_result(
                self,
                "user.kick",
                &[serde_json::json!(user_id), serde_json::json!(reason)],
            )
        }
        fn ban_user(&mut self, user_id: u32, reason: String) -> types::ApiResult {
            query_api_result(
                self,
                "ban.add",
                &[serde_json::json!(user_id), serde_json::json!(reason)],
            )
        }
        fn unban_user(&mut self, user_id: u32) -> types::ApiResult {
            query_api_result(self, "ban.remove", &[serde_json::json!(user_id)])
        }
        fn get_ban_list(&mut self) -> types::ApiResult {
            query_api_result(self, "ban.list", &[])
        }
        fn is_banned(&mut self, user_id: u32) -> bool {
            matches!(
                query_api_result(self, "ban.check", &[serde_json::json!(user_id),]),
                types::ApiResult::Ok(types::JsonValue::Flag(true))
            )
        }
    }

    // ── phira-messaging ──
    impl wit::phira::plugin::phira_messaging::Host for WitPluginHost {
        fn send_to_user(&mut self, user_id: u32, message: String) -> types::ApiResult {
            if let Err(error) = self.require_api_capability("send") {
                return error;
            }
            match &self.ctx.send_chat {
                Some(send_chat) => {
                    send_chat(user_id as i32, message);
                    types::ApiResult::Ok(types::JsonValue::Null)
                }
                None => types::ApiResult::Error("host messaging is unavailable".to_string()),
            }
        }
        fn send_to_room(&mut self, room_id: String, message: String) -> types::ApiResult {
            query_api_result(
                self,
                "send_room_chat",
                &[serde_json::json!(room_id), serde_json::json!(message)],
            )
        }
        fn send_to_all(&mut self, message: String) -> types::ApiResult {
            if let Err(error) = self.require_api_capability("send") {
                return error;
            }
            match &self.ctx.send_chat {
                Some(send_chat) => {
                    send_chat(0, message);
                    types::ApiResult::Ok(types::JsonValue::Null)
                }
                None => types::ApiResult::Error("host messaging is unavailable".to_string()),
            }
        }
    }

    // ── phira-persistence ──
    impl wit::phira::plugin::phira_persistence::Host for WitPluginHost {
        fn query_events(
            &mut self,
            since: u64,
            limit: u32,
            kind: Option<String>,
            room: Option<String>,
            user: Option<u32>,
        ) -> types::ApiResult {
            query_api_result(
                self,
                "persist.events",
                &[
                    serde_json::json!(since),
                    serde_json::json!(limit),
                    serde_json::json!(kind),
                    serde_json::json!(room),
                    serde_json::json!(user),
                ],
            )
        }
        fn query_room_snapshots(&mut self, since: u64, limit: u32) -> types::ApiResult {
            query_api_result(
                self,
                "persist.rooms",
                &[serde_json::json!(since), serde_json::json!(limit)],
            )
        }
        fn query_touches(
            &mut self,
            since: u64,
            limit: u32,
            round: Option<String>,
            player: Option<u32>,
        ) -> types::ApiResult {
            query_api_result(
                self,
                "persist.touches",
                &[
                    serde_json::json!(since),
                    serde_json::json!(limit),
                    serde_json::json!(round),
                    serde_json::json!(player),
                ],
            )
        }
        fn query_judges(
            &mut self,
            since: u64,
            limit: u32,
            round: Option<String>,
            player: Option<u32>,
        ) -> types::ApiResult {
            query_api_result(
                self,
                "persist.judges",
                &[
                    serde_json::json!(since),
                    serde_json::json!(limit),
                    serde_json::json!(round),
                    serde_json::json!(player),
                ],
            )
        }
        fn get_playtime(&mut self, user_id: u32) -> types::ApiResult {
            query_api_result(self, "persist.playtime", &[serde_json::json!(user_id)])
        }
        fn top_playtime(&mut self, limit: u32) -> types::ApiResult {
            query_api_result(self, "persist.top_playtime", &[serde_json::json!(limit)])
        }
    }

    // ── phira-admin ──
    impl wit::phira::plugin::phira_admin::Host for WitPluginHost {
        fn list_admin_ids(&mut self) -> types::ApiResult {
            query_api_result(self, "admin.list", &[])
        }
        fn is_admin(&mut self, user_id: u32) -> bool {
            matches!(
                query_api_result(self, "admin.check", &[serde_json::json!(user_id),]),
                types::ApiResult::Ok(types::JsonValue::Flag(true))
            )
        }
        fn add_admin_id(&mut self, user_id: u32) -> types::ApiResult {
            query_api_result(self, "admin.add", &[serde_json::json!(user_id)])
        }
        fn remove_admin_id(&mut self, user_id: u32) -> types::ApiResult {
            query_api_result(self, "admin.remove", &[serde_json::json!(user_id)])
        }
        fn set_admin_ids(&mut self, ids: Vec<u32>) -> types::ApiResult {
            query_api_result(self, "admin.set", &[serde_json::json!(ids)])
        }
    }

    // ── phira-config ──
    impl wit::phira::plugin::phira_config::Host for WitPluginHost {
        fn get_config(&mut self, key: String) -> types::ApiResult {
            if let Err(error) = self.require_api_capability("config") {
                return error;
            }
            if let Err(error) = crate::wasm_host_helpers::validate_config_key(&key) {
                return types::ApiResult::Error(error);
            }
            let path = crate::wasm_host_helpers::config_path(&self.plugin_name);
            let content = match std::fs::read_to_string(&path) {
                Ok(c) => c,
                Err(_) => return types::ApiResult::Ok(types::JsonValue::Null),
            };
            let root: serde_json::Value = match serde_json::from_str(&content) {
                Ok(v) => v,
                Err(e) => return types::ApiResult::Error(format!("parse config: {e}")),
            };
            // Navigate dot-separated key path (e.g. "api.timeout")
            let value = key
                .split('.')
                .fold(Some(&root), |acc, part| acc.and_then(|v| v.get(part)));
            match value {
                Some(v) => types::ApiResult::Ok(json_to_wit_json(v)),
                None => types::ApiResult::Ok(types::JsonValue::Null),
            }
        }
        fn set_config(&mut self, key: String, value: String) -> types::ApiResult {
            if let Err(error) = self.require_api_capability("config") {
                return error;
            }
            if let Err(error) = crate::wasm_host_helpers::validate_config_key(&key) {
                return types::ApiResult::Error(error);
            }
            let path = crate::wasm_host_helpers::config_path(&self.plugin_name);
            let mut root: serde_json::Value = std::fs::read_to_string(&path)
                .ok()
                .and_then(|c| serde_json::from_str(&c).ok())
                .unwrap_or(serde_json::Value::Object(serde_json::Map::new()));
            let parsed: serde_json::Value = match serde_json::from_str(&value) {
                Ok(v) => v,
                Err(_) => serde_json::Value::String(value),
            };
            // Navigate to the parent object and set the key
            let keys: Vec<&str> = key.split('.').collect();
            if keys.is_empty() {
                return types::ApiResult::Error("empty key".to_string());
            }
            if keys.len() == 1 {
                if let serde_json::Value::Object(ref mut map) = root {
                    map.insert(keys[0].to_string(), parsed);
                }
            } else {
                let mut current = &mut root;
                for &part in keys.iter().take(keys.len() - 1) {
                    current = match current.get_mut(part) {
                        Some(v @ serde_json::Value::Object(_)) => v,
                        Some(_) => {
                            return types::ApiResult::Error(format!(
                                "key '{part}' is not an object"
                            ))
                        }
                        None => return types::ApiResult::Error(format!("key '{part}' not found")),
                    };
                }
                if let serde_json::Value::Object(ref mut map) = current {
                    map.insert(keys[keys.len() - 1].to_string(), parsed);
                }
            }
            // Ensure parent dir exists
            if let Some(parent) = path.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            let bytes = serde_json::to_vec_pretty(&root).unwrap_or_default();
            match crate::wasm_host_helpers::atomic_write(&path, &bytes) {
                Ok(()) => types::ApiResult::Ok(types::JsonValue::Null),
                Err(e) => types::ApiResult::Error(format!("write config: {e}")),
            }
        }
        fn list_config(&mut self, prefix: String) -> types::ApiResult {
            if let Err(error) = self.require_api_capability("config") {
                return error;
            }
            if !prefix.is_empty() {
                if let Err(error) = crate::wasm_host_helpers::validate_config_key(&prefix) {
                    return types::ApiResult::Error(error);
                }
            }
            let path = crate::wasm_host_helpers::config_path(&self.plugin_name);
            let content = match std::fs::read_to_string(&path) {
                Ok(c) => c,
                Err(_) => return types::ApiResult::Ok(types::JsonValue::Array("[]".to_string())),
            };
            let root: serde_json::Value = match serde_json::from_str(&content) {
                Ok(v) => v,
                Err(e) => return types::ApiResult::Error(format!("parse config: {e}")),
            };
            // Collect keys that start with the given prefix
            fn collect_keys(value: &serde_json::Value, prefix: &str, current: &str) -> Vec<String> {
                match value {
                    serde_json::Value::Object(map) => {
                        let mut keys = Vec::new();
                        for (k, v) in map {
                            let path = if current.is_empty() {
                                k.clone()
                            } else {
                                format!("{current}.{k}")
                            };
                            if path.starts_with(prefix) {
                                keys.push(path.clone());
                            }
                            keys.extend(collect_keys(v, prefix, &path));
                        }
                        keys
                    }
                    _ => Vec::new(),
                }
            }
            let keys: Vec<String> = collect_keys(&root, &prefix, "")
                .into_iter()
                .filter(|k| k.starts_with(&prefix))
                .collect();
            types::ApiResult::Ok(json_to_wit_json(&serde_json::json!(keys)))
        }
        fn reload_config(&mut self) -> types::ApiResult {
            if let Err(error) = self.require_api_capability("config") {
                return error;
            }
            let path = crate::wasm_host_helpers::config_path(&self.plugin_name);
            match std::fs::read_to_string(&path) {
                Ok(content) => match serde_json::from_str::<serde_json::Value>(&content) {
                    Ok(_) => types::ApiResult::Ok(types::JsonValue::Null),
                    Err(e) => types::ApiResult::Error(format!("reload config parse: {e}")),
                },
                Err(e) => types::ApiResult::Error(format!("reload config read: {e}")),
            }
        }
        fn poll_config_changes(&mut self, _since: u64) -> types::ApiResult {
            if let Err(error) = self.require_api_capability("config") {
                return error;
            }
            // Simple implementation: check if the config file exists and return its
            // modification time as a version indicator.
            let path = crate::wasm_host_helpers::config_path(&self.plugin_name);
            match std::fs::metadata(&path) {
                Ok(meta) => {
                    let version = meta
                        .modified()
                        .ok()
                        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                        .map(|d| d.as_millis() as u64)
                        .unwrap_or(0);
                    types::ApiResult::Ok(types::JsonValue::Integer(version as i64))
                }
                Err(_) => types::ApiResult::Ok(types::JsonValue::Null),
            }
        }
    }

    // ── phira-runtime ──
    impl wit::phira::plugin::phira_runtime::Host for WitPluginHost {
        fn status(&mut self) -> types::ApiResult {
            query_api_result(self, "runtime.status", &[])
        }
        fn events(&mut self, limit: Option<u32>) -> types::ApiResult {
            query_api_result(
                self,
                "runtime.event_stats",
                &[serde_json::json!(limit.unwrap_or(50))],
            )
        }
        fn commands(&mut self) -> types::ApiResult {
            query_api_result(self, "runtime.commands", &[])
        }
    }

    // ═══════════════════════════════════════════════════════════════
    // JSON conversion helpers
    // ═══════════════════════════════════════════════════════════════

    fn json_to_wit_json(value: &serde_json::Value) -> types::JsonValue {
        use types::JsonValue;
        match value {
            serde_json::Value::Null => JsonValue::Null,
            serde_json::Value::Bool(b) => JsonValue::Flag(*b),
            serde_json::Value::Number(n) => {
                if let Some(i) = n.as_i64() {
                    JsonValue::Integer(i)
                } else if let Some(f) = n.as_f64() {
                    JsonValue::Float(f)
                } else {
                    JsonValue::Text(n.to_string())
                }
            }
            serde_json::Value::String(s) => JsonValue::Text(s.clone()),
            serde_json::Value::Array(arr) => {
                JsonValue::Array(serde_json::to_string(arr).unwrap_or_default())
            }
            serde_json::Value::Object(obj) => {
                JsonValue::Object(serde_json::to_string(obj).unwrap_or_default())
            }
        }
    }

    // ── phira-tcp (plain TCP, no TLS) ──
    impl wit::phira::plugin::phira_tcp::Host for WitPluginHost {
        fn connect(&mut self, addr: String) -> Result<u64, String> {
            self.require_capability("tcp")?;
            let tx = self.ctx.tcp.as_ref().ok_or("tcp not available")?;
            let (reply, rx) = std::sync::mpsc::channel();
            tx.try_send(crate::plugin_tcp::PluginTcpCommand::Connect { plugin_id: self.plugin_name.clone(), addr, reply })
                .map_err(|e| format!("tcp connect failed: {e}"))?;
            rx.recv_timeout(Duration::from_secs(5)).map_err(|e|
                match e {
                    std::sync::mpsc::RecvTimeoutError::Timeout => "tcp connect timed out".to_string(),
                    std::sync::mpsc::RecvTimeoutError::Disconnected => "tcp connect reply lost".to_string(),
                }
            )?
        }

        fn listen(&mut self, addr: String) -> Result<u64, String> {
            self.require_capability("tcp")?;
            let tx = self.ctx.tcp.as_ref().ok_or("tcp not available")?;
            let (reply, rx) = std::sync::mpsc::channel();
            tx.try_send(crate::plugin_tcp::PluginTcpCommand::Listen { plugin_id: self.plugin_name.clone(), addr, reply })
                .map_err(|e| format!("tcp listen failed: {e}"))?;
            rx.recv_timeout(Duration::from_secs(5)).map_err(|e|
                match e {
                    std::sync::mpsc::RecvTimeoutError::Timeout => "tcp listen timed out".to_string(),
                    std::sync::mpsc::RecvTimeoutError::Disconnected => "tcp listen reply lost".to_string(),
                }
            )?
        }

        fn send(&mut self, handle: u64, bytes: Vec<u8>) -> Result<(), String> {
            self.require_capability("tcp")?;
            let tx = self.ctx.tcp.as_ref().ok_or("tcp not available")?;
            tx.try_send(crate::plugin_tcp::PluginTcpCommand::Send { plugin_id: self.plugin_name.clone(), handle, bytes })
                .map_err(|e| format!("tcp send failed: {e}"))
        }

        fn close(&mut self, handle: u64) -> Result<(), String> {
            self.require_capability("tcp")?;
            let tx = self.ctx.tcp.as_ref().ok_or("tcp not available")?;
            tx.try_send(crate::plugin_tcp::PluginTcpCommand::Close { plugin_id: self.plugin_name.clone(), handle })
                .map_err(|e| format!("tcp close failed: {e}"))
        }

        fn accept(&mut self, handle: u64) -> Result<Option<u64>, String> {
            self.require_capability("tcp")?;
            let tx = self.ctx.tcp.as_ref().ok_or("tcp not available")?;
            let (reply, rx) = std::sync::mpsc::channel();
            tx.try_send(crate::plugin_tcp::PluginTcpCommand::Accept { plugin_id: self.plugin_name.clone(), listener_handle: handle, reply })
                .map_err(|e| format!("tcp accept failed: {e}"))?;
            rx.recv_timeout(Duration::from_secs(5)).map_err(|e|
                match e {
                    std::sync::mpsc::RecvTimeoutError::Timeout => "tcp accept timed out".to_string(),
                    std::sync::mpsc::RecvTimeoutError::Disconnected => "tcp accept reply lost".to_string(),
                }
            )?
        }

        fn recv(&mut self, handle: u64, max_bytes: u32) -> Result<Option<Vec<u8>>, String> {
            self.require_capability("tcp")?;
            let tx = self.ctx.tcp.as_ref().ok_or("tcp not available")?;
            let (reply, rx) = std::sync::mpsc::channel();
            tx.try_send(crate::plugin_tcp::PluginTcpCommand::Recv { plugin_id: self.plugin_name.clone(), handle, max_bytes, reply })
                .map_err(|e| format!("tcp recv failed: {e}"))?;
            rx.recv_timeout(Duration::from_secs(5)).map_err(|e|
                match e {
                    std::sync::mpsc::RecvTimeoutError::Timeout => "tcp recv timed out".to_string(),
                    std::sync::mpsc::RecvTimeoutError::Disconnected => "tcp recv reply lost".to_string(),
                }
            )?
        }

        fn peer_addr(&mut self, handle: u64) -> Result<String, String> {
            self.require_capability("tcp")?;
            let tx = self.ctx.tcp.as_ref().ok_or("tcp not available")?;
            let (reply, rx) = std::sync::mpsc::channel();
            tx.try_send(crate::plugin_tcp::PluginTcpCommand::PeerAddr { plugin_id: self.plugin_name.clone(), handle, reply })
                .map_err(|e| format!("tcp peer-addr failed: {e}"))?;
            rx.recv_timeout(Duration::from_secs(5)).map_err(|e|
                match e {
                    std::sync::mpsc::RecvTimeoutError::Timeout => "tcp peer-addr timed out".to_string(),
                    std::sync::mpsc::RecvTimeoutError::Disconnected => "tcp peer-addr reply lost".to_string(),
                }
            )?
        }
    }

    // ── phira-room-state ──
    impl wit::phira::plugin::phira_room_state::Host for WitPluginHost {
        fn get_room_state(&mut self, room_id: String) -> Result<wit::phira::plugin::phira_room_state::RoomState, String> {
            self.require_capability("room-state")?;
            let v = self.ctx.state_query.call("rooms.by_name", &[serde_json::json!(room_id)])?;
            let data = extract_snapshot_data(&v)?;

            let rid = data.get("id").and_then(|v| v.as_str()).unwrap_or("").to_string();
            let room_uuid = data.get("uuid").and_then(|v| v.as_str()).unwrap_or("").to_string();
            let host_val = data.get("host").and_then(|v| v.as_i64()).unwrap_or(-1);
            let host_id = if host_val >= 0 { Some(host_val as u32) } else { None };
            let locked = data.get("locked").and_then(|v| v.as_bool()).unwrap_or(false);
            let hidden = data.get("hidden").and_then(|v| v.as_bool()).unwrap_or(false);
            let player_count = data.get("player_count").and_then(|v| v.as_u64()).unwrap_or(0) as u32;
            let monitor_count = data.get("monitor_count").and_then(|v| v.as_u64()).unwrap_or(0) as u32;

            let players = build_room_players(data);

            // Derive current_round from round_history
            let current_round = extract_current_round(data);

            Ok(wit::phira::plugin::phira_room_state::RoomState {
                room_id: rid, room_uuid, host_id, locked, hidden,
                player_count, monitor_count, players, current_round,
            })
        }

        fn get_room_players(&mut self, room_id: String) -> Result<Vec<wit::phira::plugin::phira_room_state::RoomPlayer>, String> {
            self.require_capability("room-state")?;
            let v = self.ctx.state_query.call("rooms.by_name", &[serde_json::json!(room_id)])?;
            let data = extract_snapshot_data(&v)?;
            Ok(build_room_players(data))
        }

        fn get_player_status(&mut self, room_id: String, user_id: u32) -> Result<Option<wit::phira::plugin::phira_room_state::RoomPlayer>, String> {
            let players = self.get_room_players(room_id)?;
            Ok(players.into_iter().find(|p| p.user_id == user_id))
        }

        fn list_rooms(&mut self) -> Result<Vec<String>, String> {
            self.require_capability("room-state")?;
            let v = self.ctx.state_query.call("rooms.list", &[])?;
            let rooms: Vec<serde_json::Value> = serde_json::from_value(v)
                .map_err(|e| format!("list rooms parse error: {e}"))?;
            // Each room entry is a RoomSnapshot { name, data } — room ID is in data.id.
            // Hidden rooms are already filtered server-side by rooms.list.
            let ids: Vec<String> = rooms.iter()
                .filter_map(|r| {
                    r.get("data")
                        .and_then(|d| d.get("id"))
                        .and_then(|v| v.as_str())
                        .map(|s| s.to_string())
                })
                .collect();
            Ok(ids)
        }
    }

    // ── phira-handler ──
    impl wit::phira::plugin::phira_handler::Host for WitPluginHost {
        fn register_handler(&mut self, desc: wit::phira::plugin::phira_handler::HandlerDescriptor) -> Result<(), String> {
            self.require_capability("handler")?;
            let method = desc.method.clone();
            if method.is_empty() || method.len() > 128 {
                return Err("handler method name must be 1-128 chars".to_string());
            }
            if !method.chars().all(|c| c.is_alphanumeric() || c == '_' || c == '.' || c == ':') {
                return Err("handler method name contains invalid characters".to_string());
            }
            // Reject reserved "phira:" namespace
            if method.starts_with("phira:") {
                return Err("handler method name must not use reserved 'phira:' prefix".to_string());
            }
            // Schema length limits (max 512 bytes)
            if let Some(ref s) = desc.request_schema {
                if s.len() > 512 {
                    return Err("request_schema exceeds maximum length of 512 bytes".to_string());
                }
            }
            if let Some(ref s) = desc.response_schema {
                if s.len() > 512 {
                    return Err("response_schema exceeds maximum length of 512 bytes".to_string());
                }
            }
            let registered = crate::wit_host::RegisteredHandler {
                plugin_name: self.plugin_name.clone(),
                method: method.clone(),
                description: desc.description,
                request_schema: desc.request_schema,
                response_schema: desc.response_schema,
            };
            let mut registry = self.ctx.api_handlers.lock()
                .map_err(|e| format!("handler registry lock: {e}"))?;
            registry.insert(method.clone(), registered);

            // Register in shared PluginManager registry with plugin_name prefix
            // to avoid silent override of another plugin's handler.
            let shared_key = format!("{}.{}", self.plugin_name, method);
            if let (Some(ref shared), Some(ref forward)) = (&self.ctx.services_handlers, &self.ctx.api_forward) {
                {
                    let sh = shared.lock().map_err(|e| format!("handler registry lock: {e}"))?;
                    if sh.contains_key(&shared_key) {
                        return Err(format!("handler method '{method}' is already registered by another plugin"));
                    }
                }
                let method_clone = method.clone();
                let forward_clone = Arc::clone(forward);
                let handler: api::PluginApiHandler = Arc::new(move |_m, args| {
                    let forward = Arc::clone(&forward_clone);
                    let m = method_clone.clone();
                    Box::pin(async move {
                        forward(m, args).await
                    })
                });
                if let Ok(mut sh) = shared.lock() {
                    sh.insert(shared_key, handler);
                }
            }
            // Track handler ownership for cleanup
            if let Some(ref owners) = self.ctx.handler_owners {
                if let Ok(mut map) = owners.lock() {
                    map.entry(self.plugin_name.clone()).or_default().push(method.clone());
                }
            }

            Ok(())
        }

        fn unregister_handler(&mut self, method: String) -> Result<(), String> {
            self.require_capability("handler")?;
            let mut registry = self.ctx.api_handlers.lock()
                .map_err(|e| format!("handler registry lock: {e}"))?;
            match registry.get(&method) {
                Some(h) if h.plugin_name == self.plugin_name => {
                    registry.remove(&method);
                    // Also remove from shared PluginManager registry (prefixed key)
                    if let Some(ref shared) = self.ctx.services_handlers {
                        if let Ok(mut sh) = shared.lock() {
                            sh.remove(&format!("{}.{}", self.plugin_name, method));
                        }
                    }
                    // Remove from handler_owners tracking
                    if let Some(ref owners) = self.ctx.handler_owners {
                        if let Ok(mut map) = owners.lock() {
                            if let Some(methods) = map.get_mut(&self.plugin_name) {
                                methods.retain(|m| m != &method);
                                if methods.is_empty() {
                                    map.remove(&self.plugin_name);
                                }
                            }
                        }
                    }
                    Ok(())
                }
                Some(_) => Err("handler owned by another plugin".to_string()),
                None => Err(format!("handler '{method}' not found")),
            }
        }

        fn list_handlers(&mut self) -> Result<Vec<wit::phira::plugin::phira_handler::HandlerDescriptor>, String> {
            let registry = self.ctx.api_handlers.lock()
                .map_err(|e| format!("handler registry lock: {e}"))?;
            let handlers: Vec<_> = registry.values()
                .filter(|h| h.plugin_name == self.plugin_name)
                .map(|h| wit::phira::plugin::phira_handler::HandlerDescriptor {
                    method: h.method.clone(),
                    description: h.description.clone(),
                    request_schema: h.request_schema.clone(),
                    response_schema: h.response_schema.clone(),
                })
                .collect();
            Ok(handlers)
        }

        fn request_capability(&mut self, _capability: String) -> Result<bool, String> {
            // By default, deny dynamic capability requests.
            // Admin plugins with special permissions may override this.
            Ok(false)
        }
    }

    // ── phira-timer ──
    impl wit::phira::plugin::phira_timer::Host for WitPluginHost {
        fn set_timer(&mut self, delay_ms: u64, timer_id: String) -> Result<(), String> {
            let plugin_name = self.plugin_name.clone();
            let ctx = Arc::clone(&self.ctx);
            let timer_name = timer_id.clone();
            let cb_plugin = plugin_name.clone();

            let handle = tokio::spawn(async move {
                tokio::time::sleep(Duration::from_millis(delay_ms)).await;
                if let Some(cb) = &ctx.timer_callback {
                    cb(cb_plugin, timer_name);
                }
            });

            let mut registry = self.ctx.timers.lock().map_err(|e| format!("timer lock: {e}"))?;
            registry
                .entry(plugin_name)
                .or_default()
                .insert(timer_id, handle);
            Ok(())
        }

        fn clear_timer(&mut self, timer_id: String) -> Result<(), String> {
            let mut registry = self.ctx.timers.lock().map_err(|e| format!("timer lock: {e}"))?;
            if let Some(timers) = registry.get_mut(&self.plugin_name) {
                if let Some(handle) = timers.remove(&timer_id) {
                    handle.abort();
                }
            }
            Ok(())
        }
    }

    // ── phira-crypto ──
    impl wit::phira::plugin::phira_crypto::Host for WitPluginHost {
        fn sign(&mut self, payload: Vec<u8>) -> Result<Vec<u8>, String> {
            self.require_capability("crypto")?;
            Ok(self.ctx.node_key.sign(&payload))
        }

        fn verify(&mut self, pubkey: Vec<u8>, payload: Vec<u8>, signature: Vec<u8>) -> Result<bool, String> {
            self.require_capability("crypto")?;
            Ok(crate::crypto::NodeKey::verify(&pubkey, &payload, &signature))
        }

        fn sha256(&mut self, data: Vec<u8>) -> Result<Vec<u8>, String> {
            self.require_capability("crypto")?;
            Ok(crate::crypto::sha256(&data))
        }

        fn get_node_public_key(&mut self) -> Result<Vec<u8>, String> {
            Ok(self.ctx.node_key.public_key.clone())
        }
    }
} // mod wit_trait_impls

#[cfg(test)]
mod capability_tests {
    use crate::wasm_host_helpers;

    #[test]
    fn required_cap_maps_admin_methods() {
        assert_eq!(
            wasm_host_helpers::required_capability("admin.list"),
            Some("admin")
        );
        assert_eq!(
            wasm_host_helpers::required_capability("admin.add"),
            Some("admin")
        );
    }

    #[test]
    fn required_cap_maps_room_methods() {
        assert_eq!(
            wasm_host_helpers::required_capability("room.set_lock"),
            Some("room.manage")
        );
        assert_eq!(
            wasm_host_helpers::required_capability("room.kick"),
            Some("room.manage")
        );
    }

    #[test]
    fn required_cap_returns_none_for_unguarded_methods() {
        assert_eq!(wasm_host_helpers::required_capability("uuid.v4"), None);
        assert_eq!(wasm_host_helpers::required_capability("time.now"), None);
    }

    #[test]
    fn default_capabilities_include_all() {
        let caps = wasm_host_helpers::default_capabilities();
        assert!(caps.contains("admin"), "default must include admin");
        assert!(
            caps.contains("room.manage"),
            "default must include room.manage"
        );
        assert!(
            caps.contains("state.read"),
            "default must include state.read"
        );
        assert!(caps.contains("config"), "default must include config");
    }

    #[test]
    fn persist_methods_require_state_read() {
        let methods = [
            "persist.events",
            "persist.rooms",
            "persist.touches",
            "persist.judges",
        ];
        for method in &methods {
            let cap = wasm_host_helpers::required_capability(method);
            assert_eq!(
                cap,
                Some("state.read"),
                "method {method} should require state.read"
            );
        }
    }

    #[test]
    fn admin_methods_require_admin() {
        let methods = ["admin.list", "admin.add", "admin.remove", "admin.set"];
        for method in &methods {
            let cap = wasm_host_helpers::required_capability(method);
            assert_eq!(cap, Some("admin"), "method {method} should require admin");
        }
    }
}
