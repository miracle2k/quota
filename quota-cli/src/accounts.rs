//! Which logins `usage` reports on: the one each provider's own CLI is using
//! now, plus the extra Claude and Codex logins listed in the config file.
//!
//! The current login is often one of the configured ones as well, so logins
//! are matched by account before anything is fetched, and a configured entry
//! takes the place of the current login it repeats.

use crate::config::{expand_home, Config};
use quota_core::claude::local::{ClaudeLogin, LocalCredentialError as ClaudeCredentialError};
use quota_core::codex::local::{
    credentials_path_in, local_credentials_path, read_local_credentials_at,
    LocalCredentialError as CodexCredentialError,
};
use quota_core::providers::{ProviderError, PROVIDERS};
use quota_core::usage::ProviderUsage;
use std::collections::HashSet;
use std::path::{Path, PathBuf};

/// One login's result.
#[derive(Debug, Clone, PartialEq)]
pub struct AccountReport {
    pub provider: &'static str,
    /// The config file's label. `None` for a login found the usual way.
    pub label: Option<String>,
    pub result: Result<ProviderUsage, ProviderError>,
}

impl AccountReport {
    /// Listed in the config file, as opposed to found the usual way.
    pub fn is_configured(&self) -> bool {
        self.label.is_some()
    }
}

/// Decide which logins to report, given each login's account key. `keys[0]` is
/// the login the provider's CLI is using now; the rest come from the config
/// file, in order. A configured login that repeats an earlier configured
/// account is dropped, and the current login is dropped when a configured
/// entry covers its account, so that entry's label is the one shown. A login
/// without a key, such as one that could not be read, never matches.
///
/// Returns the indices to keep, the current login first.
pub fn unique_logins(keys: &[Option<String>]) -> Vec<usize> {
    let mut seen = HashSet::new();
    let mut configured = Vec::new();
    for (index, key) in keys.iter().enumerate().skip(1) {
        if let Some(key) = key {
            if !seen.insert(key.as_str()) {
                continue;
            }
        }
        configured.push(index);
    }

    let current_is_covered = keys
        .first()
        .and_then(Option::as_ref)
        .map(|key| seen.contains(key.as_str()))
        .unwrap_or(false);

    let mut kept = Vec::with_capacity(configured.len() + 1);
    if !keys.is_empty() && !current_is_covered {
        kept.push(0);
    }
    kept.extend(configured);
    kept
}

/// Fetch every login in registry order: for each provider the current login
/// first, then its configured logins in file order. Like the single-login
/// version, the calls run one after another.
pub async fn fetch_all(config: &Config) -> Vec<AccountReport> {
    let mut reports = Vec::new();
    for &provider in PROVIDERS {
        reports.extend(fetch_provider_logins(provider, config).await);
    }
    reports
}

/// Fetch every login for one provider. Only Claude and Codex can have more
/// than the current one; an unknown provider has none.
pub async fn fetch_provider_logins(provider: &'static str, config: &Config) -> Vec<AccountReport> {
    match provider {
        "claude" => fetch_claude(config).await,
        "codex" => fetch_codex(config).await,
        _ => quota_core::providers::fetch_provider(provider)
            .await
            .map(|result| AccountReport {
                provider,
                label: None,
                result,
            })
            .into_iter()
            .collect(),
    }
}

/// One login to report on before anything is fetched.
struct Entry<L> {
    label: Option<String>,
    /// The directory as the config file wrote it, for messages.
    configured_dir: Option<String>,
    login: Option<L>,
}

/// The login a configured Claude directory names. The default directory means
/// the default login, which Claude Code keeps under the plain Keychain item,
/// not the item it would use if `CLAUDE_CONFIG_DIR` were set to that path.
pub fn claude_login_for(config_dir: &str) -> ClaudeLogin {
    match ClaudeLogin::default_dir() {
        Some(default) if Path::new(config_dir) == default.config_dir() => default,
        _ => ClaudeLogin::for_config_dir(config_dir),
    }
}

