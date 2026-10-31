//! Human-readable rendering for `quota-cli usage`.
//!
//! The Herdr sidebar has one narrow line per pane, so `compact_token` stays
//! percent-only. A terminal has room, so the text report labels each percent
//! as what's left, like the desktop app's default, and spells out the reset time.

use crate::accounts::AccountReport;
use quota_core::usage::{LimitResets, ProviderUsage, UsageWindow};
use serde::Serialize;

/// Render one provider's detail line: every window that reports a percent,
/// plus how long until it rolls over when the provider tells us, and any
/// banked limit resets.
///
/// `None` when there is nothing at all to say, matching `compact_token` so the
/// caller keeps one fallback.
pub fn usage_detail(usage: &ProviderUsage, now: i64) -> Option<String> {
    let mut parts: Vec<String> = usage
        .windows
        .iter()
        .filter_map(|window| {
            let percent = window.remaining_percent?;
            Some(match window.reset_at {
                Some(reset_at) => format!(
                    "{} {}% left ({})",
                    window.label,
                    percent,
                    relative_reset(reset_at, now)
                ),
                None => format!("{} {}% left", window.label, percent),
            })
        })
        .collect();

    if let Some(resets) = usage
        .resets
        .as_ref()
        .and_then(|resets| resets_detail(resets, now))
    {
        parts.push(resets);
    }

    if parts.is_empty() {
        usage.note.clone()
    } else {
        Some(parts.join(" · "))
    }
}

/// The name a report line starts with: the provider, plus the label from the
/// config file when the login has one.
pub fn account_name(provider: &str, label: Option<&str>) -> String {
    match label {
        Some(label) => format!("{} ({})", provider, label),
        None => provider.to_string(),
    }
}

/// Whether the text report shows a login. Without `--all` it leaves out
/// providers that are not set up here and logins with nothing to show. A login
/// from the config file always shows, so a mistake in the file is visible.
pub fn shown_in_text(report: &AccountReport, now: i64, all: bool) -> bool {
    if all || report.is_configured() {
        return true;
    }
    match &report.result {
        Ok(usage) => usage_detail(usage, now).is_some(),
        Err(err) => !err.not_configured,
    }
}

/// One JSON record per login, including the ones that failed or are not set
/// up, so a script sees what was checked. Every record has every field, so a
/// script can read `windows` without checking `status` first.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UsageRecord<'a> {
    pub provider: &'a str,
    /// The config file's label, `null` for a login found the usual way.
    pub label: Option<&'a str>,
    /// `ok`, `not_configured`, or `error`.
    pub status: &'static str,
    pub account_label: Option<&'a str>,
    pub windows: &'a [UsageWindow],
    pub note: Option<&'a str>,
    pub resets: Option<&'a LimitResets>,
    pub error: Option<&'a str>,
}

pub fn usage_record(report: &AccountReport) -> UsageRecord<'_> {
    let base = UsageRecord {
        provider: report.provider,
        label: report.label.as_deref(),
        status: "ok",
        account_label: None,
        windows: &[],
        note: None,
        resets: None,
        error: None,
    };
    match &report.result {
        Ok(usage) => UsageRecord {
            account_label: usage.account_label.as_deref(),
            windows: &usage.windows,
            note: usage.note.as_deref(),
            resets: usage.resets.as_ref(),
            ..base
        },
        Err(err) => UsageRecord {
            status: if err.not_configured {
                "not_configured"
            } else {
                "error"
            },
            error: Some(&err.message),
            ..base
        },
    }
}

/// "2 resets available (next expires in 6d 4h)". Nothing for zero, since a
/// line per account saying so would only be noise.
fn resets_detail(resets: &LimitResets, now: i64) -> Option<String> {
    let count = resets.available;
    if count == 0 {
        return None;
    }

    let available = if count == 1 {
        "1 reset available".to_string()
    } else {
        format!("{} resets available", count)
    };

    Some(match resets.expires_at {
        Some(expires_at) => {
            let which = if count == 1 {
                "expires"
            } else {
                "next expires"
            };
            match duration_until(expires_at, now) {
                Some(left) => format!("{} ({} in {})", available, which, left),
                None => format!("{} ({} now)", available, which),
            }
        }
        None => available,
    })
}

/// How long until a window resets. A stamp already in the past reads as due
/// rather than as a negative duration, because a provider can report a
/// boundary we have crossed before its own numbers catch up.
fn relative_reset(reset_at: i64, now: i64) -> String {
    match duration_until(reset_at, now) {
        Some(left) => format!("resets in {}", left),
        None => "resets now".to_string(),
    }
}

/// The time until `at`, in the coarsest two units that still say something
/// useful. `None` once it has passed.
fn duration_until(at: i64, now: i64) -> Option<String> {
    let seconds = at - now;
    if seconds <= 0 {
        return None;
    }

    let minutes = seconds / 60;
    let hours = minutes / 60;
    let days = hours / 24;

    if days > 0 {
        let hours = hours % 24;
        if hours > 0 {
            return Some(format!("{}d {}h", days, hours));
        }
        return Some(format!("{}d", days));
    }

    if hours > 0 {
        let minutes = minutes % 60;
        if minutes > 0 {
            return Some(format!("{}h {}m", hours, minutes));
        }
        return Some(format!("{}h", hours));
    }

    Some(format!("{}m", minutes.max(1)))
}
