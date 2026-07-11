//! Authentication helpers for client sessions.
//!
//! This module keeps token validation, remote `/me` lookup, ban rejection
//! formatting, and the delayed auth failure flush out of the session hot path.

use crate::l10n::Language;
use crate::phira_client::PhiraRetryNoticeTarget;
use crate::server::PlusServerState;
use anyhow::{bail, Result};
use phira_mp_common::{ServerCommand, StreamSender};
use serde::Deserialize;
use tokio::time::{self, Duration};
use tracing::warn;

const AUTH_FAILURE_RESPONSE_DELAY: Duration = Duration::from_millis(50);
const AUTH_FAILURE_FLUSH_TIMEOUT: Duration = Duration::from_secs(1);
const MAX_CLIENT_BAN_REASON_CHARS: usize = 160;

#[derive(Debug, Deserialize)]
pub(crate) struct AuthUserInfo {
    pub id: i32,
    pub name: String,
    pub language: String,
}

pub(crate) async fn authenticate_remote_with_notice(
    server: &PlusServerState,
    token: &str,
    target: PhiraRetryNoticeTarget<'_>,
) -> Result<AuthUserInfo> {
    if token.len() > 128 {
        bail!("invalid token");
    }
    server
        .phira_client
        .get_json(
            &server.config.phira_api_endpoint,
            None,
            "/me",
            Some(token),
            target,
        )
        .await
}

#[allow(dead_code)]
pub(crate) fn ban_rejection_message(language: &str, reason: &str) -> String {
    let language = language.parse::<Language>().unwrap_or_default();
    let mut reason = reason.split_whitespace().collect::<Vec<_>>().join(" ");

    if reason.is_empty() || reason == "你的账号已被封禁" {
        reason = crate::l10n::try_translate(&language.0, "auth-banned-default-reason");
    }

    if reason.chars().count() > MAX_CLIENT_BAN_REASON_CHARS {
        reason = reason
            .chars()
            .take(MAX_CLIENT_BAN_REASON_CHARS.saturating_sub(1))
            .collect::<String>();
        reason.push('…');
    }

    let mut args = fluent::FluentArgs::new();
    args.set("reason", reason);
    crate::l10n::try_translate_with_args(&language.0, "auth-banned", args)
        .chars()
        .filter(|ch| !matches!(ch, '\u{2068}' | '\u{2069}'))
        .collect()
}

pub(crate) async fn send_auth_rejection(send_tx: &StreamSender<ServerCommand>, message: String) {
    time::sleep(AUTH_FAILURE_RESPONSE_DELAY).await;
    match time::timeout(
        AUTH_FAILURE_FLUSH_TIMEOUT,
        send_tx.send_and_flush(ServerCommand::Authenticate(Err(message))),
    )
    .await
    {
        Ok(Ok(())) => {}
        Ok(Err(err)) => warn!("failed to deliver authentication rejection: {err:?}"),
        Err(_) => warn!("timed out while delivering authentication rejection"),
    }
}

#[cfg(test)]
mod tests {
    use super::ban_rejection_message;

    #[test]
    fn ban_rejection_includes_reason_in_client_language() {
        let message = ban_rejection_message("zh-CN", "恶意刷屏");
        assert!(message.contains("恶意刷屏"));
        assert!(message.contains("封禁"));
    }

    #[test]
    fn ban_rejection_uses_default_reason_when_empty() {
        let message = ban_rejection_message("en-US", "   ");
        assert!(message.contains("Violation of server rules"));
    }
}
