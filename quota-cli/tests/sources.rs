//! Covers reading other tools' account lists and quota-cli's own store.

use quota_cli::claude_swap::parse_list;
use quota_cli::codexbar::{parse_managed_accounts, parse_profile_homes};
use quota_cli::store::{self, ManagedAccount, Registry};
use std::path::PathBuf;

#[test]
fn reads_usage_from_cswap_list_json() {
    // Shape from claude-swap's JSON schema version 1. `pct` is percent used.
    let text = r#"{
      "schemaVersion": 1,
      "activeAccountNumber": 2,
      "accounts": [
        {
          "number": 1, "email": "Personal@Example.com", "organizationName": "",
          "organizationUuid": "", "isOrganization": false, "active": false,
          "usageStatus": "ok", "alias": "home",
          "usage": {
            "fiveHour": {"pct": 25.0, "resetsAt": "2026-10-31T20:00:00+00:00"},
            "sevenDay": {"pct": 60.4, "resetsAt": "2026-11-03T08:00:00Z", "aheadOfPace": true}
          }
        },
        {
          "number": 2, "email": "work@example.com", "organizationName": "Acme",
          "organizationUuid": "org-9", "isOrganization": true, "active": true,
          "usageStatus": "token_expired", "usage": null,
          "lastGoodUsage": {"fiveHour": {"pct": 90}},
          "lastGoodFetchedAt": "2026-10-31T10:00:00Z"
        },
        {
          "number": 3, "email": "api-key-3@token.local", "organizationName": "",
          "organizationUuid": "", "isOrganization": false, "active": false,
          "usageStatus": "api_key", "usage": null
        },
        {
          "number": 4, "email": "gone@example.com", "organizationName": "",
          "organizationUuid": "", "isOrganization": false, "active": false,
          "usageStatus": "relogin_required", "usage": null
        }
      ]
    }"#;

    let accounts = parse_list(text).expect("parse");
    assert_eq!(accounts.len(), 4);

    let home = &accounts[0];
    assert_eq!(home.alias.as_deref(), Some("home"));
    assert_eq!(home.organization_uuid, "");
    let usage = home.result.as_ref().expect("usage");
    assert_eq!(usage.windows[0].label, "5h");
    assert_eq!(usage.windows[0].remaining_percent, Some(75));
    assert_eq!(usage.windows[0].reset_at, Some(1_793_476_800));
    assert_eq!(usage.windows[1].remaining_percent, Some(40));

    // No live reading, but the last good one still says something.
    let work = &accounts[1];
    assert!(work.active);
    assert_eq!(
        work.result.as_ref().expect("last good").windows[0].remaining_percent,
        Some(10)
    );

    let api_key = accounts[2].result.as_ref().expect_err("api key");
    assert!(api_key.not_configured);

    let gone = accounts[3].result.as_ref().expect_err("relogin");
    assert!(!gone.not_configured);
    assert!(gone.message.contains("sign in"));
}

