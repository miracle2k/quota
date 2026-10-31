//! Shared provider logic for Quota, free of any Tauri dependency so both the
//! desktop app and the standalone CLI can build from it.

pub mod fs_util;
pub(crate) mod http_error;
pub mod local_paths;
pub mod providers;
pub mod usage;
pub mod antigravity;
pub mod claude;
pub mod codex;
pub mod cursor;
pub mod grok;
pub mod kiro;
