//! Read-only access to the logins the pi coding agent keeps in
//! `~/.pi/agent/auth.json` (`PI_CODING_AGENT_DIR` moves the directory).
//!
//! pi stores one entry per provider, either an API key or an OAuth login:
//!
//! ```json
//! {
//!   "zai": { "type": "api_key", "key": "..." },
//!   "openai-codex": { "type": "oauth", "access": "...", "refresh": "...",
//!                     "expires": 1790000000000, "accountId": "..." }
//! }
//! ```
//!
//! Nothing here writes. pi renews its own logins under a lock and rewrites the
//! file in place, so a reader that catches it half-written simply reads again.

use serde_json::{Map, Value};
use std::fmt;
use std::path::PathBuf;

/// One login from pi's `auth.json`.
#[derive(Clone)]
pub enum PiCredential {
    ApiKey(String),
    OAuth(PiOAuth),
}

#[derive(Clone)]
pub struct PiOAuth {
    pub access: String,
    pub refresh: Option<String>,
    /// When the access token expires, in milliseconds since the epoch. pi
    /// already subtracts a few minutes for some providers.
    pub expires_ms: Option<i64>,
    /// Every other field pi keeps for this provider, such as `accountId`,
    /// `projectId` or `email`.
    pub extra: Map<String, Value>,
}

impl PiOAuth {
    /// A text field from the provider-specific extras.
    pub fn extra_text(&self, key: &str) -> Option<String> {
        text(self.extra.get(key))
    }

    pub fn is_expired_at_ms(&self, now_ms: i64, margin_ms: i64) -> bool {
        self.expires_ms
            .is_some_and(|expires| expires <= now_ms + margin_ms)
    }
}

/// Deliberately hand-written so no secret can reach the Debug output.
impl fmt::Debug for PiCredential {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ApiKey(_) => formatter.write_str("PiCredential::ApiKey(<redacted>)"),
            Self::OAuth(oauth) => formatter
                .debug_struct("PiCredential::OAuth")
                .field("expires_ms", &oauth.expires_ms)
                .field("has_refresh", &oauth.refresh.is_some())
                .field("extra_keys", &oauth.extra.keys().collect::<Vec<_>>())
                .finish(),
        }
    }
}

#[derive(Debug)]
pub enum PiError {
    NotFound,
    Unreadable(String),
    Malformed(String),
    /// The key is the output of a shell command, which only pi runs.
    CommandKey,
}

impl fmt::Display for PiError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotFound => write!(formatter, "pi has no login for this provider."),
            Self::Unreadable(detail) => write!(formatter, "Could not read pi's logins: {}", detail),
            Self::Malformed(detail) => {
                write!(
                    formatter,
                    "pi's auth.json is not in the expected shape: {}",
                    detail
                )
            }
            Self::CommandKey => write!(
                formatter,
                "pi gets this API key by running a command, which quota does not do."
            ),
        }
    }
}

/// pi's agent directory: `PI_CODING_AGENT_DIR`, else `~/.pi/agent`.
pub fn agent_dir() -> Option<PathBuf> {
    match std::env::var("PI_CODING_AGENT_DIR") {
        Ok(dir) if !dir.trim().is_empty() => Some(expand_home(dir.trim())),
        _ => dirs::home_dir().map(|home| home.join(".pi").join("agent")),
    }
}

/// All of pi's logins, keyed by provider. A missing file means none.
pub fn read_auth() -> Result<Map<String, Value>, PiError> {
    let path = agent_dir()
        .ok_or_else(|| PiError::Unreadable("no home directory".to_string()))?
        .join("auth.json");
    read_json_object(&path)
}

/// The login pi keeps for `provider`.
pub fn login(auth: &Map<String, Value>, provider: &str) -> Result<PiCredential, PiError> {
    let entry = auth
        .get(provider)
        .and_then(Value::as_object)
        .ok_or(PiError::NotFound)?;
    match entry.get("type").and_then(Value::as_str) {
        Some("api_key") => api_key(entry).map(PiCredential::ApiKey),
        Some("oauth") => oauth(entry)
            .map(PiCredential::OAuth)
            .ok_or_else(|| PiError::Malformed(format!("{}: no access token", provider))),
        other => Err(PiError::Malformed(format!(
            "{}: unknown login type {:?}",
            provider, other
        ))),
    }
}

