//! `quota-cli accounts add` and `accounts remove`: sign in to another Claude or
//! Codex account in a directory quota-cli keeps, or delete one again.
//!
//! Signing in is left to the provider's own CLI, pointed at the new directory
//! the same way a user would point it there by hand, so the result is a
//! normal login that CLI can use too.

use crate::store::{self, ManagedAccount};
use quota_core::claude::local::ClaudeLogin;
use quota_core::codex::local::{credentials_path_in, read_local_credentials_at};
use std::path::{Path, PathBuf};
use std::process::Command;

/// A new Codex home gets this config, so the login lands in `auth.json`
/// where quota reads it, even if the CLI's default store changes.
const CODEX_CONFIG: &str = "# Written by quota-cli: keep this login in auth.json.\n\
cli_auth_credentials_store = \"file\"\n";

/// Sign in to an account and keep it under `label`. Signing in again under a
/// label that exists reuses its directory, which is how an account whose login
/// expired for good is repaired.
pub fn add(provider: &str, label: &str, device_auth: bool) -> Result<String, String> {
    let data_dir = store::data_dir().ok_or("could not find a data directory")?;
    let mut registry = store::load(&data_dir)?;
    let existing = registry.find(provider, label).cloned();

    let dir = match &existing {
        Some(account) => PathBuf::from(&account.dir),
        None => store::new_account_dir(&data_dir, provider, label),
    };
    let dir_text = dir
        .to_str()
        .ok_or_else(|| format!("{} is not valid UTF-8", dir.display()))?
        .to_string();
    store::create_private_dir(&dir)?;
    if provider == "codex" && !dir.join("config.toml").exists() {
        quota_core::fs_util::write_private_atomic(&dir.join("config.toml"), CODEX_CONFIG)?;
    }

    match existing {
        Some(_) => eprintln!(
            "Signing in to the {} account \"{}\" again.",
            provider, label
        ),
        None => eprintln!(
            "Signing in to a new {} account for \"{}\".",
            provider, label
        ),
    }
    let signed_in = run_sign_in(provider, &dir_text, device_auth);

    let email = match signed_in.and_then(|()| signed_in_email(provider, &dir_text)) {
        Ok(email) => email,
        Err(message) => {
            if existing.is_none() {
                // Nothing was saved, so leave nothing behind.
                let _ = std::fs::remove_dir_all(&dir);
            }
            return Err(message);
        }
    };

    if existing.is_none() {
        registry.accounts.push(ManagedAccount {
            provider: provider.to_string(),
            label: label.to_string(),
            dir: dir_text.clone(),
            created_at: Some(chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true)),
        });
        store::save(&data_dir, &registry)?;
    }

    let who = email
        .map(|email| format!(" as {}", email))
        .unwrap_or_default();
    let mut message = format!(
        "Signed in to {} \"{}\"{}.\nThe login is in {}",
        provider, label, who, dir_text
    );
    if let Some(other) = registry
        .for_provider(provider)
        .find(|account| account.label != label && same_account(provider, &account.dir, &dir_text))
    {
        message.push_str(&format!(
            "\nNote: this is the same account as \"{}\". `usage` shows it once.",
            other.label
        ));
    }
    Ok(message)
}

/// Forget an account `add` made and delete its login: the directory, and on
/// macOS the Keychain item Claude Code filed under it.
pub fn remove(provider: &str, label: &str) -> Result<String, String> {
    let data_dir = store::data_dir().ok_or("could not find a data directory")?;
    let mut registry = store::load(&data_dir)?;
    let account = registry.find(provider, label).cloned().ok_or_else(|| {
        format!(
            "quota-cli has no {} account \"{}\". `quota-cli accounts` lists them; accounts \
             from the config file, CodexBar or claude-swap are removed there.",
            provider, label
        )
    })?;

    // Check first, so a hand-edited entry pointing somewhere else never gets
    // its Keychain item deleted either.
    let dir = Path::new(&account.dir);
    if !store::is_account_dir(&data_dir, provider, dir) {
        return Err(format!(
            "refusing to delete {}: it is not an account directory under {}",
            dir.display(),
            data_dir.join("accounts").join(provider).display()
        ));
    }
    if provider == "claude" {
        delete_claude_keychain_item(&account.dir)?;
    }
    if dir.exists() {
        std::fs::remove_dir_all(dir)
            .map_err(|err| format!("could not delete {}: {}", dir.display(), err))?;
    }

    registry
        .accounts
        .retain(|kept| !(kept.provider == provider && kept.label == label));
    store::save(&data_dir, &registry)?;
    Ok(format!("Removed {} \"{}\".", provider, label))
}

