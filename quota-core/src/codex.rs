//! Codex provider support for the standalone CLI.
//!
//! Only the local-credential path lives here: reading a Codex CLI login,
//! fetching its usage, and renewing its token the way the CLI does. The
//! desktop app keeps its own account store, OAuth flow, and token refresh in
//! `src-tauri/src/codex.rs`, none of which the CLI needs or touches.

pub mod local;
pub mod local_usage;
pub mod refresh;
