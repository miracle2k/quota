//! Renew a Codex login's access token the way the Codex CLI does, so a login
//! nobody has used for a while still reports usage.
//!
//! The Codex CLI expects other programs to rewrite `auth.json`: before it
//! renews a token it reads the file again, and after an HTTP 401 it reloads the
//! file before anything else. So a token renewed here is adopted rather than
//! fought over, as long as the file keeps its shape and is written under the
//! same locks the CLI takes.

use chrono::{DateTime, Duration as ChronoDuration, SecondsFormat, Utc};
use serde_json::Value;
use std::fs::File;
use std::path::{Path, PathBuf};
use std::time::Duration;

use super::local::credentials_path_in;

/// The Codex CLI's OAuth issuer and client, used when the login does not
/// record its own.
pub const ISSUER: &str = "https://auth.openai.com";
pub const CLIENT_ID: &str = "app_EMoamEEZ73f0CkXaXp7hrann";

/// The CLI renews a token five minutes before it expires, or, when the token
/// carries no expiry, once the last renewal is eight days old.
const EXPIRY_MARGIN_MINUTES: i64 = 5;
const REFRESH_INTERVAL_DAYS: i64 = 8;
/// The CLI's own limit. Giving up early risks the server rotating the token
/// in an answer nobody reads.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(45);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RenewOutcome {
    /// The stored token is still good. Nothing was sent or written.
    Fresh,
    /// This call renewed the token and saved the result.
    Renewed,
    /// Another program renewed it first; read the file again to use it.
    RenewedElsewhere,
}

#[derive(Debug)]
pub enum RenewError {
    Unreadable(String),
    /// The login carries no refresh token, so only signing in again helps.
    NotRenewable,
    /// The server refused the refresh token: it expired, was revoked, or was
    /// already used.
    Rejected,
    Request(String),
    Store(String),
}

