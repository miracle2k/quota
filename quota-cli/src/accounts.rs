//! Which logins `usage` reports on, and how each one is fetched.
//!
//! Claude, Codex, Antigravity and Z.ai logins come from several places, in
//! this order:
//!
//! 1. the login each provider's own CLI uses now (for Z.ai, `ZAI_API_KEY`);
//! 2. quota-cli's own accounts, made with `quota-cli accounts add`;
//! 3. the config file;
//! 4. CodexBar's Codex accounts;
//! 5. claude-swap's Claude accounts;
//! 6. the pi coding agent's logins.
//!
//! The same account often turns up in more than one place, so logins are
//! grouped by account before anything is fetched, and each account is reported
//! once. Its name is the most specific one any of its logins has, and its
//! numbers come from the login with the freshest token.

use crate::config::{expand_home, Config};
use crate::store::Registry;
use crate::{claude_swap, codexbar};
use quota_core::antigravity::local::LocalAntigravityCredentials;
use quota_core::claude::local::LocalClaudeCredentials;
use quota_core::claude::local::{ClaudeLogin, LocalCredentialError as ClaudeCredentialError};
use quota_core::claude::local_usage::LocalUsageError as ClaudeUsageError;
use quota_core::claude::refresh as claude_refresh;
use quota_core::codex::local::LocalCodexCredentials;
use quota_core::codex::local::{
    credentials_path_in, read_local_credentials_at, LocalCredentialError as CodexCredentialError,
};
use quota_core::codex::local_usage::LocalUsageError as CodexUsageError;
use quota_core::codex::refresh as codex_refresh;
use quota_core::providers::{ProviderError, PROVIDERS};
use quota_core::usage::ProviderUsage;
use quota_core::zai::Region as ZaiRegion;
use std::path::{Path, PathBuf};

/// The providers whose logins can come from more than one place.
pub const MULTI_LOGIN_PROVIDERS: &[&str] = &["claude", "codex", "antigravity", "zai"];

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
    /// The pi coding agent's `auth.json`.
    Pi,
}

