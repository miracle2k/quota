//! Renew a Claude Code login's access token the way Claude Code itself does,
//! so a login nobody has used for a while still reports usage.
//!
//! Claude Code expects several processes to share one login. Each renews only
//! while holding a lock in the config directory, reads the stored token again
//! once it has the lock, and writes its result only if the stored refresh token
//! is still the one it sent. This follows the same steps, so a Claude Code
//! session that renews later finds the new token and adopts it instead of
//! sending a refresh token that is no longer valid.

use super::local::{ClaudeLogin, CredentialStore, LocalCredentialError};
use serde::Deserialize;
use serde_json::Value;
use std::path::{Path, PathBuf};
use std::time::Duration;

/// Claude Code's OAuth client and token endpoint.
pub const TOKEN_URL: &str = "https://platform.claude.com/v1/oauth/token";
pub const CLIENT_ID: &str = "9d1c250a-e61b-44d9-88ed-5944d1962f5e";

/// Claude Code renews a token once it is within five minutes of expiry.
pub const EXPIRY_MARGIN_MS: i64 = 5 * 60 * 1000;

/// The scopes Claude Code asks for when the stored login names none.
const DEFAULT_SCOPES: &[&str] = &[
    "user:profile",
    "user:inference",
    "user:sessions:claude_code",
    "user:mcp_servers",
    "user:file_upload",
    "user:plugins",
];

/// The lock Claude Code takes inside the config directory, plus the older one
/// beside it that earlier versions take. Both are directories created with
/// `mkdir`, the way the `proper-lockfile` package makes them.
const LOCK_NAME: &str = ".oauth_refresh.lock";
/// `proper-lockfile` treats a lock not touched for this long as abandoned.
const LOCK_STALE: Duration = Duration::from_secs(60);
/// Claude Code tries five times, one to two seconds apart.
const LOCK_ATTEMPTS: u32 = 6;
/// Claude Code's own limit. It stays under the lock's stale limit, so no other
/// process takes the lock over while the request is still out.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RenewOutcome {
    /// The stored token is still good. Nothing was sent or written.
    Fresh,
    /// This call renewed the token and saved the result.
    Renewed,
    /// Another program renewed it first; read the store again to use it.
    RenewedElsewhere,
}

#[derive(Debug)]
pub enum RenewError {
    Credentials(LocalCredentialError),
    /// The login carries no refresh token, so only signing in again helps.
    NotRenewable,
    /// The server refused the refresh token: it expired or was revoked.
    Rejected,
    /// Another program held the lock the whole time.
    Busy,
    Request(String),
    Store(String),
}

impl std::fmt::Display for RenewError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Credentials(inner) => write!(formatter, "{}", inner),
            Self::NotRenewable => write!(
                formatter,
                "The Claude Code login has expired and has no refresh token. Sign in again."
            ),
            Self::Rejected => write!(
                formatter,
                "Claude refused to renew the saved login. Sign in again."
            ),
            Self::Busy => write!(
                formatter,
                "Another program is renewing this Claude Code login. Try again in a moment."
            ),
            Self::Request(detail) => write!(formatter, "Claude token renewal failed: {}", detail),
            Self::Store(detail) => {
                write!(
                    formatter,
                    "Could not save the renewed Claude token: {}",
                    detail
                )
            }
        }
    }
}

impl RenewError {
    /// Signing in again is the only fix.
    pub fn needs_sign_in(&self) -> bool {
        matches!(self, Self::NotRenewable | Self::Rejected)
    }
}

/// Renew the login's token when it expires within five minutes, or when
/// `rejected_token` is the access token still stored, which means the usage
/// API turned it down.
pub async fn renew(
    login: &ClaudeLogin,
    rejected_token: Option<&str>,
) -> Result<RenewOutcome, RenewError> {
    renew_with(login, rejected_token, TOKEN_URL, now_ms).await
}