#[test]
fn a_cswap_error_or_unknown_schema_is_reported_not_guessed() {
    let error = parse_list(r#"{"schemaVersion":1,"error":{"type":"LockError","message":"busy"}}"#)
        .expect_err("error envelope");
    assert!(error.contains("busy"));

    assert!(parse_list(r#"{"schemaVersion":2,"accounts":[]}"#).is_err());
    assert!(parse_list("not json").is_err());
}

#[test]
fn reads_codexbar_managed_accounts() {
    let text = r#"{
      "version": 3,
      "accounts": [
        {"id": "A", "email": "me@example.com", "managedHomePath": "/tmp/codexbar/A",
         "workspaceLabel": "Personal", "createdAt": 1789164952.6},
        {"id": "B", "email": "", "managedHomePath": "~/codexbar/B"},
        {"id": "C", "email": "no-home@example.com"}
      ]
    }"#;
    let homes = parse_managed_accounts(text);
    assert_eq!(homes.len(), 2);
    assert_eq!(homes[0].email.as_deref(), Some("me@example.com"));
    assert_eq!(homes[0].home, PathBuf::from("/tmp/codexbar/A"));
    assert_eq!(homes[1].email, None);
    let home = dirs::home_dir().expect("home");
    assert_eq!(homes[1].home, home.join("codexbar/B"));

    // A registry version CodexBar has not written yet is skipped.
    assert!(
        parse_managed_accounts(r#"{"version":4,"accounts":[{"managedHomePath":"/x"}]}"#).is_empty()
    );
    assert!(parse_managed_accounts("{").is_empty());
}

#[test]
fn reads_only_the_codex_profile_homes_from_codexbar_config() {
    let text = r#"{
      "version": 1,
      "providers": [
        {"id": "claude", "codexProfileHomePaths": ["/not/codex"], "apiKey": "ignored"},
        {"id": "codex", "codexProfileHomePaths": ["~/.codex-work", "relative/skipped", "/srv/codex"]}
      ]
    }"#;
    let homes: Vec<PathBuf> = parse_profile_homes(text)
        .into_iter()
        .map(|home| home.home)
        .collect();
    let home = dirs::home_dir().expect("home");
    assert_eq!(
        homes,
        vec![home.join(".codex-work"), PathBuf::from("/srv/codex")]
    );
}

#[test]
fn account_directories_are_named_after_the_label() {
    assert_eq!(store::slug("Work"), "work");
    assert_eq!(store::slug("  Client: Acme / EU "), "client-acme-eu");
    assert_eq!(store::slug("!!!"), "account");

    let data = std::env::temp_dir().join(format!("quota-cli-store-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&data);
    let first = store::new_account_dir(&data, "codex", "Work");
    assert_eq!(first, data.join("accounts").join("codex").join("work"));
    std::fs::create_dir_all(&first).expect("create");
    // A leftover directory with that name is never reused.
    assert_eq!(
        store::new_account_dir(&data, "codex", "work"),
        data.join("accounts").join("codex").join("work-2")
    );
    let _ = std::fs::remove_dir_all(&data);
}

#[test]
fn the_registry_round_trips_and_a_missing_one_is_empty() {
    let data = std::env::temp_dir().join(format!("quota-cli-registry-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&data);
    assert_eq!(store::load(&data), Ok(Registry::default()));

    let registry = Registry {
        version: 1,
        accounts: vec![ManagedAccount {
            provider: "claude".to_string(),
            label: "work".to_string(),
            dir: "/data/accounts/claude/work".to_string(),
            created_at: None,
        }],
    };
    store::save(&data, &registry).expect("save");
    assert_eq!(store::load(&data), Ok(registry.clone()));
    assert!(registry.find("claude", "work").is_some());
    assert!(registry.find("codex", "work").is_none());

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(store::registry_path(&data))
            .expect("metadata")
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600);
    }
    let _ = std::fs::remove_dir_all(&data);
}

#[test]
fn only_an_account_directory_of_that_provider_counts_as_removable() {
    let data = std::env::temp_dir().join(format!("quota-cli-inside-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&data);
    let accounts = data.join("accounts");
    let work = accounts.join("codex").join("work");
    std::fs::create_dir_all(&work).expect("create");

    assert!(store::is_account_dir(&data, "codex", &work));
    // One that is already gone has nothing left to redirect a deletion.
    assert!(store::is_account_dir(
        &data,
        "codex",
        &accounts.join("codex").join("gone")
    ));

    assert!(!store::is_account_dir(&data, "claude", &work));
    assert!(!store::is_account_dir(
        &data,
        "codex",
        &accounts.join("codex")
    ));
    assert!(!store::is_account_dir(&data, "codex", &accounts));
    assert!(!store::is_account_dir(&data, "codex", &data));
    assert!(!store::is_account_dir(&data, "codex", &work.join("nested")));
    assert!(!store::is_account_dir(
        &data,
        "codex",
        &accounts.join("codex").join("..")
    ));
    assert!(!store::is_account_dir(
        &data,
        "codex",
        &std::env::temp_dir()
    ));

    #[cfg(unix)]
    {
        // A symlink that looks like an account directory but leads elsewhere.
        let elsewhere = data.join("elsewhere");
        std::fs::create_dir_all(&elsewhere).expect("create");
        let link = accounts.join("codex").join("link");
        std::os::unix::fs::symlink(&elsewhere, &link).expect("symlink");
        assert!(!store::is_account_dir(&data, "codex", &link));
    }
    let _ = std::fs::remove_dir_all(&data);
}
