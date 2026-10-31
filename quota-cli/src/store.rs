//! The logins quota-cli keeps itself, made with `quota-cli accounts add`.
//!
//! Each one is an ordinary provider login in a directory of its own, so the
//! provider's CLI can use it as well:
//!
//! ```text
//! CLAUDE_CONFIG_DIR=<dir> claude
//! CODEX_HOME=<dir> codex
//! ```
//!
//! The directories live under the data directory, next to `accounts.json`,
//! which records the label of each one.

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// Moves the data directory somewhere other than the default.
pub const DATA_DIR_ENV: &str = "QUOTA_CLI_DATA_DIR";
const REGISTRY_FILE: &str = "accounts.json";
const REGISTRY_VERSION: u32 = 1;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Registry {
    pub version: u32,
    #[serde(default)]
    pub accounts: Vec<ManagedAccount>,
}

impl Default for Registry {
    fn default() -> Self {
        Self {
            version: REGISTRY_VERSION,
            accounts: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ManagedAccount {
    /// `claude` or `codex`.
    pub provider: String,
    pub label: String,
    /// The login's directory, exactly as it is passed to the provider's CLI.
    /// Claude Code names its Keychain item after this text, so it is stored
    /// rather than rebuilt.
    pub dir: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub created_at: Option<String>,
}

impl Registry {
    pub fn find(&self, provider: &str, label: &str) -> Option<&ManagedAccount> {
        self.accounts
            .iter()
            .find(|account| account.provider == provider && account.label == label)
    }

    pub fn for_provider<'a>(
        &'a self,
        provider: &'a str,
    ) -> impl Iterator<Item = &'a ManagedAccount> + 'a {
        self.accounts
            .iter()
            .filter(move |account| account.provider == provider)
    }
}

/// `quota-cli` under the platform's data directory: `~/Library/Application
/// Support` on macOS, `$XDG_DATA_HOME` or `~/.local/share` on Linux, and
/// `%APPDATA%` on Windows. `QUOTA_CLI_DATA_DIR` replaces the whole path.
pub fn data_dir() -> Option<PathBuf> {
    match std::env::var(DATA_DIR_ENV) {
        Ok(value) if !value.trim().is_empty() => {
            Some(PathBuf::from(crate::config::expand_home(value.trim())))
        }
        _ => dirs::data_dir().map(|dir| dir.join("quota-cli")),
    }
}

pub fn registry_path(data_dir: &Path) -> PathBuf {
    data_dir.join(REGISTRY_FILE)
}

/// Load the registry. A missing file is an empty registry.
pub fn load(data_dir: &Path) -> Result<Registry, String> {
    let path = registry_path(data_dir);
    match std::fs::read_to_string(&path) {
        Ok(text) => {
            serde_json::from_str(&text).map_err(|err| format!("{}: {}", path.display(), err))
        }
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(Registry::default()),
        Err(err) => Err(format!("could not read {}: {}", path.display(), err)),
    }
}

pub fn save(data_dir: &Path, registry: &Registry) -> Result<(), String> {
    create_private_dir(data_dir)?;
    let text = serde_json::to_string_pretty(registry).map_err(|err| err.to_string())?;
    quota_core::fs_util::write_private_atomic(&registry_path(data_dir), &format!("{}\n", text))
}

/// The directory a new login for `label` goes in: `accounts/<provider>/<slug>`,
/// with a number added when that name is already taken on disk.
pub fn new_account_dir(data_dir: &Path, provider: &str, label: &str) -> PathBuf {
    let parent = data_dir.join("accounts").join(provider);
    let base = slug(label);
    let mut candidate = parent.join(&base);
    let mut number = 2;
    while candidate.exists() {
        candidate = parent.join(format!("{}-{}", base, number));
        number += 1;
    }
    candidate
}

/// A directory name for a label: lowercase letters, digits and dashes.
pub fn slug(label: &str) -> String {
    let mut slug = String::new();
    for c in label.trim().chars().flat_map(char::to_lowercase) {
        if c.is_ascii_alphanumeric() {
            slug.push(c);
        } else if !slug.ends_with('-') {
            slug.push('-');
        }
    }
    let slug = slug.trim_matches('-').to_string();
    if slug.is_empty() {
        "account".to_string()
    } else {
        slug
    }
}

/// True when `dir` is a login directory `accounts add` made for `provider`:
/// exactly one level below `accounts/<provider>` in this data directory, also
/// after resolving symlinks. Only such a directory, and the Keychain item named
/// after it, is ever deleted.
pub fn is_account_dir(data_dir: &Path, provider: &str, dir: &Path) -> bool {
    use std::path::Component;

    let parent = data_dir.join("accounts").join(provider);
    let one_level_below = dir.parent() == Some(parent.as_path())
        && matches!(dir.components().next_back(), Some(Component::Normal(_)));
    if !one_level_below {
        return false;
    }
    match (parent.canonicalize(), dir.canonicalize()) {
        (Ok(parent), Ok(dir)) => dir.parent() == Some(parent.as_path()),
        // Already gone, so there is nothing on disk a symlink could redirect.
        (_, Err(err)) => err.kind() == std::io::ErrorKind::NotFound,
        (Err(_), Ok(_)) => false,
    }
}

/// Create a directory, and its parents, readable by the owner only.
pub fn create_private_dir(dir: &Path) -> Result<(), String> {
    std::fs::create_dir_all(dir)
        .map_err(|err| format!("could not create {}: {}", dir.display(), err))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))
            .map_err(|err| format!("could not protect {}: {}", dir.display(), err))?;
    }
    Ok(())
}
