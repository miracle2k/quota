//! Which logins `usage` reports on, and how each one is fetched.
//!
//! Claude and Codex logins come from several places, in this order:
//!
//! 1. the login each provider's own CLI uses now;
//! 2. quota-cli's own accounts, made with `quota-cli accounts add`;
//! 3. the config file;
//! 4. CodexBar's Codex accounts;
//! 5. claude-swap's Claude accounts.
//!
//! The same account often turns up in more than one place, so logins are
//! grouped by account before anything is fetched, and each account is reported
//! once. Its name is the most specific one any of its logins has, and its
//! numbers come from the login with the freshest token.

use crate::config::{expand_home, Config};
use crate::store::Registry;
use crate::{claude_swap, codexbar};
use quota_core::claude::local::{ClaudeLogin, LocalCredentialError as ClaudeCredentialError};
use quota_core::claude::local_usage::LocalUsageError as ClaudeUsageError;
use quota_core::claude::refresh as claude_refresh;
use quota_core::codex::local::{
    credentials_path_in, read_local_credentials_at, LocalCredentialError as CodexCredentialError,
};
use quota_core::codex::local_usage::LocalUsageError as CodexUsageError;
use quota_core::codex::refresh as codex_refresh;
use quota_core::providers::{ProviderError, PROVIDERS};
use quota_core::usage::ProviderUsage;
use std::path::{Path, PathBuf};

/// Where a login was found.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Source {
    /// The login the provider's CLI uses in this environment.
    Current,
    /// One of quota-cli's own accounts.
    Quota,
    /// The config file.
    Config,
    CodexBar,
    ClaudeSwap,
}

impl Source {
    pub fn name(self) -> &'static str {
        match self {
            Self::Current => "current",
            Self::Quota => "quota-cli",
            Self::Config => "config",
            Self::CodexBar => "codexbar",
            Self::ClaudeSwap => "claude-swap",
        }
    }

    /// The user set this login up for quota-cli, so it is always reported,
    /// and its label wins over any other name.
    pub fn is_explicit(self) -> bool {
        matches!(self, Self::Quota | Self::Config)
    }
}

/// Where a login's credentials are.
#[derive(Debug, Clone)]
pub enum Store {
    /// A Claude Code login: a config directory and its Keychain item.
    Claude(ClaudeLogin),
    /// A Codex login: the `CODEX_HOME` that holds `auth.json`.
    Codex(PathBuf),
    /// Usage another tool measured. quota has no credentials for it.
    Reported(Result<ProviderUsage, ProviderError>),
}

/// One login, as found before anything is fetched.
#[derive(Debug, Clone)]
pub struct Login {
    pub provider: &'static str,
    pub source: Source,
    /// The name it was given: a quota-cli or config file label, or a
    /// claude-swap alias.
    pub label: Option<String>,
    /// Where it lives, for messages and `accounts list`.
    pub location: Option<String>,
    pub store: Store,
    pub email: Option<String>,
    /// The account it is signed in to. Logins with the same key are the same
    /// account. `None` when that is unknown, which never matches.
    pub key: Option<String>,
    /// When the stored access token expires, in milliseconds.
    pub expires_at_ms: Option<i64>,
    /// Why the login cannot be used, when it cannot.
    pub problem: Option<ProviderError>,
}

impl Login {
    fn new(
        provider: &'static str,
        source: Source,
        label: Option<String>,
        location: Option<String>,
        store: Store,
    ) -> Self {
        Self {
            provider,
            source,
            label,
            location,
            store,
            email: None,
            key: None,
            expires_at_ms: None,
            problem: None,
        }
    }

    /// The command that signs this login in again.
    pub fn sign_in_hint(&self) -> String {
        let location = self.location.as_deref().unwrap_or_default();
        let label = self.label.as_deref().unwrap_or_default();
        match (self.source, self.provider) {
            (Source::Current, "claude") => "Sign in again with: claude auth login".to_string(),
            (Source::Current, _) => "Sign in again with: codex login".to_string(),
            (Source::Quota, provider) => format!(
                "Sign in again with: quota-cli accounts add {} {}",
                provider,
                quote(label)
            ),
            (Source::Config, "claude") => format!(
                "Sign in again with: CLAUDE_CONFIG_DIR={} claude auth login",
                quote(location)
            ),
            (Source::Config, _) => format!(
                "Sign in again with: CODEX_HOME={} codex login",
                quote(location)
            ),
            (Source::CodexBar, _) => "Sign in to this account again in CodexBar.".to_string(),
            (Source::ClaudeSwap, _) => "Sign in again with claude-swap.".to_string(),
        }
    }
}