/// Every Antigravity account the pi-antigravity extension has linked, from
/// `antigravity-accounts.json` next to `auth.json`. The active one is also in
/// `auth.json`; the others are only here.
pub fn antigravity_accounts() -> Result<Vec<PiOAuth>, PiError> {
    let path = agent_dir()
        .ok_or_else(|| PiError::Unreadable("no home directory".to_string()))?
        .join("antigravity-accounts.json");
    let root = read_json_object(&path)?;
    Ok(parse_antigravity_accounts(&root))
}

pub fn parse_antigravity_accounts(root: &Map<String, Value>) -> Vec<PiOAuth> {
    root.get("accounts")
        .and_then(Value::as_object)
        .map(|accounts| {
            accounts
                .values()
                .filter_map(Value::as_object)
                .filter_map(oauth)
                .collect()
        })
        .unwrap_or_default()
}

/// Parse the text of a pi JSON file into its top-level object.
pub fn parse_object(text: &str) -> Result<Map<String, Value>, PiError> {
    // Only position information from a serde error is kept: its message can
    // quote the input, which here is full of secrets.
    match serde_json::from_str::<Value>(text.trim_start_matches('\u{feff}')) {
        Ok(Value::Object(map)) => Ok(map),
        Ok(_) => Err(PiError::Malformed("not a JSON object".to_string())),
        Err(err) => Err(PiError::Malformed(format!(
            "JSON error at line {} column {}",
            err.line(),
            err.column()
        ))),
    }
}

fn read_json_object(path: &std::path::Path) -> Result<Map<String, Value>, PiError> {
    // pi writes in place, so a read can land mid-write. That looks like broken
    // JSON for a moment, then not.
    let mut attempts = 0;
    loop {
        let text = match std::fs::read_to_string(path) {
            Ok(text) => text,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(Map::new()),
            Err(err) => return Err(PiError::Unreadable(err.to_string())),
        };
        match parse_object(&text) {
            Err(PiError::Malformed(_)) if attempts < 3 => {
                attempts += 1;
                std::thread::sleep(std::time::Duration::from_millis(100));
            }
            result => return result,
        }
    }
}

/// An API key as pi resolves it: the text itself, or `$VAR` / `${VAR}` read
/// from the entry's `env` map or the environment. `$$` and `$!` stand for a
/// literal `$` and `!`. A key that starts with `!` is a shell command.
fn api_key(entry: &Map<String, Value>) -> Result<String, PiError> {
    let raw = text(entry.get("key")).ok_or(PiError::NotFound)?;
    if let Some(rest) = raw.strip_prefix("$$") {
        return Ok(format!("${}", rest));
    }
    if let Some(rest) = raw.strip_prefix("$!") {
        return Ok(format!("!{}", rest));
    }
    if raw.starts_with('!') {
        return Err(PiError::CommandKey);
    }
    let variable = raw
        .strip_prefix("${")
        .and_then(|rest| rest.strip_suffix('}'))
        .or_else(|| raw.strip_prefix('$'));
    match variable {
        Some(name) => entry
            .get("env")
            .and_then(|env| env.get(name))
            .and_then(Value::as_str)
            .map(str::to_string)
            .or_else(|| std::env::var(name).ok())
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty())
            .ok_or(PiError::NotFound),
        None => Ok(raw),
    }
}

fn oauth(entry: &Map<String, Value>) -> Option<PiOAuth> {
    let access = text(entry.get("access"))?;
    let mut extra = entry.clone();
    for known in ["type", "access", "refresh", "expires"] {
        extra.remove(known);
    }
    Some(PiOAuth {
        access,
        refresh: text(entry.get("refresh")),
        expires_ms: entry
            .get("expires")
            .and_then(Value::as_f64)
            .map(|ms| ms as i64),
        extra,
    })
}

fn text(value: Option<&Value>) -> Option<String> {
    value
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}

fn expand_home(path: &str) -> PathBuf {
    match (path.strip_prefix("~/"), dirs::home_dir()) {
        (Some(rest), Some(home)) => home.join(rest),
        _ if path == "~" => dirs::home_dir().unwrap_or_else(|| PathBuf::from(path)),
        _ => PathBuf::from(path),
    }
}