/// [`renew`] against another token endpoint and clock, for tests.
pub async fn renew_with(
    login: &ClaudeLogin,
    rejected_token: Option<&str>,
    token_url: &str,
    now: fn() -> i64,
) -> Result<RenewOutcome, RenewError> {
    let (_, raw) = login.read_stored().map_err(RenewError::Credentials)?;
    let seen = StoredTokens::parse(&raw)?;
    if !seen.needs_renewal(rejected_token, now()) {
        return Ok(RenewOutcome::Fresh);
    }
    if seen.refresh_token.is_none() {
        return Err(RenewError::NotRenewable);
    }

    let _locks = lock(login.config_dir()).await?;

    // Another process may have renewed while this one waited for the lock.
    // From here on only this store is read and written.
    let (store, raw) = login.read_stored().map_err(RenewError::Credentials)?;
    let locked = StoredTokens::parse(&raw)?;
    if locked.access_token != seen.access_token {
        return Ok(RenewOutcome::RenewedElsewhere);
    }
    if !locked.needs_renewal(rejected_token, now()) {
        return Ok(RenewOutcome::Fresh);
    }
    let refresh_token = locked.refresh_token.ok_or(RenewError::NotRenewable)?;
    // The refresh token is spent once it is sent. Anything that could stop the
    // result from being saved is checked before that.
    if !store.fits(raw.len() + RESPONSE_GROWTH) {
        return Err(RenewError::Store(
            "the login is too long to save without putting it on a command line. \
             Run Claude Code to renew it."
                .to_string(),
        ));
    }

    let response = request_tokens(token_url, &refresh_token, &locked.scopes).await?;
    let mut renewed = parse_document(&raw)?;
    apply_token_response(&mut renewed, &response, &refresh_token, now())?;

    // Write only if nobody replaced the refresh token that was just spent.
    // Their write is kept, along with anything else they changed.
    let current = match store.read() {
        Ok(current) => current,
        Err(err) => return Err(keep_renewed(login, &renewed, err.to_string())),
    };
    let mut document = parse_document(&current)?;
    let stored_refresh = document
        .get("claudeAiOauth")
        .and_then(|oauth| oauth.get("refreshToken"))
        .and_then(Value::as_str);
    if stored_refresh != Some(refresh_token.as_str()) {
        return Ok(RenewOutcome::RenewedElsewhere);
    }

    apply_token_response(&mut document, &response, &refresh_token, now())?;
    let serialized =
        serde_json::to_string(&document).map_err(|err| RenewError::Store(err.to_string()))?;
    write_store(&store, &serialized).map_err(|err| keep_renewed(login, &document, err))?;
    Ok(RenewOutcome::Renewed)
}

/// Room for a token response to grow the stored login: new tokens can be a
/// little longer, and a refresh-token expiry may be added.
const RESPONSE_GROWTH: usize = 512;

fn parse_document(raw: &str) -> Result<Value, RenewError> {
    serde_json::from_str(raw.trim())
        .map_err(|_| RenewError::Store("the stored login is not JSON".to_string()))
}

/// The token was renewed but could not be saved where it belongs. The old
/// refresh token is spent by now, so keep the new login in a file beside the
/// config directory's credentials rather than drop it.
fn keep_renewed(login: &ClaudeLogin, document: &Value, failure: String) -> RenewError {
    let saved = serde_json::to_string(document)
        .ok()
        .and_then(|text| crate::fs_util::save_recovery_copy(&login.credentials_path(), &text));
    RenewError::Store(match saved {
        Some(path) => format!(
            "{}. The renewed login is in {}; until it is put back, sign in again.",
            failure,
            path.display()
        ),
        None => format!("{}. Sign in again.", failure),
    })
}

/// The token endpoint's answer. Only the fields Claude Code saves are kept.
#[derive(Deserialize)]
pub struct TokenResponse {
    pub access_token: String,
    pub refresh_token: Option<String>,
    pub expires_in: Option<i64>,
    pub refresh_token_expires_in: Option<i64>,
    pub scope: Option<String>,
}