/// One account's result, which is what `usage` prints a line for.
#[derive(Debug, Clone, PartialEq)]
pub struct AccountReport {
    pub provider: &'static str,
    /// The name shown after the provider. `None` for the login the
    /// provider's CLI uses now, when nothing gave it a label.
    pub label: Option<String>,
    pub email: Option<String>,
    /// Every place this account was found, in the order above.
    pub sources: Vec<Source>,
    pub result: Result<ProviderUsage, ProviderError>,
}

impl AccountReport {
    /// Set up for quota-cli on purpose, as opposed to found on the way.
    pub fn is_configured(&self) -> bool {
        self.sources.iter().any(|source| source.is_explicit())
    }
}

/// Group logins by account. Each group lists its logins' indices in order;
/// the groups are in the order their first login was found. Logins without a
/// key each get a group of their own, so each problem stays visible.
pub fn group_logins(keys: &[Option<String>]) -> Vec<Vec<usize>> {
    let mut groups: Vec<Vec<usize>> = Vec::new();
    let mut group_keys: Vec<Option<&str>> = Vec::new();
    for (index, key) in keys.iter().enumerate() {
        let existing = key.as_deref().and_then(|key| {
            group_keys
                .iter()
                .position(|group_key| *group_key == Some(key))
        });
        match existing {
            Some(group) => groups[group].push(index),
            None => {
                groups.push(vec![index]);
                group_keys.push(key.as_deref());
            }
        }
    }
    groups
}

/// The name an account is reported under: an explicit label first; then no
/// name at all for the provider's current login; then an alias; then the
/// email address.
pub fn account_label(logins: &[&Login]) -> Option<String> {
    let explicit = logins
        .iter()
        .find(|login| login.source.is_explicit() && login.label.is_some());
    if let Some(login) = explicit {
        return login.label.clone();
    }
    if logins.iter().any(|login| login.source == Source::Current) {
        return None;
    }
    logins
        .iter()
        .find_map(|login| login.label.clone())
        .or_else(|| logins.iter().find_map(|login| login.email.clone()))
}

/// The order to try an account's logins in: first the ones quota can fetch
/// itself, the one whose token expires last first, since another copy of the
/// same account may hold a token renewed since; then usage another tool
/// measured; then the rest, which only have a problem to report.
pub fn candidate_order(logins: &[&Login]) -> Vec<usize> {
    let fetchable =
        |login: &Login| login.problem.is_none() && !matches!(login.store, Store::Reported(_));
    let mut order: Vec<usize> = (0..logins.len())
        .filter(|&index| fetchable(logins[index]))
        .collect();
    // A stable sort, so equal expiries keep the order the logins were found in.
    order.sort_by_key(|&index| std::cmp::Reverse(logins[index].expires_at_ms.unwrap_or(i64::MIN)));
    order.extend(
        (0..logins.len()).filter(|&index| matches!(logins[index].store, Store::Reported(_))),
    );
    let rest: Vec<usize> = (0..logins.len())
        .filter(|index| !order.contains(index))
        .collect();
    order.extend(rest);
    order
}

/// Fetch one account, trying its logins in [`candidate_order`] until one
/// works. Only the first may renew its token. The others are used as they are
/// and skipped when their token has expired, because an older copy of an
/// account may hold a refresh token that was already spent, and sending a
/// spent one can get every copy of the login revoked.
pub async fn fetch_account(logins: &[&Login]) -> Result<ProviderUsage, ProviderError> {
    let mut first_error = None;
    for (position, index) in candidate_order(logins).into_iter().enumerate() {
        let login = logins[index];
        if position > 0 && login.problem.is_some() {
            continue;
        }
        match fetch_login(login, position == 0).await {
            Ok(usage) => return Ok(usage),
            Err(err) => {
                first_error.get_or_insert(err);
            }
        }
    }
    Err(first_error.unwrap_or_else(|| ProviderError::new("no login to fetch", false)))
}