impl Source {
    pub fn name(self) -> &'static str {
        match self {
            Self::Current => "current",
            Self::Quota => "quota-cli",
            Self::Config => "config",
            Self::CodexBar => "codexbar",
            Self::ClaudeSwap => "claude-swap",
            Self::Pi => "pi",
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
    /// A Claude or Codex access token another tool owns, such as pi's. It is
    /// used while it is good and never renewed here: the tool keeps the
    /// refresh token, and a Claude or Codex refresh token works only once.
    ClaudeToken(LocalClaudeCredentials),
    CodexToken(LocalCodexCredentials),
    /// A Google login for Antigravity. Google renews the access token without
    /// replacing the refresh token, so it is renewed in memory when needed.
    Antigravity(Box<LocalAntigravityCredentials>),
    /// A Z.ai API key.
    Zai(ApiKey, ZaiRegion),
}

impl Store {
    /// Whether an expired token here can be renewed without writing anything,
    /// so that trying it costs no other copy of the login its refresh token.
    fn renews_in_memory(&self) -> bool {
        matches!(self, Self::Antigravity(_) | Self::Zai(..))
    }
}

/// An API key, kept out of `Debug` output.
#[derive(Clone)]
pub struct ApiKey(pub String);

impl std::fmt::Debug for ApiKey {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("ApiKey(<redacted>)")
    }
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
            (Source::Current, "antigravity") => "Sign in to Antigravity again.".to_string(),
            (Source::Current, "zai") => {
                format!("Check the key in {}.", quota_core::zai::API_KEY_ENV)
            }
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
            (Source::Pi, _) => "Sign in again in pi with /login.".to_string(),
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

/// Fetch every account of the given providers, in registry order. For Claude
/// and Codex that is every account found, the current login's first.
pub async fn fetch_all(
    config: &Config,
    registry: &Registry,
    providers: &[&'static str],
) -> Vec<AccountReport> {
    let mut reports = Vec::new();
    for &provider in PROVIDERS {
        if providers.contains(&provider) {
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
    if !MULTI_LOGIN_PROVIDERS.contains(&provider) {
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

/// Every account of the providers that can have several, grouped like
/// `usage` groups them, without fetching usage. (claude-swap measures usage
/// while it lists, and pi's Claude login is looked up by its token.)
pub async fn list_accounts(config: &Config, registry: &Registry) -> Vec<ListedAccount> {
    let mut listed = Vec::new();
    for &provider in MULTI_LOGIN_PROVIDERS {
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
            if config.sources.pi {
                logins.extend(pi_claude_login().await);
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
            if config.sources.pi {
                logins.extend(pi_codex_login());
            }
        }
        "antigravity" => {
            logins.push(current_antigravity_login());
            if config.sources.pi {
                logins.extend(pi_antigravity_logins());
            }
        }
        "zai" => {
            if let Ok(key) = std::env::var(quota_core::zai::API_KEY_ENV) {
                if !key.trim().is_empty() {
                    logins.push(zai_login(
                        Source::Current,
                        quota_core::zai::API_KEY_ENV,
                        key.trim(),
                        ZaiRegion::Global,
                    ));
                }
            }
            if config.sources.pi {
                logins.extend(pi_zai_logins());
            }
        }
        _ => {}
    }
    logins
}

/// How long before expiry a token another tool owns stops being used. Both
/// CLIs renew five minutes early.
const TOKEN_MARGIN_MS: i64 = 5 * 60 * 1000;

fn now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

/// Where pi keeps its logins, for messages and `accounts list`.
fn pi_location() -> Option<String> {
    quota_core::pi::agent_dir().map(|dir| dir.join("auth.json").display().to_string())
}

/// pi's logins, or why there are none. A missing file is simply no logins.
fn pi_auth() -> Result<serde_json::Map<String, serde_json::Value>, ProviderError> {
    quota_core::pi::read_auth().map_err(|err| ProviderError::new(err.to_string(), false))
}

/// A login of pi's whose token has expired. pi renews it the next time it
/// uses it; until then there is nothing to report, so it counts as not set up.
fn expired_pi_login(provider: &'static str, store: Store) -> Login {
    let mut login = Login::new(provider, Source::Pi, None, pi_location(), store);
    login.problem = Some(ProviderError::new(
        format!(
            "pi's {} login has expired. pi renews it the next time it uses it.",
            provider
        ),
        true,
    ));
    login
}

/// pi's Claude login. Its token does not say whose it is, so the account is
/// looked up with it, which needs a token that is still good.
async fn pi_claude_login() -> Option<Login> {
    let auth = match pi_auth() {
        Ok(auth) => auth,
        Err(problem) => return Some(pi_problem_login("claude", problem)),
    };
    let quota_core::pi::PiCredential::OAuth(oauth) =
        quota_core::pi::login(&auth, "anthropic").ok()?
    else {
        return None;
    };
    let credentials = LocalClaudeCredentials {
        access_token: oauth.access.clone(),
        expires_at_ms: oauth.expires_ms,
        scopes: Vec::new(),
        subscription_type: None,
        organization_uuid: None,
    };
    if oauth.is_expired_at_ms(now_ms(), TOKEN_MARGIN_MS) {
        return Some(expired_pi_login("claude", Store::ClaudeToken(credentials)));
    }
    let mut login = Login::new(
        "claude",
        Source::Pi,
        None,
        pi_location(),
        Store::ClaudeToken(credentials),
    );
    login.expires_at_ms = oauth.expires_ms;
    match quota_core::claude::local_usage::fetch_account(&oauth.access).await {
        Ok(info) => {
            login.email = info.email.clone();
            login.key = info
                .email
                .as_deref()
                .map(|email| claude_key(info.organization_uuid.as_deref(), email));
        }
        Err(err) => login.problem = Some(ProviderError::new(err.to_string(), false)),
    }
    Some(login)
}

/// pi's Codex login. Its access token names the account, so no request is
/// needed to place it.
fn pi_codex_login() -> Option<Login> {
    let auth = match pi_auth() {
        Ok(auth) => auth,
        Err(problem) => return Some(pi_problem_login("codex", problem)),
    };
    let quota_core::pi::PiCredential::OAuth(oauth) =
        quota_core::pi::login(&auth, "openai-codex").ok()?
    else {
        return None;
    };
    let credentials = quota_core::codex::local::credentials_from_access_token(
        &oauth.access,
        oauth.extra_text("accountId"),
    );
    let expires_at_ms = oauth.expires_ms.or(credentials.access_token_expires_at_ms);
    let identity = credentials.identity();
    let email = credentials.email.clone();
    if expires_at_ms.is_some_and(|expires| expires <= now_ms() + TOKEN_MARGIN_MS) {
        return Some(expired_pi_login("codex", Store::CodexToken(credentials)));
    }
    let mut login = Login::new(
        "codex",
        Source::Pi,
        None,
        pi_location(),
        Store::CodexToken(credentials),
    );
    login.expires_at_ms = expires_at_ms;
    login.email = email;
    login.key = identity.map(|identity| format!("codex:{}", identity));
    Some(login)
}

/// The Antigravity login this machine's own Antigravity sign-in left, if any.
fn current_antigravity_login() -> Login {
    use quota_core::antigravity::local::{read_any_credentials, LocalCredentialError};
    use quota_core::antigravity::local_usage::{oauth_client_for, LocalUsageError};

    let mut login = Login::new(
        "antigravity",
        Source::Current,
        None,
        None,
        Store::Reported(Err(ProviderError::new("not read", true))),
    );
    match read_any_credentials() {
        Ok(credentials) => {
            if oauth_client_for(credentials.issued_to_client_id.as_deref()).is_none() {
                // A Google login of another tool, such as the Gemini CLI. It
                // is not an Antigravity login at all.
                let err =
                    LocalUsageError::UnknownOauthClient(credentials.issued_to_client_id.clone());
                login.problem = Some(ProviderError::new(err.to_string(), true));
                return login;
            }
            login.email = credentials.email.clone();
            login.expires_at_ms = credentials.expiry_date_ms;
            login.key = credentials.email.as_deref().map(antigravity_key);
            login.store = Store::Antigravity(Box::new(credentials));
        }
        Err(err) => {
            let not_found = matches!(err, LocalCredentialError::NotFound);
            login.problem = Some(ProviderError::new(err.to_string(), not_found));
        }
    }
    login
}

fn antigravity_key(email: &str) -> String {
    format!("google:{}", email.trim().to_lowercase())
}

/// The Antigravity accounts the pi-antigravity extension has signed in: the
/// active one in `auth.json`, and any others it linked.
fn pi_antigravity_logins() -> Vec<Login> {
    let auth = match pi_auth() {
        Ok(auth) => auth,
        Err(problem) => return vec![pi_problem_login("antigravity", problem)],
    };
    let mut accounts = Vec::new();
    if let Ok(quota_core::pi::PiCredential::OAuth(oauth)) =
        quota_core::pi::login(&auth, "antigravity")
    {
        accounts.push(oauth);
    }
    accounts.extend(quota_core::pi::antigravity_accounts().unwrap_or_default());

    let mut logins: Vec<Login> = Vec::new();
    for oauth in accounts {
        let credentials = LocalAntigravityCredentials::from_pi(&oauth);
        let key = credentials.email.as_deref().map(antigravity_key);
        // The active account is in both files.
        if key.is_some() && logins.iter().any(|login| login.key == key) {
            continue;
        }
        let mut login = Login::new(
            "antigravity",
            Source::Pi,
            None,
            pi_location(),
            Store::Antigravity(Box::new(credentials.clone())),
        );
        login.email = credentials.email.clone();
        login.expires_at_ms = credentials.expiry_date_ms;
        login.key = key;
        logins.push(login);
    }
    logins
}

/// pi's Z.ai keys: `zai` for api.z.ai, `zai-coding-cn` for BigModel.
fn pi_zai_logins() -> Vec<Login> {
    let auth = match pi_auth() {
        Ok(auth) => auth,
        Err(problem) => return vec![pi_problem_login("zai", problem)],
    };
    let mut logins = Vec::new();
    for (provider, region) in quota_core::zai::PI_PROVIDERS {
        match quota_core::pi::login(&auth, provider) {
            Ok(quota_core::pi::PiCredential::ApiKey(key)) => {
                logins.push(zai_login(
                    Source::Pi,
                    &pi_location().unwrap_or_default(),
                    &key,
                    *region,
                ));
            }
            Ok(quota_core::pi::PiCredential::OAuth(_)) | Err(quota_core::pi::PiError::NotFound) => {
            }
            Err(err) => logins.push(pi_problem_login(
                "zai",
                ProviderError::new(err.to_string(), false),
            )),
        }
    }
    logins
}

fn zai_login(source: Source, location: &str, key: &str, region: ZaiRegion) -> Login {
    use sha2::{Digest, Sha256};

    let digest = Sha256::digest(key.as_bytes());
    let fingerprint: String = digest
        .iter()
        .take(8)
        .map(|byte| format!("{:02x}", byte))
        .collect();
    let mut login = Login::new(
        "zai",
        source,
        None,
        Some(location.to_string()),
        Store::Zai(ApiKey(key.to_string()), region),
    );
    login.key = Some(format!("zai:{:?}:{}", region, fingerprint));
    login
}

/// pi's file exists but cannot be read: one line saying so.
fn pi_problem_login(provider: &'static str, problem: ProviderError) -> Login {
    let mut login = Login::new(
        provider,
        Source::Pi,
        None,
        pi_location(),
        Store::Reported(Err(problem.clone())),
    );
    login.problem = Some(problem);
    login
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
        Store::Reported(_)
        | Store::ClaudeToken(_)
        | Store::CodexToken(_)
        | Store::Antigravity(_)
        | Store::Zai(..) => {}
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
    if !renew && !login.store.renews_in_memory() && expires_soon(login.expires_at_ms) {
        return Err(ProviderError::new("the saved token has expired", false));
    }
    match &login.store {
        Store::Claude(claude) => fetch_claude(login, claude, renew).await,
        Store::Codex(home) => fetch_codex(login, home, renew).await,
        Store::Reported(result) => result.clone(),
        Store::ClaudeToken(credentials) => {
            quota_core::claude::local_usage::fetch_usage(credentials)
                .await
                .map_err(|err| owned_token_error(login, err.to_string()))
        }
        Store::CodexToken(credentials) => quota_core::codex::local_usage::fetch_usage(credentials)
            .await
            .map_err(|err| owned_token_error(login, err.to_string())),
        Store::Antigravity(credentials) => {
            quota_core::antigravity::local_usage::fetch_usage(credentials)
                .await
                .map_err(|err| ProviderError::new(err.to_string(), err.is_not_configured()))
        }
        Store::Zai(key, region) => quota_core::zai::fetch_usage(&key.0, *region)
            .await
            .map_err(|err| ProviderError::new(err.to_string(), err.is_not_configured())),
    }
}

/// A token another tool owns failed. Its own messages suggest running a CLI
/// that would not renew it, so name the owner instead.
fn owned_token_error(login: &Login, message: String) -> ProviderError {
    ProviderError::new(
        format!(
            "{} ({} login)",
            message.trim_end_matches('.'),
            login.source.name()
        ),
        false,
    )
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
