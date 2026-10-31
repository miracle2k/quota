//! Codex accounts that CodexBar knows about.
//!
//! CodexBar signs each extra Codex account in to a Codex home of its own and
//! lists them in `managed-codex-accounts.json`. Its config file can also name
//! more Codex homes (`codexProfileHomePaths`). Each one is an ordinary Codex
//! login, so quota reads it like any other `CODEX_HOME`.

use serde::Deserialize;
use std::path::PathBuf;

/// The registry versions CodexBar has written so far. A newer one may mean
/// something else, so it is skipped rather than guessed at.
const KNOWN_REGISTRY_VERSIONS: std::ops::RangeInclusive<u32> = 1..=3;

/// One Codex home CodexBar knows about.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CodexBarHome {
    /// The address CodexBar recorded for it, if any.
    pub email: Option<String>,
    /// The account's `CODEX_HOME`.
    pub home: PathBuf,
}

#[derive(Deserialize)]
struct ManagedAccounts {
    version: Option<u32>,
    #[serde(default)]
    accounts: Vec<ManagedAccount>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ManagedAccount {
    email: Option<String>,
    managed_home_path: Option<String>,
}

#[derive(Deserialize)]
struct ConfigFile {
    #[serde(default)]
    providers: Vec<ProviderConfig>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ProviderConfig {
    #[serde(default)]
    id: String,
    #[serde(default)]
    codex_profile_home_paths: Vec<String>,
}

/// Every Codex home CodexBar knows about: its own accounts first, then the
/// ones its config file names. Files that are missing or unreadable mean no
/// homes; CodexBar's state is not quota's to report errors about.
pub fn homes() -> Vec<CodexBarHome> {
    let mut homes = managed_accounts_path()
        .and_then(|path| std::fs::read_to_string(path).ok())
        .map(|text| parse_managed_accounts(&text))
        .unwrap_or_default();
    homes.extend(
        config_path()
            .and_then(|path| std::fs::read_to_string(path).ok())
            .map(|text| parse_profile_homes(&text))
            .unwrap_or_default(),
    );
    homes
}

/// `CodexBar/managed-codex-accounts.json` in the platform's data directory,
/// which on macOS is `~/Library/Application Support`.
pub fn managed_accounts_path() -> Option<PathBuf> {
    dirs::data_dir().map(|dir| dir.join("CodexBar").join("managed-codex-accounts.json"))
}

pub fn parse_managed_accounts(text: &str) -> Vec<CodexBarHome> {
    let Ok(list) = serde_json::from_str::<ManagedAccounts>(text) else {
        return Vec::new();
    };
    if !list
        .version
        .is_some_and(|version| KNOWN_REGISTRY_VERSIONS.contains(&version))
    {
        return Vec::new();
    }
    list.accounts
        .into_iter()
        .filter_map(|account| {
            let home = account
                .managed_home_path
                .filter(|path| !path.trim().is_empty())?;
            Some(CodexBarHome {
                email: account.email.filter(|email| !email.trim().is_empty()),
                home: PathBuf::from(crate::config::expand_home(&home)),
            })
        })
        .collect()
}

/// CodexBar's config file, found the way CodexBar finds it.
pub fn config_path() -> Option<PathBuf> {
    if let Some(path) = std::env::var_os("CODEXBAR_CONFIG").filter(|value| !value.is_empty()) {
        return Some(PathBuf::from(path));
    }
    if let Some(dir) = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .filter(|dir| dir.is_absolute())
    {
        return Some(dir.join("codexbar").join("config.json"));
    }
    let home = dirs::home_dir()?;
    let xdg_default = home.join(".config").join("codexbar").join("config.json");
    let legacy = home.join(".codexbar").join("config.json");
    if !xdg_default.exists() && legacy.exists() {
        return Some(legacy);
    }
    Some(xdg_default)
}

/// The extra Codex homes in CodexBar's config file. Only that one setting is
/// read; the file holds other providers' settings too, which stay untouched.
pub fn parse_profile_homes(text: &str) -> Vec<CodexBarHome> {
    let Ok(config) = serde_json::from_str::<ConfigFile>(text) else {
        return Vec::new();
    };
    config
        .providers
        .into_iter()
        .filter(|provider| provider.id == "codex")
        .flat_map(|provider| provider.codex_profile_home_paths)
        .map(|path| path.trim().to_string())
        // CodexBar accepts `~` and absolute paths only.
        .filter(|path| path == "~" || path.starts_with("~/") || path.starts_with('/'))
        .map(|path| CodexBarHome {
            email: None,
            home: PathBuf::from(crate::config::expand_home(&path)),
        })
        .collect()
}