impl std::fmt::Display for RenewError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Unreadable(detail) => {
                write!(formatter, "Could not read the Codex login: {}", detail)
            }
            Self::NotRenewable => write!(
                formatter,
                "The Codex login has expired and has no refresh token. Sign in again."
            ),
            Self::Rejected => write!(
                formatter,
                "OpenAI refused to renew the saved Codex login. Sign in again."
            ),
            Self::Request(detail) => write!(formatter, "Codex token renewal failed: {}", detail),
            Self::Store(detail) => {
                write!(
                    formatter,
                    "Could not save the renewed Codex token: {}",
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

/// Renew the token in `codex_home/auth.json` when it is about to expire, or
/// when `rejected_token` is the access token still stored, which means the
/// usage API turned it down. A login without OAuth tokens, such as an API key,
/// is left alone.
pub async fn renew(
    codex_home: &Path,
    rejected_token: Option<&str>,
) -> Result<RenewOutcome, RenewError> {
    renew_with(codex_home, rejected_token, None, Utc::now).await
}

/// [`renew`] against another token endpoint and clock, for tests.
pub async fn renew_with(
    codex_home: &Path,
    rejected_token: Option<&str>,
    token_url: Option<&str>,
    now: fn() -> DateTime<Utc>,
) -> Result<RenewOutcome, RenewError> {
    let path = resolved_auth_path(codex_home);
    let seen = read_document(&path)?;
    let Some(seen_tokens) = Tokens::from(&seen) else {
        return Ok(RenewOutcome::Fresh);
    };
    if !seen_tokens.needs_renewal(rejected_token, now()) {
        return Ok(RenewOutcome::Fresh);
    }

    // One quota process renews a login at a time, so two of them never spend
    // the same refresh token. The CLI does not take this lock; a renewal of
    // its own is caught by the comparison before writing.
    let _renewing = quota_lock(&path).await?;
    let document = read_document(&path)?;
    let Some(tokens) = Tokens::from(&document) else {
        return Ok(RenewOutcome::RenewedElsewhere);
    };
    if tokens.access_token != seen_tokens.access_token {
        return Ok(RenewOutcome::RenewedElsewhere);
    }
    if !tokens.needs_renewal(rejected_token, now()) {
        return Ok(RenewOutcome::Fresh);
    }
    let refresh_token = tokens
        .refresh_token
        .clone()
        .ok_or(RenewError::NotRenewable)?;

    // The refresh token is spent once it is sent, so anything that could stop
    // the result from being saved is checked before that: the lock files open,
    // which also shows both folders take new files.
    let lock_files = LockFiles::open(codex_home, &path)?;

    let (default_url, client_id) = tokens.endpoint();
    let url = token_url.map(str::to_string).unwrap_or(default_url);
    let response = match request_tokens(&url, &client_id, &refresh_token).await {
        Ok(response) => response,
        Err(RenewError::Rejected) => {
            // The CLI may have spent the same refresh token a moment earlier
            // and still be saving its result, which is as good as this one's.
            for wait in [0, 1000] {
                tokio::time::sleep(Duration::from_millis(wait)).await;
                if stored_refresh_token(&path).as_deref() != Some(&refresh_token) {
                    return Ok(RenewOutcome::RenewedElsewhere);
                }
            }
            return Err(RenewError::Rejected);
        }
        Err(err) => return Err(err),
    };
    let mut renewed = document;
    apply_token_response(&mut renewed, &response, now())?;

    // Same locks as the CLI, held only around read, compare, and write.
    let _locks = match lock_files.lock().await {
        Ok(locks) => locks,
        Err(failure) => return Err(keep_renewed(&path, &renewed, failure)),
    };
    let mut current = match read_document(&path) {
        Ok(current) => current,
        Err(err) => return Err(keep_renewed(&path, &renewed, err.to_string())),
    };
    if stored_refresh_token_in(&current).as_deref() != Some(&refresh_token) {
        return Ok(RenewOutcome::RenewedElsewhere);
    }
    apply_token_response(&mut current, &response, now())?;
    let text =
        serde_json::to_string_pretty(&current).map_err(|err| RenewError::Store(err.to_string()))?;
    crate::fs_util::write_private_atomic(&path, &text)
        .map_err(|failure| keep_renewed(&path, &current, failure))?;
    Ok(RenewOutcome::Renewed)
}

/// The token was renewed but could not be saved in `auth.json`. The old
/// refresh token is spent by now, so keep the new login beside it rather than
/// drop it.
fn keep_renewed(auth_path: &Path, document: &Value, failure: String) -> RenewError {
    let saved = serde_json::to_string_pretty(document)
        .ok()
        .and_then(|text| crate::fs_util::save_recovery_copy(auth_path, &text));
    RenewError::Store(match saved {
        Some(copy) => format!(
            "{}. The renewed login is in {}; move it over {} to keep it, or sign in again.",
            failure,
            copy.display(),
            auth_path.display()
        ),
        None => format!("{}. Sign in again.", failure),
    })
}

/// Fold a token response into `auth.json`: new tokens where the response has
/// them, and the time of this renewal. Everything else, including the account
/// id and fields this reader does not know, stays as it was.
pub fn apply_token_response(
    document: &mut Value,
    response: &Value,
    now: DateTime<Utc>,
) -> Result<(), RenewError> {
    let tokens = document
        .get_mut("tokens")
        .and_then(Value::as_object_mut)
        .ok_or_else(|| RenewError::Store("no tokens block".to_string()))?;
    for key in ["id_token", "access_token", "refresh_token"] {
        if let Some(value) = response
            .get(key)
            .and_then(Value::as_str)
            .filter(|value| !value.is_empty())
        {
            tokens.insert(key.to_string(), Value::from(value));
        }
    }
    if let Some(root) = document.as_object_mut() {
        root.insert(
            "last_refresh".to_string(),
            Value::from(now.to_rfc3339_opts(SecondsFormat::Micros, true)),
        );
    }
    Ok(())
}

/// Sort a failed renewal into "sign in again" and everything else, using the
/// same codes the CLI treats as final. Only the code is quoted, never the body.
pub fn classify_failure(status: u16, body: &str) -> RenewError {
    let parsed = serde_json::from_str::<Value>(body).ok();
    let code = parsed.as_ref().and_then(|value| {
        value
            .get("error")
            .and_then(|error| {
                error
                    .as_str()
                    .or_else(|| error.get("code").and_then(Value::as_str))
            })
            .or_else(|| value.get("code").and_then(Value::as_str))
            .map(str::to_string)
    });
    let final_code = matches!(
        code.as_deref(),
        Some("refresh_token_expired" | "refresh_token_reused" | "refresh_token_invalidated")
    );
    if status == 401 || final_code || (status == 400 && code.as_deref() == Some("invalid_grant")) {
        RenewError::Rejected
    } else {
        RenewError::Request(match code {
            Some(code) => format!("status={} error={}", status, code),
            None => format!("status={}", status),
        })
    }
}

struct Tokens {
    access_token: String,
    refresh_token: Option<String>,
    last_refresh: Option<DateTime<Utc>>,
    issuer: Option<String>,
    client_id: Option<String>,
}

impl Tokens {
    fn from(document: &Value) -> Option<Self> {
        let tokens = document.get("tokens")?.as_object()?;
        let text = |value: Option<&Value>| {
            value
                .and_then(Value::as_str)
                .filter(|text| !text.is_empty())
                .map(str::to_string)
        };
        let provenance = tokens.get("oauth_provenance");
        Some(Self {
            access_token: text(tokens.get("access_token"))?,
            refresh_token: text(tokens.get("refresh_token")),
            last_refresh: text(document.get("last_refresh"))
                .and_then(|stamp| DateTime::parse_from_rfc3339(&stamp).ok())
                .map(|stamp| stamp.with_timezone(&Utc)),
            issuer: text(provenance.and_then(|value| value.get("issuer"))),
            client_id: text(provenance.and_then(|value| value.get("client_id"))),
        })
    }

    fn needs_renewal(&self, rejected_token: Option<&str>, now: DateTime<Utc>) -> bool {
        if rejected_token.is_some_and(|token| token == self.access_token) {
            return true;
        }
        match super::local::jwt_expiry(&self.access_token) {
            Some(expires_at) => {
                expires_at.timestamp()
                    <= (now + ChronoDuration::minutes(EXPIRY_MARGIN_MINUTES)).timestamp()
            }
            None => match self.last_refresh {
                Some(last) => last < now - ChronoDuration::days(REFRESH_INTERVAL_DAYS),
                None => false,
            },
        }
    }

    /// The token endpoint and client of the authority that issued this
    /// refresh token, which the login records since recent CLI versions.
    fn endpoint(&self) -> (String, String) {
        let issuer = self
            .issuer
            .as_deref()
            .unwrap_or(ISSUER)
            .trim_end_matches('/');
        (
            format!("{}/oauth/token", issuer),
            self.client_id
                .clone()
                .unwrap_or_else(|| CLIENT_ID.to_string()),
        )
    }
}

async fn request_tokens(
    url: &str,
    client_id: &str,
    refresh_token: &str,
) -> Result<Value, RenewError> {
    let client = reqwest::Client::builder()
        .timeout(REQUEST_TIMEOUT)
        .build()
        .map_err(|err| RenewError::Request(err.to_string()))?;
    let response = client
        .post(url)
        .header(reqwest::header::USER_AGENT, "quota")
        .json(&serde_json::json!({
            "client_id": client_id,
            "grant_type": "refresh_token",
            "refresh_token": refresh_token,
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
    let value: Value = serde_json::from_str(&body).map_err(|err| {
        RenewError::Request(format!(
            "unexpected response at line {} column {}",
            err.line(),
            err.column()
        ))
    })?;
    if value.get("access_token").and_then(Value::as_str).is_none() {
        return Err(RenewError::Request(
            "the response carried no access token".to_string(),
        ));
    }
    Ok(value)
}

/// `auth.json`, with symlinks resolved so the file the link points at is the
/// one rewritten, as the CLI does.
fn resolved_auth_path(codex_home: &Path) -> PathBuf {
    let path = credentials_path_in(codex_home);
    std::fs::canonicalize(&path).unwrap_or(path)
}

fn read_document(path: &Path) -> Result<Value, RenewError> {
    let text =
        std::fs::read_to_string(path).map_err(|err| RenewError::Unreadable(err.to_string()))?;
    serde_json::from_str(&text).map_err(|err| {
        RenewError::Unreadable(format!(
            "JSON error at line {} column {}",
            err.line(),
            err.column()
        ))
    })
}

fn stored_refresh_token(path: &Path) -> Option<String> {
    stored_refresh_token_in(&read_document(path).ok()?)
}

fn stored_refresh_token_in(document: &Value) -> Option<String> {
    document
        .get("tokens")?
        .get("refresh_token")?
        .as_str()
        .map(str::to_string)
}

/// How long to wait for a lock another process holds. The CLI holds its
/// locks only while it writes; another quota process holds its own for up to
/// one token request.
const CLI_LOCK_WAIT: Duration = Duration::from_secs(10);
const QUOTA_LOCK_WAIT: Duration = Duration::from_secs(60);

/// `auth.lock` in the Codex home and `auth.json.lock` beside the file: the two
/// locks the CLI holds while it writes.
struct LockFiles {
    home: File,
    file: File,
}

impl LockFiles {
    fn open(codex_home: &Path, auth_path: &Path) -> Result<Self, RenewError> {
        Ok(Self {
            home: open_lock_file(&codex_home.join("auth.lock")).map_err(RenewError::Store)?,
            file: open_lock_file(&auth_path.with_extension("json.lock"))
                .map_err(RenewError::Store)?,
        })
    }

    /// Lock both, in the CLI's order. They are released when dropped.
    async fn lock(self) -> Result<Self, String> {
        lock_within(&self.home, CLI_LOCK_WAIT).await?;
        lock_within(&self.file, CLI_LOCK_WAIT).await?;
        Ok(self)
    }
}

/// quota's own lock for renewing one login, in the temporary directory so
/// nothing is added to the Codex home.
async fn quota_lock(auth_path: &Path) -> Result<File, RenewError> {
    use sha2::{Digest, Sha256};

    let digest = Sha256::digest(auth_path.to_string_lossy().as_bytes());
    let name: String = digest
        .iter()
        .take(8)
        .map(|byte| format!("{:02x}", byte))
        .collect();
    let path = std::env::temp_dir().join(format!("quota-codex-renew-{}.lock", name));
    let file = open_lock_file(&path).map_err(RenewError::Store)?;
    lock_within(&file, QUOTA_LOCK_WAIT)
        .await
        .map_err(RenewError::Store)?;
    Ok(file)
}

fn open_lock_file(path: &Path) -> Result<File, String> {
    let mut options = std::fs::OpenOptions::new();
    options.read(true).write(true).create(true).truncate(false);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options
        .open(path)
        .map_err(|err| format!("could not open {}: {}", path.display(), err))
}

/// Take an exclusive lock on `file`, giving up after `limit` rather than
/// hanging behind a process that never lets go.
async fn lock_within(file: &File, limit: Duration) -> Result<(), String> {
    let deadline = std::time::Instant::now() + limit;
    loop {
        match file.try_lock() {
            Ok(()) => return Ok(()),
            Err(std::fs::TryLockError::WouldBlock) if std::time::Instant::now() < deadline => {
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
            Err(std::fs::TryLockError::WouldBlock) => {
                return Err("another program kept the Codex login locked".to_string())
            }
            Err(std::fs::TryLockError::Error(err)) => return Err(err.to_string()),
        }
    }
}
