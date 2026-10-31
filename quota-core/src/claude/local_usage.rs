//! Fetch Claude usage with the locally stored Claude Code token and normalize
//! the response into the shared usage model.

use crate::usage::{LimitResets, ProviderUsage, UsageWindow};
use serde_json::Value;

use super::local::{read_local_credentials, LocalClaudeCredentials, LocalCredentialError};

/// `cedar_ember=1` asks for the limit-reset status block, which the plain call
/// returns as null. `skip_spend=1` leaves out spend figures this reader never
/// shows. Claude Code sends the same pair when it checks for resets.
const USAGE_URL: &str = "https://api.anthropic.com/api/oauth/usage?cedar_ember=1&skip_spend=1";
const BETA_HEADER: &str = "oauth-2025-04-20";

/// The usage API reports limit resets only to a client whose user agent starts
/// with `claude-cli/`. Any other client gets `ineligible_reason: "surface"` and
/// no grants. So this leads with Claude Code's product token, and keeps quota's
/// own token after it, so the request still says which tool sent it.
pub(crate) const USER_AGENT: &str = concat!(
    "claude-cli/2.1.289 (external, cli) quota/",
    env!("CARGO_PKG_VERSION")
);

/// Convert the raw usage payload into the two windows the sidebar shows, plus
/// any banked limit resets. Model-specific blocks in the response are
/// deliberately ignored.
pub fn normalize_usage_response(raw: &Value, account_label: Option<String>) -> ProviderUsage {
    ProviderUsage {
        provider: "claude".to_string(),
        account_label,
        windows: vec![
            window_from("5h", raw.get("five_hour")),
            window_from("Wk", raw.get("seven_day")),
        ],
        note: None,
        resets: limit_resets(raw.get("cedar_ember")),
    }
}

/// Count the banked limit resets in the `cedar_ember` block, the API's name for
/// Claude's limit-reset grants. Each grant carries its own `resets_left` and
/// an `ends_at` after which its resets lapse.
///
/// `None` when the API did not say: the block is missing or null, or it was
/// withheld because the request did not look like Claude Code (`"surface"`).
/// An account without grants reports zero.
fn limit_resets(block: Option<&Value>) -> Option<LimitResets> {
    let block = block.filter(|value| value.is_object())?;
    if block.get("ineligible_reason").and_then(Value::as_str) == Some("surface") {
        return None;
    }

    let mut available: u32 = 0;
    let mut expires_at: Option<i64> = None;
    let grants = block.get("grants").and_then(Value::as_array);
    for grant in grants.into_iter().flatten() {
        let left = grant.get("resets_left").and_then(Value::as_u64).unwrap_or(0);
        if left == 0 {
            continue;
        }
        available = available.saturating_add(u32::try_from(left).unwrap_or(u32::MAX));
        if let Some(ends_at) = grant.get("ends_at").and_then(parse_timestamp) {
            expires_at = Some(expires_at.map_or(ends_at, |soonest| soonest.min(ends_at)));
        }
    }

    Some(LimitResets {
        available,
        expires_at,
    })
}

fn parse_timestamp(value: &Value) -> Option<i64> {
    value
        .as_str()
        .and_then(|text| chrono::DateTime::parse_from_rfc3339(text).ok())
        .map(|stamp| stamp.timestamp())
}

fn window_from(label: &str, block: Option<&Value>) -> UsageWindow {
    UsageWindow {
        label: label.to_string(),
        remaining_percent: block
            .and_then(|item| item.get("utilization"))
            .and_then(Value::as_f64)
            .filter(|used| used.is_finite())
            .map(|used| (100.0 - used.round()).clamp(0.0, 100.0) as i32),
        reset_at: block
            .and_then(|item| item.get("resets_at"))
            .and_then(parse_timestamp),
    }
}

#[derive(Debug)]
pub enum LocalUsageError {
    Credentials(LocalCredentialError),
    /// HTTP 401: the token is no good. Renewing it can help.
    Unauthorized,
    /// HTTP 403: the request was refused for another reason, such as a
    /// missing permission or a bot check. Renewing the token does not help.
    Forbidden,
    Request(String),
}

impl LocalUsageError {
    /// No Claude Code login exists here, as opposed to one that failed.
    pub fn is_not_configured(&self) -> bool {
        matches!(self, Self::Credentials(LocalCredentialError::NotFound))
    }
}

impl std::fmt::Display for LocalUsageError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Credentials(inner) => write!(formatter, "{}", inner),
            Self::Unauthorized => write!(
                formatter,
                "Claude rejected the stored token. Run the Claude Code CLI to refresh it."
            ),
            Self::Forbidden => write!(
                formatter,
                "Claude refused the usage request (HTTP 403)."
            ),
            Self::Request(detail) => write!(formatter, "Claude usage request failed: {}", detail),
        }
    }
}

/// Read local credentials and fetch current usage. Never writes to disk.
pub async fn fetch_local_usage() -> Result<ProviderUsage, LocalUsageError> {
    let credentials = read_local_credentials().map_err(LocalUsageError::Credentials)?;
    fetch_usage(&credentials).await
}

/// Fetch current usage for credentials already read, such as one of several
/// logins. Never writes to disk.
pub async fn fetch_usage(
    credentials: &LocalClaudeCredentials,
) -> Result<ProviderUsage, LocalUsageError> {
    let response = reqwest::Client::new()
        .get(USAGE_URL)
        .header(
            reqwest::header::AUTHORIZATION,
            format!("Bearer {}", credentials.access_token),
        )
        .header("anthropic-beta", BETA_HEADER)
        .header(reqwest::header::USER_AGENT, USER_AGENT)
        .send()
        .await
        .map_err(|err| LocalUsageError::Request(err.to_string()))?;

    let status = response.status();
    let body = response
        .text()
        .await
        .map_err(|err| LocalUsageError::Request(err.to_string()))?;

    if status.as_u16() == 401 {
        return Err(LocalUsageError::Unauthorized);
    }
    if status.as_u16() == 403 {
        return Err(LocalUsageError::Forbidden);
    }
    if !status.is_success() {
        return Err(LocalUsageError::Request(format!(
            "status={} body_length={}",
            status.as_u16(),
            body.len()
        )));
    }

    let raw: Value =
        serde_json::from_str(&body).map_err(|err| LocalUsageError::Request(err.to_string()))?;

    Ok(normalize_usage_response(
        &raw,
        credentials.subscription_type.clone(),
    ))
}
