//! The optional config file. It lists extra Claude and Codex logins for
//! `usage` to report on, each kept in its own directory the way the provider's
//! CLI keeps one when pointed somewhere else.
//!
//! ```toml
//! [[claude]]
//! label = "work"
//! config_dir = "~/.claude-work"   # the CLAUDE_CONFIG_DIR of that login
//!
//! [[codex]]
//! label = "personal"
//! home = "~/.codex-personal"      # the CODEX_HOME of that login
//! ```

use serde::Deserialize;
use std::path::{Path, PathBuf};

/// Points at a config file somewhere other than the default location.
pub const CONFIG_ENV: &str = "QUOTA_CLI_CONFIG";

#[derive(Debug, Default, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    #[serde(default)]
    pub claude: Vec<ClaudeAccount>,
    #[serde(default)]
    pub codex: Vec<CodexAccount>,
}

#[derive(Debug, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ClaudeAccount {
    pub label: String,
    /// The `CLAUDE_CONFIG_DIR` Claude Code keeps this login in.
    pub config_dir: String,
}

#[derive(Debug, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CodexAccount {
    pub label: String,
    /// The `CODEX_HOME` the Codex CLI keeps this login in.
    pub home: String,
}

/// `quota-cli/config.toml` under the XDG config directory, which is
/// `~/.config` unless `XDG_CONFIG_HOME` moves it.
pub fn default_config_path() -> Option<PathBuf> {
    quota_core::local_paths::config_home().map(|dir| dir.join("quota-cli").join("config.toml"))
}

/// Load the config file. The default file is optional, so a missing one is an
/// empty config. A file named through `QUOTA_CLI_CONFIG` must exist, because
/// pointing at a file that is not there is a mistake worth reporting.
pub fn load() -> Result<Config, String> {
    if let Some(path) = std::env::var_os(CONFIG_ENV).filter(|value| !value.is_empty()) {
        return load_from(Path::new(&path), true);
    }
    match default_config_path() {
        Some(path) => load_from(&path, false),
        None => Ok(Config::default()),
    }
}

pub fn load_from(path: &Path, required: bool) -> Result<Config, String> {
    match std::fs::read_to_string(path) {
        Ok(text) => parse(&text).map_err(|err| format!("{}: {}", path.display(), err)),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound && !required => {
            Ok(Config::default())
        }
        Err(err) => Err(format!("could not read {}: {}", path.display(), err)),
    }
}

pub fn parse(text: &str) -> Result<Config, String> {
    let config: Config = toml::from_str(text).map_err(|err| err.to_string())?;

    let labels = config
        .claude
        .iter()
        .map(|account| account.label.as_str())
        .chain(config.codex.iter().map(|account| account.label.as_str()));
    if labels.into_iter().any(|label| label.trim().is_empty()) {
        return Err("every account needs a non-empty label".to_string());
    }

    Ok(config)
}

/// Expand a leading `~` the way a shell does, so the file can name a
/// directory the way the user types it.
pub fn expand_home(path: &str) -> String {
    let home = || dirs::home_dir().map(|home| home.to_string_lossy().into_owned());
    if path == "~" {
        return home().unwrap_or_else(|| path.to_string());
    }
    match (path.strip_prefix("~/"), home()) {
        (Some(rest), Some(home)) => format!("{}/{}", home.trim_end_matches('/'), rest),
        _ => path.to_string(),
    }
}
