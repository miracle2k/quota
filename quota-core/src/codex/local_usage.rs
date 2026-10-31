//! Fetch Codex usage with the locally stored Codex CLI token and normalize the
//! response into the shared usage model.

use crate::usage::{LimitResets, ProviderUsage, UsageWindow};
use serde_json::Value;

use super::local::{read_local_credentials, LocalCodexCredentials, LocalCredentialError};

const USAGE_URL: &str = "https://chatgpt.com/backend-api/wham/usage";
/// Lists each banked reset with its expiry. Read-only: redeeming one is a
/// separate POST that this reader never sends.
const RESET_CREDITS_URL: &str = "https://chatgpt.com/backend-api/wham/rate-limit-reset-credits";

/// Convert the raw usage payload into the two windows the sidebar shows.
///
/// The API reports `used_percent`, so every number here is inverted into the
/// remaining percent the shared model stores.
pub fn normalize_usage_response(raw: &Value, account_label: Option<String>) -> ProviderUsage {
    let rate_limit = raw.get("rate_limit");
    let primary = rate_limit.and_then(|limit| limit.get("primary_window"));
    let secondary = rate_limit.and_then(|limit| limit.get("secondary_window"));

    let label = account_label.or_else(|| {
        raw.get("plan_type")
            .and_then(Value::as_str)
            .map(str::to_string)
    });

    ProviderUsage {
        provider: "codex".to_string(),
        account_label: label,
        windows: vec![
            window_from(primary, "5h"),
            window_from(secondary, "Wk"),
        ],
        note: None,
        resets: limit_resets(raw),
        balance: None,
    }
}

/// Banked usage-limit resets, which the payload counts in
/// `rate_limit_reset_credits`. The count carries no expiry; [`fetch_usage`]
/// asks for that separately when there is a reset to ask about.
fn limit_resets(raw: &Value) -> Option<LimitResets> {
    let count = raw
        .get("rate_limit_reset_credits")?
        .get("available_count")?
        .as_u64()?;
    Some(LimitResets {
        available: u32::try_from(count).unwrap_or(u32::MAX),
        expires_at: None,
    })
}

/// Build one window, preferring a label derived from the window's own length
/// over the caller's guess. Codex reports the window size, so a plan with a
/// different primary window still reads correctly.
fn window_from(block: Option<&Value>, fallback_label: &str) -> UsageWindow {
    let minutes = block
        .and_then(|item| item.get("limit_window_seconds"))
        .and_then(Value::as_i64)
        .filter(|seconds| *seconds > 0)
        .map(|seconds| (seconds + 59) / 60);

    UsageWindow {
        label: minutes
            .and_then(label_for_minutes)
            .unwrap_or_else(|| fallback_label.to_string()),
        remaining_percent: block
            .and_then(|item| item.get("used_percent"))
            .and_then(Value::as_f64)
            .filter(|used| used.is_finite())
            .map(|used| (100.0 - used.round()).clamp(0.0, 100.0) as i32),
        reset_at: reset_at(block),
    }
}

/// Render a window length as the short label the sidebar shows. Anything that
/// is not a clean hour or day count falls back to the caller's label rather
/// than inventing something like "437m".
fn label_for_minutes(minutes: i64) -> Option<String> {
    if minutes % 1440 == 0 {
        let days = minutes / 1440;
        return match days {
            1 => Some("1d".to_string()),
            7 => Some("Wk".to_string()),
            other => Some(format!("{}d", other)),
        };
    }
    if minutes % 60 == 0 {
        return Some(format!("{}h", minutes / 60));
    }
    None
}

/// Codex sends either an absolute reset timestamp or seconds from now.
fn reset_at(block: Option<&Value>) -> Option<i64> {
    let block = block?;
    if let Some(value) = block.get("reset_at").and_then(Value::as_i64) {
        return Some(value);
    }
    let seconds = block.get("reset_after_seconds").and_then(Value::as_i64)?;
    if seconds < 0 {
        return None;
    }
    Some(chrono::Utc::now().timestamp() + seconds)
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
    /// No Codex login with usage exists here, as opposed to one that failed.
    /// An API-key login counts, because it has no usage window to report.
    pub fn is_not_configured(&self) -> bool {
        matches!(
            self,
            Self::Credentials(LocalCredentialError::NotFound | LocalCredentialError::ApiKeyOnly)
        )
    }
}

impl std::fmt::Display for LocalUsageError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Credentials(inner) => write!(formatter, "{}", inner),
            Self::Unauthorized => write!(
                formatter,
                "Codex rejected the stored token. Run the Codex CLI to refresh it."
            ),
            Self::Forbidden => write!(formatter, "Codex refused the usage request (HTTP 403)."),
            Self::Request(detail) => write!(formatter, "Codex usage request failed: {}", detail),
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
    credentials: &LocalCodexCredentials,
) -> Result<ProviderUsage, LocalUsageError> {
    let mut request = reqwest::Client::new()
        .get(USAGE_URL)
        .header(reqwest::header::ACCEPT, "application/json")
        .header(
            reqwest::header::AUTHORIZATION,
            format!("Bearer {}", credentials.access_token),
        )
        .header(reqwest::header::USER_AGENT, "quota");

    if let Some(account_id) = credentials.account_id.as_deref() {
        request = request.header("ChatGPT-Account-Id", account_id);
    }

    let response = request
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

    let mut usage = normalize_usage_response(&raw, credentials.plan.clone());
    if usage
        .resets
        .as_ref()
        .is_some_and(|resets| resets.available > 0)
    {
        // The expiry is a detail. Without it the count still stands.
        if let Some(listed) = fetch_reset_credits(credentials).await {
            usage.resets = Some(listed);
        }
    }
    Ok(usage)
}

async fn fetch_reset_credits(credentials: &LocalCodexCredentials) -> Option<LimitResets> {
    let mut request = reqwest::Client::new()
        .get(RESET_CREDITS_URL)
        .header(reqwest::header::ACCEPT, "application/json")
        .header(
            reqwest::header::AUTHORIZATION,
            format!("Bearer {}", credentials.access_token),
        )
        .header(reqwest::header::USER_AGENT, "quota");
    if let Some(account_id) = credentials.account_id.as_deref() {
        request = request.header("ChatGPT-Account-Id", account_id);
    }
    let response = request.send().await.ok()?;
    if !response.status().is_success() {
        return None;
    }
    let raw: Value = response.json().await.ok()?;
    reset_credits_from(&raw, chrono::Utc::now().timestamp())
}

/// Count the resets still usable in a reset-credit list, and find the soonest
/// expiry among them. A credit without an expiry never lapses.
pub fn reset_credits_from(raw: &Value, now: i64) -> Option<LimitResets> {
    let credits = raw.get("credits")?.as_array()?;
    let mut available: u32 = 0;
    let mut expires_at: Option<i64> = None;
    for credit in credits {
        if credit.get("status").and_then(Value::as_str) != Some("available") {
            continue;
        }
        let expiry = credit
            .get("expires_at")
            .and_then(Value::as_str)
            .and_then(|text| chrono::DateTime::parse_from_rfc3339(text).ok())
            .map(|stamp| stamp.timestamp());
        if expiry.is_some_and(|expiry| expiry <= now) {
            continue;
        }
        available = available.saturating_add(1);
        if let Some(expiry) = expiry {
            expires_at = Some(expires_at.map_or(expiry, |soonest| soonest.min(expiry)));
        }
    }
    Some(LimitResets {
        available,
        expires_at,
    })
}