/// Fold a token response into the stored login, keeping every field Claude
/// Code wrote that the response does not replace. A response without a new
/// refresh token keeps the one that was sent, as Claude Code does.
pub fn apply_token_response(
    document: &mut Value,
    response: &TokenResponse,
    sent_refresh_token: &str,
    now_ms: i64,
) -> Result<(), RenewError> {
    let oauth = document
        .get_mut("claudeAiOauth")
        .and_then(Value::as_object_mut)
        .ok_or_else(|| RenewError::Store("no claudeAiOauth block".to_string()))?;

    oauth.insert("accessToken".into(), response.access_token.clone().into());
    oauth.insert(
        "refreshToken".into(),
        response
            .refresh_token
            .clone()
            .unwrap_or_else(|| sent_refresh_token.to_string())
            .into(),
    );
    if let Some(seconds) = response.expires_in {
        oauth.insert(
            "expiresAt".into(),
            now_ms.saturating_add(seconds.saturating_mul(1000)).into(),
        );
    }
    if let Some(seconds) = response.refresh_token_expires_in {
        oauth.insert(
            "refreshTokenExpiresAt".into(),
            now_ms.saturating_add(seconds.saturating_mul(1000)).into(),
        );
    }
    let scopes: Vec<Value> = response
        .scope
        .as_deref()
        .unwrap_or_default()
        .split_whitespace()
        .map(|scope| Value::from(scope.to_string()))
        .collect();
    if !scopes.is_empty() {
        oauth.insert("scopes".into(), Value::Array(scopes));
    }
    Ok(())
}

struct StoredTokens {
    access_token: String,
    refresh_token: Option<String>,
    expires_at: Option<i64>,
    scopes: Vec<String>,
}

impl StoredTokens {
    fn parse(raw: &str) -> Result<Self, RenewError> {
        #[derive(Deserialize)]
        struct Document {
            #[serde(rename = "claudeAiOauth")]
            oauth: Option<Oauth>,
        }
        #[derive(Deserialize)]
        struct Oauth {
            #[serde(rename = "accessToken")]
            access_token: Option<String>,
            #[serde(rename = "refreshToken")]
            refresh_token: Option<String>,
            #[serde(rename = "expiresAt")]
            expires_at: Option<i64>,
            #[serde(default)]
            scopes: Vec<String>,
        }

        let oauth = serde_json::from_str::<Document>(raw.trim())
            .ok()
            .and_then(|document| document.oauth)
            .ok_or_else(|| {
                RenewError::Credentials(LocalCredentialError::Malformed(
                    "no claudeAiOauth block".to_string(),
                ))
            })?;
        Ok(Self {
            access_token: oauth.access_token.unwrap_or_default(),
            refresh_token: oauth.refresh_token.filter(|token| !token.is_empty()),
            expires_at: oauth.expires_at,
            scopes: oauth.scopes,
        })
    }

    fn needs_renewal(&self, rejected_token: Option<&str>, now_ms: i64) -> bool {
        if rejected_token.is_some_and(|token| token == self.access_token) {
            return true;
        }
        match self.expires_at {
            Some(expires_at) => now_ms + EXPIRY_MARGIN_MS >= expires_at,
            None => false,
        }
    }
}

async fn request_tokens(
    token_url: &str,
    refresh_token: &str,
    scopes: &[String],
) -> Result<TokenResponse, RenewError> {
    let scope = if scopes.is_empty() {
        DEFAULT_SCOPES.join(" ")
    } else {
        scopes.join(" ")
    };

    let client = reqwest::Client::builder()
        .timeout(REQUEST_TIMEOUT)
        .build()
        .map_err(|err| RenewError::Request(err.to_string()))?;
    let response = client
        .post(token_url)
        .header(reqwest::header::USER_AGENT, super::local_usage::USER_AGENT)
        .json(&serde_json::json!({
            "grant_type": "refresh_token",
            "refresh_token": refresh_token,
            "client_id": CLIENT_ID,
            "scope": scope,
        }))
        .send()
        .await
        .map_err(|err| RenewError::Request(crate::http_error::describe(err)))?;

    let status = response.status().as_u16();
    let body = response
        .text()
        .await
        .map_err(|err| RenewError::Request(crate::http_error::describe(err)))?;

    if !(200..300).contains(&status) {
        return Err(classify_failure(status, &body));
    }
    serde_json::from_str(&body).map_err(|err| {
        RenewError::Request(format!(
            "unexpected response at line {} column {}",
            err.line(),
            err.column()
        ))
    })
}

