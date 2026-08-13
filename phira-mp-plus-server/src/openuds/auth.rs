//! Authentication for OpenUDS connections.
//!
//! Two modes:
//! 1. **Token mode** (auth_token is set): Client sends `{"type":"authenticate","token":"xxx"}`
//!    and the server validates the token.
//! 2. **Direct mode** (auth_token is empty): The Unix socket's filesystem
//!    permissions (mode 660) already isolate access, so any `authenticate` frame
//!    is accepted directly.

/// Shared authentication state.
pub struct AuthState {
    /// Configured auth token. Empty = direct (socket-permission) mode.
    auth_token: String,
}

impl AuthState {
    pub fn new(auth_token: String) -> Self {
        Self { auth_token }
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

/// Generate a standardized `auth_error` response.
pub fn build_auth_error_response(message: &str) -> serde_json::Value {
    serde_json::json!({
        "type": "auth_error",
        "message": message,
    })
}
