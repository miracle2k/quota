//! Human-readable rendering for `quota-cli usage`.
//!
//! The Herdr sidebar has one narrow line per pane, so `compact_token` stays
//! percent-only. A terminal has room, so the text report labels each percent
//! as what's left, like the desktop app's default, and spells out the reset time.

use crate::accounts::{AccountReport, ListedAccount, Login, Store};
use quota_core::usage::{Balance, LimitResets, ProviderUsage, UsageWindow};
use serde::Serialize;

/// Render one provider's detail line: every window that reports a percent,
/// plus how long until it rolls over when the provider tells us, any banked
/// limit resets, and a prepaid balance.
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
    let has_windows = !parts.is_empty();

    if let Some(resets) = usage
        .resets
        .as_ref()
        .and_then(|resets| resets_detail(resets, now))
    {
        parts.push(resets);
    }
    if let Some(balance) = &usage.balance {
        parts.push(format!("{} balance", balance.display()));
    }

    if parts.is_empty() {
        return usage.note.clone();
    }
    // Without windows the note explains why, as in "No GLM Coding Plan".
    if !has_windows {
        parts.extend(usage.note.clone());
    }
    Some(parts.join(" · "))
}

/// The name a report line starts with: the provider, plus the account's label
/// or address when it has one.
pub fn account_name(provider: &str, label: Option<&str>) -> String {
    match label {
        Some(label) => format!("{} ({})", provider, label),
        None => provider.to_string(),
    }
}

/// Whether the text report shows an account. Without `--all` it leaves out
/// providers that are not set up here, logins with nothing to show, and
/// failures of the providers the config file lists in `usage.hide_errors`.
/// Otherwise an account set up for quota-cli, through `accounts add` or the
/// config file, always shows, so a mistake there is visible.
pub fn shown_in_text(report: &AccountReport, now: i64, all: bool, hide_errors: bool) -> bool {
    if all {
        return true;
    }
    if hide_errors && report.result.is_err() {
        return false;
    }
    if report.is_configured() {
        return true;
    }
    match &report.result {
        Ok(usage) => usage_detail(usage, now).is_some(),
        Err(err) => !err.not_configured,
    }
}

/// One JSON record per account, including the ones that failed or are not set
/// up, so a script sees what was checked. Every record has every field, so a
/// script can read `windows` without checking `status` first.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UsageRecord<'a> {
    pub provider: &'a str,
    /// The name shown for the account: a label, an alias, or the address.
    /// `null` for the login the provider's CLI uses now.
    pub label: Option<&'a str>,
    pub email: Option<&'a str>,
    /// Where the account was found: `current`, `quota-cli`, `config`,
    /// `codexbar`, `claude-swap`.
    pub sources: Vec<&'static str>,
    /// `ok`, `not_configured`, or `error`.
    pub status: &'static str,
    pub account_label: Option<&'a str>,
    pub windows: &'a [UsageWindow],
    pub note: Option<&'a str>,
    pub resets: Option<&'a LimitResets>,
    pub balance: Option<&'a Balance>,
    pub error: Option<&'a str>,
}

pub fn usage_record(report: &AccountReport) -> UsageRecord<'_> {
    let base = UsageRecord {
        provider: report.provider,
        label: report.label.as_deref(),
        email: report.email.as_deref(),
        sources: report.sources.iter().map(|source| source.name()).collect(),
        status: "ok",
        account_label: None,
        windows: &[],
        note: None,
        resets: None,
        balance: None,
        error: None,
    };
    match &report.result {
        Ok(usage) => UsageRecord {
            account_label: usage.account_label.as_deref(),
            windows: &usage.windows,
            note: usage.note.as_deref(),
            resets: usage.resets.as_ref(),
            balance: usage.balance.as_ref(),
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

/// `accounts list` as text: each account's name, then one line per login of
/// it: where it was found, whose it is, and where it lives.
pub fn account_list(accounts: &[ListedAccount]) -> String {
    let rows: Vec<(String, String, String)> = accounts
        .iter()
        .flat_map(|account| account.logins.iter())
        .map(|login| {
            (
                login.source.name().to_string(),
                login_state(login),
                login
                    .location
                    .as_deref()
                    .map(shorten_home)
                    .unwrap_or_default(),
            )
        })
        .collect();
    let source_width = rows.iter().map(|row| row.0.len()).max().unwrap_or(0);
    let state_width = rows
        .iter()
        .map(|row| row.1.chars().count())
        .max()
        .unwrap_or(0);

    let mut out = String::new();
    let mut rows = rows.into_iter();
    for account in accounts {
        out.push_str(&account_name(account.provider, account.label.as_deref()));
        out.push('\n');
        for _ in &account.logins {
            let (source, state, location) = rows.next().unwrap_or_default();
            let line = format!(
                "  {:<source_width$}  {:<state_width$}  {}",
                source,
                state,
                location,
                source_width = source_width,
                state_width = state_width
            );
            out.push_str(line.trim_end());
            out.push('\n');
        }
    }
    out
}

/// The login's address, or why it cannot be used.
fn login_state(login: &Login) -> String {
    match (&login.problem, &login.store) {
        (Some(problem), _) if problem.not_configured => "not signed in".to_string(),
        (Some(_), _) => "unusable (`usage` says why)".to_string(),
        (None, Store::Zai(..)) => "API key".to_string(),
        (None, _) => login
            .email
            .clone()
            .unwrap_or_else(|| "signed in".to_string()),
    }
}

/// `accounts list --json`: one record per account, each with its logins.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ListedRecord<'a> {
    pub provider: &'a str,
    pub label: Option<&'a str>,
    pub email: Option<&'a str>,
    pub logins: Vec<ListedLogin<'a>>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ListedLogin<'a> {
    pub source: &'static str,
    /// Its own label: a quota-cli or config file label, or a claude-swap alias.
    pub label: Option<&'a str>,
    pub email: Option<&'a str>,
    pub location: Option<&'a str>,
    /// `ok`, `not_configured`, or `error`.
    pub status: &'static str,
    pub error: Option<&'a str>,
}

pub fn listed_record(account: &ListedAccount) -> ListedRecord<'_> {
    ListedRecord {
        provider: account.provider,
        label: account.label.as_deref(),
        email: account.email.as_deref(),
        logins: account
            .logins
            .iter()
            .map(|login| ListedLogin {
                source: login.source.name(),
                label: login.label.as_deref(),
                email: login.email.as_deref(),
                location: login.location.as_deref(),
                status: match &login.problem {
                    None => "ok",
                    Some(problem) if problem.not_configured => "not_configured",
                    Some(_) => "error",
                },
                error: login
                    .problem
                    .as_ref()
                    .map(|problem| problem.message.as_str()),
            })
            .collect(),
    }
}

/// Write a path under the home directory with `~`, the way it is typed.
pub fn shorten_home(path: &str) -> String {
    let Some(home) = dirs::home_dir() else {
        return path.to_string();
    };
    let home = home.to_string_lossy();
    match path.strip_prefix(home.as_ref()) {
        Some("") => "~".to_string(),
        Some(rest) if rest.starts_with('/') => format!("~{}", rest),
        _ => path.to_string(),
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