/// Run the provider's sign-in with the terminal attached, pointed at `dir`.
fn run_sign_in(provider: &str, dir: &str, device_auth: bool) -> Result<(), String> {
    let mut command = match provider {
        "claude" => {
            let mut command = Command::new("claude");
            command
                .args(["auth", "login"])
                .env("CLAUDE_CONFIG_DIR", dir)
                // This would file the login under another Keychain item.
                .env_remove("CLAUDE_SECURESTORAGE_CONFIG_DIR");
            command
        }
        _ => {
            let mut command = Command::new("codex");
            command.arg("login").env("CODEX_HOME", dir);
            if device_auth {
                command.arg("--device-auth");
            }
            command
        }
    };
    let program = if provider == "claude" {
        "claude"
    } else {
        "codex"
    };
    let status = command.status().map_err(|err| {
        format!(
            "could not run {}: {}. Install the {} CLI first.",
            program,
            err,
            if provider == "claude" {
                "Claude Code"
            } else {
                "Codex"
            }
        )
    })?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("{} sign-in did not finish ({})", program, status))
    }
}

/// Confirm the sign-in left a login quota can read, and say whose it is.
fn signed_in_email(provider: &str, dir: &str) -> Result<Option<String>, String> {
    if provider == "claude" {
        let login = ClaudeLogin::for_config_dir(dir);
        login.read_credentials().map_err(|err| {
            format!(
                "the sign-in finished, but quota cannot read the login: {}",
                err
            )
        })?;
        Ok(login.account_info().and_then(|info| info.email))
    } else {
        let credentials =
            read_local_credentials_at(&credentials_path_in(Path::new(dir))).map_err(|err| {
                format!(
                    "the sign-in finished, but quota cannot read the login: {}",
                    err
                )
            })?;
        Ok(credentials.email)
    }
}

fn same_account(provider: &str, a: &str, b: &str) -> bool {
    let key = |dir: &str| -> Option<String> {
        if provider == "claude" {
            let info = ClaudeLogin::for_config_dir(dir).account_info()?;
            Some(crate::accounts::claude_key(
                info.organization_uuid.as_deref(),
                &info.email?,
            ))
        } else {
            read_local_credentials_at(&credentials_path_in(Path::new(dir)))
                .ok()?
                .identity()
        }
    };
    matches!((key(a), key(b)), (Some(a), Some(b)) if a == b)
}

/// Delete the Keychain item Claude Code made for a login in `dir`. Only the
/// item named after that directory is touched; the default login's plain item
/// never matches.
#[cfg(target_os = "macos")]
fn delete_claude_keychain_item(dir: &str) -> Result<(), String> {
    let service = ClaudeLogin::for_config_dir(dir).keychain_service();
    if service == quota_core::claude::local::KEYCHAIN_SERVICE {
        return Err("refusing to delete the default Claude Code login".to_string());
    }
    // One item per account name can share the service, so delete until none
    // is left. `security` exits with 44 once no item matches.
    for _ in 0..8 {
        let status = Command::new("/usr/bin/security")
            .args(["delete-generic-password", "-s", &service])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .map_err(|err| err.to_string())?;
        match status.code() {
            Some(0) => continue,
            Some(44) => return Ok(()),
            _ => {
                return Err(format!(
                    "could not delete the Keychain item {} ({})",
                    service, status
                ))
            }
        }
    }
    Ok(())
}

#[cfg(not(target_os = "macos"))]
fn delete_claude_keychain_item(_dir: &str) -> Result<(), String> {
    Ok(())
}
