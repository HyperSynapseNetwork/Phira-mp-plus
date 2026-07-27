//! Authentication for OpenUDS connections.
//!
//! Two modes:
//! 1. **Token mode** (auth_token is set): Client sends `{"type":"authenticate","token":"xxx"}`
//!    and the server validates the token.
//! 2. **CLI approve mode** (auth_token is empty): Client sends
//!    `{"type":"authenticate","client_name":"my-tool"}` and the server generates
//!    a pending_id. An admin must run `approve openuds <pending_id>` in the CLI
//!    to authorize the connection.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;
use uuid::Uuid;

/// Time-to-live for a pending approval (milliseconds).
const PENDING_AUTH_TTL_MS: u64 = 120_000;

/// A pending approval request.
#[derive(Debug, Clone)]
pub struct PendingAuth {
    /// Unique identifier for this pending request.
    pub pending_id: String,
    /// Client-provided name.
    pub client_name: String,
    /// Completion channel. The session task waits on this.
    pub tx: tokio::sync::oneshot::Sender<AuthResult>,
    /// Creation timestamp (ms).
    pub created_at: i64,
}

/// Result of an authentication attempt.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuthResult {
    /// Whether authentication was successful.
    pub success: bool,
    /// Assigned session UUID (if success).
    pub session_id: Option<String>,
    /// Error message (if failure).
    pub error: Option<String>,
}

/// Shared authentication state.
pub struct AuthState {
    /// Configured auth token. Empty = CLI approve mode.
    auth_token: String,
    /// Pending approvals keyed by pending_id (CLI approve mode only).
    pending: RwLock<HashMap<String, PendingAuth>>,
}

impl AuthState {
    pub fn new(auth_token: String) -> Self {
        Self {
            auth_token,
            pending: RwLock::new(HashMap::new()),
        }
    }

    /// Returns true if token mode is active.
    pub fn is_token_mode(&self) -> bool {
        !self.auth_token.is_empty()
    }

    /// Validate a token against the configured auth token.
    pub fn validate_token(&self, token: &str) -> bool {
        if self.auth_token.is_empty() {
            return false;
        }
        // Constant-time comparison to prevent timing attacks
        let configured = self.auth_token.as_bytes();
        let provided = token.as_bytes();
        if configured.len() != provided.len() {
            return false;
        }
        let mut result = 0u8;
        for (a, b) in configured.iter().zip(provided.iter()) {
            result |= a ^ b;
        }
        result == 0
    }

    /// Create a pending auth request in CLI approve mode.
    /// Returns the pending_id.
    pub async fn create_pending(
        &self,
        client_name: String,
    ) -> Result<(String, tokio::sync::oneshot::Receiver<AuthResult>), String> {
        let (tx, rx) = tokio::sync::oneshot::channel();
        let pending_id = Uuid::new_v4().to_string();
        let now_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as i64)
            .unwrap_or(0);

        let pending = PendingAuth {
            pending_id: pending_id.clone(),
            client_name,
            tx,
            created_at: now_ms,
        };

        let mut map = self.pending.write().await;
        // Prune expired entries before insert
        map.retain(|_, p| (now_ms - p.created_at) < PENDING_AUTH_TTL_MS as i64);
        map.insert(pending_id.clone(), pending);

        Ok((pending_id, rx))
    }

    /// Approve a pending auth request. Called by the CLI `approve openuds` command.
    /// Returns true if a pending request was found and approved.
    pub async fn approve_pending(&self, pending_id: &str) -> bool {
        let mut map = self.pending.write().await;
        if let Some(pending) = map.remove(pending_id) {
            let session_id = Uuid::new_v4().to_string();
            let result = AuthResult {
                success: true,
                session_id: Some(session_id),
                error: None,
            };
            let _ = pending.tx.send(result);
            true
        } else {
            false
        }
    }

    /// Reject a pending auth request. Called on timeout or explicit rejection.
    pub async fn reject_pending(&self, pending_id: &str, error: &str) -> bool {
        let mut map = self.pending.write().await;
        if let Some(pending) = map.remove(pending_id) {
            let result = AuthResult {
                success: false,
                session_id: None,
                error: Some(error.to_string()),
            };
            let _ = pending.tx.send(result);
            true
        } else {
            false
        }
    }

    /// List all pending auth requests (for CLI display).
    pub async fn list_pending(&self) -> Vec<(String, String, i64)> {
        let map = self.pending.read().await;
        map.iter()
            .map(|(id, p)| (id.clone(), p.client_name.clone(), p.created_at))
            .collect()
    }
}

/// Generate a standardized `authenticated` response.
pub fn build_authenticated_response(
    session_id: &str,
    server_version: &str,
) -> serde_json::Value {
    serde_json::json!({
        "type": "authenticated",
        "session_id": session_id,
        "server_version": server_version,
    })
}

/// Generate a standardized `auth_pending` response.
pub fn build_auth_pending_response(pending_id: &str) -> serde_json::Value {
    serde_json::json!({
        "type": "auth_pending",
        "pending_id": pending_id,
    })
}

/// Generate a standardized `auth_error` response.
pub fn build_auth_error_response(message: &str) -> serde_json::Value {
    serde_json::json!({
        "type": "auth_error",
        "message": message,
    })
}
