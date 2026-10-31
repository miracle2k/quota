//! Claude accounts that claude-swap (`cswap`) manages.
//!
//! claude-swap keeps each account's login in a store of its own and renews
//! those tokens itself. A Claude refresh token works only once, so quota never
//! touches them: it asks `cswap --list --json` for the usage claude-swap
//! measured and shows that, the same interface CodexBar reads.

use quota_core::providers::ProviderError;
use quota_core::usage::{ProviderUsage, UsageWindow};
use serde::Deserialize;
use std::path::{Path, PathBuf};
use std::time::Duration;

/// `cswap` may renew tokens and fetch usage for every account before it
/// answers, a few seconds each.
const TIMEOUT: Duration = Duration::from_secs(90);

/// One account from `cswap --list --json`.
#[derive(Debug, Clone, PartialEq)]
pub struct SwapAccount {
    pub email: String,
    /// Empty for a personal account.
    pub organization_uuid: String,
    /// The nickname set with claude-swap, if any.
    pub alias: Option<String>,
    /// The account Claude Code is signed in to right now.
    pub active: bool,
    pub result: Result<ProviderUsage, ProviderError>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ListOutput {
    schema_version: Option<u32>,
    #[serde(default)]
    accounts: Vec<Row>,
    error: Option<ErrorBody>,
}

#[derive(Deserialize)]
struct ErrorBody {
    #[serde(default)]
    message: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Row {
    #[serde(default)]
    email: String,
    #[serde(default)]
    organization_uuid: String,
    #[serde(default)]
    active: bool,
    #[serde(default)]
    usage_status: String,
    usage: Option<Usage>,
    /// The last good reading, sent when `usage` is null.
    last_good_usage: Option<Usage>,
    alias: Option<String>,
    usage_error: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Usage {
    five_hour: Option<Window>,
    seven_day: Option<Window>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Window {
    /// Percent used.
    pct: Option<f64>,
    resets_at: Option<String>,
}

/// Read the output of `cswap --list --json`.
pub fn parse_list(text: &str) -> Result<Vec<SwapAccount>, String> {
    let output: ListOutput = serde_json::from_str(text).map_err(|err| {
        format!(
            "unexpected cswap output at line {} column {}",
            err.line(),
            err.column()
        )
    })?;
    if let Some(error) = output.error {
        return Err(format!("cswap failed: {}", error.message));
    }
    if output.schema_version != Some(1) {
        return Err(format!(
            "cswap printed schema version {:?}; quota-cli reads version 1",
            output.schema_version
        ));
    }
    Ok(output.accounts.into_iter().map(account_from).collect())
}

fn account_from(row: Row) -> SwapAccount {
    let result = match (row.usage_status.as_str(), row.usage.or(row.last_good_usage)) {
        ("api_key", _) => Err(ProviderError::new(
            "This claude-swap account uses an API key, which has no usage window to report.",
            true,
        )),
        (_, Some(usage)) => Ok(usage_from(usage)),
        ("relogin_required", None) => Err(ProviderError::new(
            "claude-swap needs you to sign in to this account again.",
            false,
        )),
        (status, None) => Err(ProviderError::new(
            match row.usage_error.as_deref() {
                Some(detail) => format!("claude-swap reports {} ({})", status, detail),
                None => format!("claude-swap reports {}", status),
            },
            false,
        )),
    };
    SwapAccount {
        email: row.email,
        organization_uuid: row.organization_uuid,
        alias: row.alias.filter(|alias| !alias.trim().is_empty()),
        active: row.active,
        result,
    }
}

fn usage_from(usage: Usage) -> ProviderUsage {
    ProviderUsage {
        provider: "claude".to_string(),
        account_label: None,
        windows: vec![
            window_from("5h", usage.five_hour),
            window_from("Wk", usage.seven_day),
        ],
        note: None,
        resets: None,
    }
}

fn window_from(label: &str, window: Option<Window>) -> UsageWindow {
    let window = window.unwrap_or(Window {
        pct: None,
        resets_at: None,
    });
    UsageWindow {
        label: label.to_string(),
        remaining_percent: window
            .pct
            .filter(|used| used.is_finite())
            .map(|used| (100.0 - used.round()).clamp(0.0, 100.0) as i32),
        reset_at: window
            .resets_at
            .as_deref()
            .and_then(|text| chrono::DateTime::parse_from_rfc3339(text).ok())
            .map(|stamp| stamp.timestamp()),
    }
}

/// Run `cswap --list --json`. `None` when claude-swap is not installed.
pub async fn list() -> Option<Result<Vec<SwapAccount>, String>> {
    let program = find_cswap()?;
    Some(run(&program).await)
}

async fn run(program: &Path) -> Result<Vec<SwapAccount>, String> {
    let mut command = tokio::process::Command::new(program);
    // Never killed, not even after the timeout: cswap may be in the middle of
    // renewing a token, and stopping it between spending the old refresh token
    // and saving the new one would lose that account's login.
    command
        .args(["--list", "--json"])
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .kill_on_drop(false);
    let output = tokio::time::timeout(TIMEOUT, command.output())
        .await
        .map_err(|_| {
            format!(
                "cswap did not answer within {} seconds; it is left to finish on its own",
                TIMEOUT.as_secs()
            )
        })?
        .map_err(|err| format!("could not run cswap: {}", err))?;

    let parsed = parse_list(&String::from_utf8_lossy(&output.stdout));
    if output.status.success() {
        return parsed;
    }
    // A failing run prints an error envelope, which says more than the exit
    // status does.
    match parsed {
        Err(message) if message.starts_with("cswap failed") => Err(message),
        _ => Err(format!("cswap exited with {}", output.status)),
    }
}

/// `cswap` on `PATH`, or in `~/.local/bin`, where `uv tool install` and
/// `pipx` put it but which a non-login shell may leave off `PATH`.
fn find_cswap() -> Option<PathBuf> {
    let name = if cfg!(windows) { "cswap.exe" } else { "cswap" };
    let on_path = std::env::var_os("PATH").and_then(|path| {
        std::env::split_paths(&path)
            .map(|dir| dir.join(name))
            .find(|candidate| is_executable(candidate))
    });
    on_path.or_else(|| {
        dirs::home_dir()
            .map(|home| home.join(".local").join("bin").join(name))
            .filter(|candidate| is_executable(candidate))
    })
}

fn is_executable(path: &Path) -> bool {
    let Ok(meta) = std::fs::metadata(path) else {
        return false;
    };
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        meta.is_file() && meta.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    {
        meta.is_file()
    }
}