/// Fetch every account in registry order. For Claude and Codex that is every
/// account found, the current login's first.
pub async fn fetch_all(
    config: &Config,
    registry: &Registry,
    providers: &[&'static str],
) -> Vec<AccountReport> {
    let mut reports = Vec::new();
    for &provider in PROVIDERS {
        if providers.is_empty() || providers.contains(&provider) {
            reports.extend(fetch_provider(provider, config, registry).await);
        }
    }
    reports
}

/// Fetch every account of one provider. Only Claude and Codex can have more
/// than the current login; an unknown provider has none.
pub async fn fetch_provider(
    provider: &'static str,
    config: &Config,
    registry: &Registry,
) -> Vec<AccountReport> {
    if provider != "claude" && provider != "codex" {
        return quota_core::providers::fetch_provider(provider)
            .await
            .map(|result| AccountReport {
                provider,
                label: None,
                email: None,
                sources: vec![Source::Current],
                result,
            })
            .into_iter()
            .collect();
    }

    let logins = discover(provider, config, registry).await;
    let keys: Vec<Option<String>> = logins.iter().map(|login| login.key.clone()).collect();

    let mut reports = Vec::new();
    for group in group_logins(&keys) {
        let members: Vec<&Login> = group.iter().map(|&index| &logins[index]).collect();
        reports.push(AccountReport {
            provider,
            label: account_label(&members),
            email: members.iter().find_map(|login| login.email.clone()),
            sources: members.iter().map(|login| login.source).collect(),
            result: fetch_account(&members).await,
        });
    }
    reports
}

/// One account and every login of it, for `accounts list`.
#[derive(Debug, Clone)]
pub struct ListedAccount {
    pub provider: &'static str,
    pub label: Option<String>,
    pub email: Option<String>,
    pub logins: Vec<Login>,
}

/// Every Claude and Codex account found, grouped like `usage` groups them,
/// without fetching usage. (claude-swap measures usage while it lists.)
pub async fn list_accounts(config: &Config, registry: &Registry) -> Vec<ListedAccount> {
    let mut listed = Vec::new();
    for provider in ["claude", "codex"] {
        let logins = discover(provider, config, registry).await;
        let keys: Vec<Option<String>> = logins.iter().map(|login| login.key.clone()).collect();
        for group in group_logins(&keys) {
            let members: Vec<&Login> = group.iter().map(|&index| &logins[index]).collect();
            listed.push(ListedAccount {
                provider,
                label: account_label(&members),
                email: members.iter().find_map(|login| login.email.clone()),
                logins: members.into_iter().cloned().collect(),
            });
        }
    }
    listed
}

/// Find every Claude or Codex login and read what can be read without the
/// network: which account each one is, and whether it is usable at all.
/// claude-swap is the exception, since it measures usage while it lists.
pub async fn discover(provider: &'static str, config: &Config, registry: &Registry) -> Vec<Login> {
    let mut logins = Vec::new();
    match provider {
        "claude" => {
            if let Some(login) = ClaudeLogin::current() {
                logins.push(Login::new(
                    provider,
                    Source::Current,
                    None,
                    Some(login.config_dir().display().to_string()),
                    Store::Claude(login),
                ));
            }
            for account in registry.for_provider(provider) {
                logins.push(Login::new(
                    provider,
                    Source::Quota,
                    Some(account.label.clone()),
                    Some(account.dir.clone()),
                    Store::Claude(ClaudeLogin::for_config_dir(&account.dir)),
                ));
            }
            for account in &config.claude {
                logins.push(Login::new(
                    provider,
                    Source::Config,
                    Some(account.label.clone()),
                    Some(account.config_dir.clone()),
                    Store::Claude(claude_login_for(&expand_home(&account.config_dir))),
                ));
            }
            logins.iter_mut().for_each(inspect);
            if config.sources.claude_swap {
                logins.extend(claude_swap_logins().await);
            }
        }
        "codex" => {
            if let Some(home) = quota_core::local_paths::provider_home("CODEX_HOME", ".codex") {
                logins.push(Login::new(
                    provider,
                    Source::Current,
                    None,
                    Some(home.display().to_string()),
                    Store::Codex(home),
                ));
            }
            for account in registry.for_provider(provider) {
                logins.push(Login::new(
                    provider,
                    Source::Quota,
                    Some(account.label.clone()),
                    Some(account.dir.clone()),
                    Store::Codex(PathBuf::from(&account.dir)),
                ));
            }
            for account in &config.codex {
                logins.push(Login::new(
                    provider,
                    Source::Config,
                    Some(account.label.clone()),
                    Some(account.home.clone()),
                    Store::Codex(PathBuf::from(expand_home(&account.home))),
                ));
            }
            if config.sources.codexbar {
                for account in codexbar::homes() {
                    let mut login = Login::new(
                        provider,
                        Source::CodexBar,
                        None,
                        Some(account.home.display().to_string()),
                        Store::Codex(account.home),
                    );
                    login.email = account.email;
                    logins.push(login);
                }
            }
            logins.iter_mut().for_each(inspect);
        }
        _ => {}
    }
    logins
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

/// The account key of a Claude login. claude-swap names accounts by email
/// and organization, and so does this, so the two can be matched.
pub fn claude_key(organization_uuid: Option<&str>, email: &str) -> String {
    format!(
        "claude:{}:{}",
        organization_uuid.unwrap_or_default(),
        email.trim().to_lowercase()
    )
}

/// Read a login's stored credentials and account without the network.
fn inspect(login: &mut Login) {
    match &login.store {
        Store::Claude(claude) => match claude.read_credentials() {
            Ok(credentials) => {
                login.expires_at_ms = credentials.expires_at_ms;
                let info = claude.account_info().unwrap_or_default();
                login.email = info.email.clone().or(login.email.take());
                login.key = Some(match (&info.email, &info.account_uuid) {
                    (Some(email), _) => claude_key(info.organization_uuid.as_deref(), email),
                    (None, Some(uuid)) => format!(
                        "claude-uuid:{}:{}",
                        info.organization_uuid.unwrap_or_default(),
                        uuid
                    ),
                    // Nothing recorded the account, so only the same store
                    // can match.
                    (None, None) => format!(
                        "claude-store:{}|{}",
                        claude.keychain_service(),
                        claude.credentials_path().display()
                    ),
                });
            }
            Err(err) => login.problem = Some(claude_problem(login, &err)),
        },
        Store::Codex(home) => match read_local_credentials_at(&credentials_path_in(home)) {
            Ok(credentials) => {
                login.expires_at_ms = credentials.access_token_expires_at_ms;
                login.email = credentials.email.clone().or(login.email.take());
                login.key = Some(match credentials.identity() {
                    Some(identity) => format!("codex:{}", identity),
                    None => {
                        let file = std::fs::canonicalize(credentials_path_in(home))
                            .unwrap_or_else(|_| credentials_path_in(home));
                        format!("codex-file:{}", file.display())
                    }
                });
            }
            Err(err) => login.problem = Some(codex_problem(login, &err)),
        },
        Store::Reported(_) => {}
    }
}

fn claude_problem(login: &Login, err: &ClaudeCredentialError) -> ProviderError {
    let not_found = matches!(err, ClaudeCredentialError::NotFound);
    match (login.source, not_found) {
        (Source::Current, _) | (_, false) => ProviderError::new(err.to_string(), not_found),
        (_, true) => ProviderError::new(
            format!(
                "No Claude Code login in {}. {}",
                login.location.as_deref().unwrap_or_default(),
                login.sign_in_hint().replace("Sign in again", "Sign in")
            ),
            true,
        ),
    }
}

fn codex_problem(login: &Login, err: &CodexCredentialError) -> ProviderError {
    let not_set_up = matches!(
        err,
        CodexCredentialError::NotFound | CodexCredentialError::ApiKeyOnly
    );
    match (login.source, err) {
        (Source::Current, _) => ProviderError::new(err.to_string(), not_set_up),
        (_, CodexCredentialError::NotFound) => ProviderError::new(
            format!(
                "No Codex login in {}. {}",
                login.location.as_deref().unwrap_or_default(),
                login.sign_in_hint().replace("Sign in again", "Sign in")
            ),
            true,
        ),
        _ => ProviderError::new(err.to_string(), not_set_up),
    }
}

async fn claude_swap_logins() -> Vec<Login> {
    let Some(listed) = claude_swap::list().await else {
        return Vec::new();
    };
    match listed {
        Ok(accounts) => accounts
            .into_iter()
            .map(|account| {
                let mut login = Login::new(
                    "claude",
                    Source::ClaudeSwap,
                    account.alias,
                    None,
                    Store::Reported(account.result),
                );
                if !account.email.is_empty() {
                    login.key = Some(claude_key(Some(&account.organization_uuid), &account.email));
                    login.email = Some(account.email);
                }
                login
            })
            .collect(),
        Err(message) => {
            let mut login = Login::new(
                "claude",
                Source::ClaudeSwap,
                Some("claude-swap".to_string()),
                None,
                Store::Reported(Err(ProviderError::new(message.clone(), false))),
            );
            login.problem = Some(ProviderError::new(message, false));
            vec![login]
        }
    }
}

/// Fetch usage for one login. With `renew`, an expired token is renewed first,
/// and once more when the usage API turns it down. Without, an expired token
/// is not even sent.
pub async fn fetch_login(login: &Login, renew: bool) -> Result<ProviderUsage, ProviderError> {
    if let Some(problem) = &login.problem {
        return Err(problem.clone());
    }
    if !renew && expires_soon(login.expires_at_ms) {
        return Err(ProviderError::new("the saved token has expired", false));
    }
    match &login.store {
        Store::Claude(claude) => fetch_claude(login, claude, renew).await,
        Store::Codex(home) => fetch_codex(login, home, renew).await,
        Store::Reported(result) => result.clone(),
    }
}

/// Both CLIs treat a token as expired five minutes early.
fn expires_soon(expires_at_ms: Option<i64>) -> bool {
    let now_ms = chrono::Utc::now().timestamp_millis();
    expires_at_ms.is_some_and(|expires_at| expires_at <= now_ms + 5 * 60 * 1000)
}

async fn fetch_claude(
    login: &Login,
    claude: &ClaudeLogin,
    renew: bool,
) -> Result<ProviderUsage, ProviderError> {
    let renew_failed = |err: claude_refresh::RenewError| {
        let message = if err.needs_sign_in() {
            format!("{} {}", err, login.sign_in_hint())
        } else {
            err.to_string()
        };
        ProviderError::new(message, false)
    };
    let read = || {
        claude
            .read_credentials()
            .map_err(|err| ProviderError::new(err.to_string(), false))
    };

    if renew {
        claude_refresh::renew(claude, None)
            .await
            .map_err(renew_failed)?;
    }
    let credentials = read()?;
    match quota_core::claude::local_usage::fetch_usage(&credentials).await {
        Err(ClaudeUsageError::Unauthorized) if renew => {
            claude_refresh::renew(claude, Some(&credentials.access_token))
                .await
                .map_err(renew_failed)?;
            let credentials = read()?;
            quota_core::claude::local_usage::fetch_usage(&credentials)
                .await
                .map_err(|err| claude_usage_error(login, err))
        }
        other => other.map_err(|err| claude_usage_error(login, err)),
    }
}

fn claude_usage_error(login: &Login, err: ClaudeUsageError) -> ProviderError {
    match err {
        ClaudeUsageError::Unauthorized => ProviderError::new(
            format!(
                "Claude still rejects the token after renewing it. {}",
                login.sign_in_hint()
            ),
            false,
        ),
        other => ProviderError::new(other.to_string(), other.is_not_configured()),
    }
}

async fn fetch_codex(
    login: &Login,
    home: &Path,
    renew: bool,
) -> Result<ProviderUsage, ProviderError> {
    let renew_failed = |err: codex_refresh::RenewError| {
        let message = if err.needs_sign_in() {
            format!("{} {}", err, login.sign_in_hint())
        } else {
            err.to_string()
        };
        ProviderError::new(message, false)
    };
    let read = || {
        read_local_credentials_at(&credentials_path_in(home))
            .map_err(|err| ProviderError::new(err.to_string(), false))
    };

    if renew {
        codex_refresh::renew(home, None)
            .await
            .map_err(renew_failed)?;
    }
    let credentials = read()?;
    match quota_core::codex::local_usage::fetch_usage(&credentials).await {
        Err(CodexUsageError::Unauthorized) if renew => {
            codex_refresh::renew(home, Some(&credentials.access_token))
                .await
                .map_err(renew_failed)?;
            let credentials = read()?;
            quota_core::codex::local_usage::fetch_usage(&credentials)
                .await
                .map_err(|err| codex_usage_error(login, err))
        }
        other => other.map_err(|err| codex_usage_error(login, err)),
    }
}

fn codex_usage_error(login: &Login, err: CodexUsageError) -> ProviderError {
    match err {
        CodexUsageError::Unauthorized => ProviderError::new(
            format!(
                "Codex still rejects the token after renewing it. {}",
                login.sign_in_hint()
            ),
            false,
        ),
        other => ProviderError::new(other.to_string(), other.is_not_configured()),
    }
}

/// Quote a value for a shell command line when it needs it.
fn quote(value: &str) -> String {
    let plain = !value.is_empty()
        && value.chars().all(|c| {
            c.is_ascii_alphanumeric() || matches!(c, '/' | '.' | '_' | '-' | '~' | ':' | '@')
        });
    if plain {
        value.to_string()
    } else {
        format!("'{}'", value.replace('\'', "'\\''"))
    }
}