/// Sort a failed renewal into "sign in again" and everything else. Only the
/// OAuth error code is ever quoted, never the body.
pub fn classify_failure(status: u16, body: &str) -> RenewError {
    let code = serde_json::from_str::<Value>(body).ok().and_then(|value| {
        value
            .get("error")
            .and_then(Value::as_str)
            .map(str::to_string)
    });
    let refused =
        matches!(status, 401 | 403) || (status == 400 && code.as_deref() == Some("invalid_grant"));
    if refused {
        RenewError::Rejected
    } else {
        RenewError::Request(match code {
            Some(code) => format!("status={} error={}", status, code),
            None => format!("status={}", status),
        })
    }
}

fn write_store(store: &CredentialStore, serialized: &str) -> Result<(), String> {
    match store {
        CredentialStore::File(path) => crate::fs_util::write_private_atomic(path, serialized),
        #[cfg(target_os = "macos")]
        CredentialStore::Keychain { service, account } => {
            super::local::write_keychain_item(service, account.as_deref(), serialized)
        }
        #[cfg(not(target_os = "macos"))]
        CredentialStore::Keychain { .. } => Err("no Keychain on this system".to_string()),
    }
}

/// Holds Claude Code's renewal locks until dropped.
struct Locks {
    _current: DirLock,
    _legacy: Option<DirLock>,
}

/// Take both locks in the order Claude Code takes them, so the two can never
/// each hold one and wait for the other.
async fn lock(config_dir: &Path) -> Result<Locks, RenewError> {
    std::fs::create_dir_all(config_dir).map_err(|err| RenewError::Store(err.to_string()))?;
    let current_path = config_dir.join(LOCK_NAME);
    let legacy_path = legacy_lock_path(config_dir);

    for attempt in 0..LOCK_ATTEMPTS {
        if attempt > 0 {
            let jitter = rand::random::<u64>() % 1000;
            tokio::time::sleep(Duration::from_millis(1000 + jitter)).await;
        }
        let current = match DirLock::try_take(&current_path) {
            Ok(Some(lock)) => lock,
            Ok(None) => continue,
            Err(err) => return Err(RenewError::Store(err.to_string())),
        };
        match DirLock::try_take(&legacy_path) {
            Ok(Some(legacy)) => {
                return Ok(Locks {
                    _current: current,
                    _legacy: Some(legacy),
                })
            }
            // Claude Code gives up the new lock and retries when only the
            // old one is held.
            Ok(None) => continue,
            // It carries on without the old lock when that cannot be made
            // at all, such as in a read-only parent directory.
            Err(_) => {
                return Ok(Locks {
                    _current: current,
                    _legacy: None,
                })
            }
        }
    }
    Err(RenewError::Busy)
}

/// `<config dir>.lock`, next to the directory, after resolving symlinks.
fn legacy_lock_path(config_dir: &Path) -> PathBuf {
    let resolved = std::fs::canonicalize(config_dir).unwrap_or_else(|_| config_dir.to_path_buf());
    let mut name = resolved.into_os_string();
    name.push(".lock");
    PathBuf::from(name)
}

struct DirLock {
    path: PathBuf,
}

impl DirLock {
    /// Create the lock directory. `None` while someone else holds it. A lock
    /// left behind longer than the stale limit is removed and taken over, as
    /// `proper-lockfile` does.
    fn try_take(path: &Path) -> std::io::Result<Option<Self>> {
        match std::fs::create_dir(path) {
            Ok(()) => {
                return Ok(Some(Self {
                    path: path.to_path_buf(),
                }))
            }
            Err(err) if err.kind() != std::io::ErrorKind::AlreadyExists => return Err(err),
            Err(_) => {}
        }

        let stale = std::fs::metadata(path)
            .and_then(|meta| meta.modified())
            .map(|modified| modified.elapsed().unwrap_or_default() > LOCK_STALE)
            .unwrap_or(false);
        if !stale {
            return Ok(None);
        }
        let _ = std::fs::remove_dir(path);
        match std::fs::create_dir(path) {
            Ok(()) => Ok(Some(Self {
                path: path.to_path_buf(),
            })),
            Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => Ok(None),
            Err(err) => Err(err),
        }
    }
}

impl Drop for DirLock {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir(&self.path);
    }
}

fn now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}
