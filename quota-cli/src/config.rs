//! The optional config file. It lists extra Claude and Codex logins for
//! `usage` to report on, each kept in its own directory the way the provider's
//! CLI keeps one when pointed somewhere else. It can also turn off reading the
//! accounts of other tools, and quiet providers you do not care about.
//!
//! ```toml
//! [[claude]]
//! label = "work"
//! config_dir = "~/.claude-work"   # the CLAUDE_CONFIG_DIR of that login
//!
//! [[codex]]
//! label = "personal"
//! home = "~/.codex-personal"      # the CODEX_HOME of that login
//!
//! [sources]
//! codexbar = true      # CodexBar's Codex accounts
//! claude_swap = true   # claude-swap's Claude accounts
//!
//! [usage]
//! skip = ["kiro"]                 # never checked
//! hide_errors = ["antigravity"]   # shown only when they report usage
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
    #[serde(default)]
    pub sources: Sources,
    #[serde(default)]
    pub usage: UsageSettings,
}

/// Providers the `usage` report leaves out.
#[derive(Debug, Default, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UsageSettings {
    /// Never checked, unless named on the command line.
    #[serde(default)]
    pub skip: Vec<String>,
    /// Shown only when they report usage. `--all` and `--json` still show
    /// why one failed.
    #[serde(default)]
    pub hide_errors: Vec<String>,
}

impl UsageSettings {
    pub fn hides_errors_of(&self, provider: &str) -> bool {
        self.hide_errors.iter().any(|name| name == provider)
    }
}

/// Other tools whose accounts `usage` reads when they are installed. Both are
/// on unless the file turns them off.
#[derive(Debug, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Sources {
    #[serde(default = "enabled")]
    pub codexbar: bool,
    #[serde(default = "enabled")]
    pub claude_swap: bool,
}

impl Default for Sources {
    fn default() -> Self {
        Self {
            codexbar: true,
            claude_swap: true,
        }
    }
}

fn enabled() -> bool {
    true
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

    // A misspelled name would silently quiet nothing.
    let named = [("skip", &config.usage.skip), ("hide_errors", &config.usage.hide_errors)];
    for (key, names) in named {
        if let Some(unknown) = names
            .iter()
            .find(|name| !quota_core::providers::PROVIDERS.contains(&name.as_str()))
        {
            return Err(format!(
                "unknown provider in usage.{}: {} (known: {})",
                key,
                unknown,
                quota_core::providers::PROVIDERS.join(", ")
            ));
        }
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