async fn fetch_claude(config: &Config) -> Vec<AccountReport> {
    let mut entries = vec![Entry {
        label: None,
        configured_dir: None,
        login: ClaudeLogin::current(),
    }];
    entries.extend(config.claude.iter().map(|account| Entry {
        label: Some(account.label.clone()),
        configured_dir: Some(account.config_dir.clone()),
        login: Some(claude_login_for(&expand_home(&account.config_dir))),
    }));

    let credentials: Vec<_> = entries
        .iter()
        .map(|entry| match &entry.login {
            Some(login) => login.read_credentials(),
            None => Err(ClaudeCredentialError::Unreadable(
                "no home directory".to_string(),
            )),
        })
        .collect();

    // Only a login that reads can be matched. The recorded account is the
    // real match; the storage location stands in when Claude Code has not
    // recorded one.
    let keys: Vec<Option<String>> = entries
        .iter()
        .zip(&credentials)
        .map(|(entry, read)| {
            let login = entry.login.as_ref().filter(|_| read.is_ok())?;
            Some(login.account_identity().unwrap_or_else(|| {
                format!(
                    "{}|{}",
                    login.keychain_service(),
                    login.credentials_path().display()
                )
            }))
        })
        .collect();

    let mut reports = Vec::new();
    for index in unique_logins(&keys) {
        let entry = &entries[index];
        let result = match &credentials[index] {
            Ok(read) => quota_core::claude::local_usage::fetch_usage(read)
                .await
                .map_err(|err| ProviderError::new(err.to_string(), err.is_not_configured())),
            Err(ClaudeCredentialError::NotFound) if entry.configured_dir.is_some() => {
                let dir = entry.configured_dir.as_deref().unwrap_or_default();
                Err(ProviderError::new(
                    format!(
                        "No Claude Code login in {dir}. Sign in with: CLAUDE_CONFIG_DIR={dir} claude"
                    ),
                    true,
                ))
            }
            Err(err) => Err(ProviderError::new(
                err.to_string(),
                matches!(err, ClaudeCredentialError::NotFound),
            )),
        };
        reports.push(AccountReport {
            provider: "claude",
            label: entry.label.clone(),
            result,
        });
    }
    reports
}

async fn fetch_codex(config: &Config) -> Vec<AccountReport> {
    let mut entries: Vec<Entry<PathBuf>> = vec![Entry {
        label: None,
        configured_dir: None,
        login: local_credentials_path(),
    }];
    entries.extend(config.codex.iter().map(|account| Entry {
        label: Some(account.label.clone()),
        configured_dir: Some(account.home.clone()),
        login: Some(credentials_path_in(Path::new(&expand_home(&account.home)))),
    }));

    let credentials: Vec<_> = entries
        .iter()
        .map(|entry| match &entry.login {
            Some(path) => read_local_credentials_at(path),
            None => Err(CodexCredentialError::Unreadable(
                "no home directory".to_string(),
            )),
        })
        .collect();

    // The signed-in account is the real match; the file stands in when the
    // token did not name one.
    let keys: Vec<Option<String>> = entries
        .iter()
        .zip(&credentials)
        .map(|(entry, read)| {
            let path = entry.login.as_ref()?;
            let read = read.as_ref().ok()?;
            Some(read.identity().unwrap_or_else(|| {
                let file = std::fs::canonicalize(path).unwrap_or_else(|_| path.clone());
                format!("file:{}", file.display())
            }))
        })
        .collect();

    let mut reports = Vec::new();
    for index in unique_logins(&keys) {
        let entry = &entries[index];
        let result = match &credentials[index] {
            Ok(read) => quota_core::codex::local_usage::fetch_usage(read)
                .await
                .map_err(|err| ProviderError::new(err.to_string(), err.is_not_configured())),
            Err(CodexCredentialError::NotFound) if entry.configured_dir.is_some() => {
                let home = entry.configured_dir.as_deref().unwrap_or_default();
                Err(ProviderError::new(
                    format!(
                        "No Codex login in {home}. Sign in with: CODEX_HOME={home} codex login"
                    ),
                    true,
                ))
            }
            Err(err) => Err(ProviderError::new(
                err.to_string(),
                matches!(
                    err,
                    CodexCredentialError::NotFound | CodexCredentialError::ApiKeyOnly
                ),
            )),
        };
        reports.push(AccountReport {
            provider: "codex",
            label: entry.label.clone(),
            result,
        });
    }
    reports
}
