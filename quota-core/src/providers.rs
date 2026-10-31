//! The provider registry: which providers have a local-credential reader, and
//! how to fetch usage for one by name.
//!
//! Callers that report on several providers at once need a single shape, so the
//! per-provider error types are flattened to their rendered message here. Every
//! one of those messages is already written to be read by a user.

use crate::usage::ProviderUsage;

/// Every provider the CLI can read, in the order it lists them. Shortest
/// windows first is a per-provider concern; this order is just the reading
/// order of the report.
pub const PROVIDERS: &[&str] = &["claude", "codex", "cursor", "antigravity", "grok", "kiro"];

/// Why a provider produced no usage.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderError {
    pub message: String,
    /// The provider is simply not set up here: no login on disk, or one with
    /// no usage to measure. A report can leave these out instead of listing
    /// every provider the user does not use.
    pub not_configured: bool,
}

impl ProviderError {
    pub fn new(message: impl Into<String>, not_configured: bool) -> Self {
        Self {
            message: message.into(),
            not_configured,
        }
    }
}

impl std::fmt::Display for ProviderError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.message)
    }
}

/// Flatten one provider's error, keeping whether it only means "not set up".
macro_rules! flatten_error {
    ($result:expr) => {
        $result.map_err(|err| ProviderError::new(err.to_string(), err.is_not_configured()))
    };
}

/// Fetch usage for one provider. `None` when the name is not a known provider,
/// which lets a caller tell "unknown provider" apart from "provider failed".
pub async fn fetch_provider(name: &str) -> Option<Result<ProviderUsage, ProviderError>> {
    let result = match name {
        "claude" => flatten_error!(crate::claude::local_usage::fetch_local_usage().await),
        "codex" => flatten_error!(crate::codex::local_usage::fetch_local_usage().await),
        "cursor" => flatten_error!(crate::cursor::local_usage::fetch_local_usage().await),
        "antigravity" => {
            flatten_error!(crate::antigravity::local_usage::fetch_local_usage().await)
        }
        "grok" => flatten_error!(crate::grok::local_usage::fetch_local_usage().await),
        "kiro" => flatten_error!(crate::kiro::local_usage::fetch_local_usage().await),
        _ => return None,
    };
    Some(result)
}

/// Fetch every provider in registry order.
///
/// The calls run one after another rather than together. A provider the user is
/// not signed into fails on the local file read without touching the network,
/// so the usual cost is one or two real requests.
pub async fn fetch_all() -> Vec<(&'static str, Result<ProviderUsage, ProviderError>)> {
    let mut reports = Vec::with_capacity(PROVIDERS.len());
    for name in PROVIDERS {
        if let Some(result) = fetch_provider(name).await {
            reports.push((*name, result));
        }
    }
    reports
}
