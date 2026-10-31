//! Covers how logins found in several places become one report per account.

use quota_cli::accounts::{
    account_label, candidate_order, claude_key, claude_login_for, group_logins, Login, Source,
    Store,
};
use quota_core::providers::ProviderError;
use quota_core::usage::ProviderUsage;
use std::path::PathBuf;

fn keys(items: &[Option<&str>]) -> Vec<Option<String>> {
    items.iter().map(|item| item.map(str::to_string)).collect()
}

fn login(source: Source, label: Option<&str>, email: Option<&str>) -> Login {
    Login {
        provider: "codex",
        source,
        label: label.map(str::to_string),
        location: None,
        store: Store::Codex(PathBuf::from("/nowhere")),
        email: email.map(str::to_string),
        key: None,
        expires_at_ms: None,
        problem: None,
    }
}

#[test]
fn logins_of_one_account_form_one_group_in_the_order_found() {
    assert_eq!(
        group_logins(&keys(&[
            Some("a"),
            Some("b"),
            Some("a"),
            Some("c"),
            Some("b")
        ])),
        vec![vec![0, 2], vec![1, 4], vec![3]]
    );
}

#[test]
fn logins_whose_account_is_unknown_are_never_merged() {
    // Each one keeps a line of its own, so each problem stays visible.
    assert_eq!(
        group_logins(&keys(&[None, None, Some("a")])),
        vec![vec![0], vec![1], vec![2]]
    );
}

#[test]
fn an_explicit_label_wins_over_the_current_login_and_other_names() {
    let current = login(Source::Current, None, Some("me@example.com"));
    let configured = login(Source::Config, Some("work"), Some("me@example.com"));
    let codexbar = login(Source::CodexBar, None, Some("me@example.com"));

    assert_eq!(
        account_label(&[&current, &codexbar, &configured]).as_deref(),
        Some("work")
    );
    // The current login keeps the plain provider name...
    assert_eq!(account_label(&[&current, &codexbar]), None);
    // ...and an account found only elsewhere is named by its address.
    assert_eq!(
        account_label(&[&codexbar]).as_deref(),
        Some("me@example.com")
    );

    let aliased = login(Source::ClaudeSwap, Some("dev"), Some("me@example.com"));
    assert_eq!(account_label(&[&aliased]).as_deref(), Some("dev"));
}

#[test]
fn the_login_with_the_freshest_token_is_tried_first() {
    let mut stale = login(Source::Current, None, None);
    stale.expires_at_ms = Some(1_000);
    let mut fresh = login(Source::CodexBar, None, None);
    fresh.expires_at_ms = Some(2_000);
    assert_eq!(candidate_order(&[&stale, &fresh]), vec![1, 0]);

    // On a tie the first found goes first, which is the current login.
    let mut twin = login(Source::CodexBar, None, None);
    twin.expires_at_ms = Some(1_000);
    assert_eq!(candidate_order(&[&stale, &twin]), vec![0, 1]);
}

#[test]
fn logins_quota_can_fetch_come_before_reported_and_broken_ones() {
    let mut broken = login(Source::Current, None, None);
    broken.problem = Some(ProviderError::new("unreadable", false));
    broken.expires_at_ms = Some(9_000);
    let mut reported = login(Source::ClaudeSwap, None, None);
    reported.store = Store::Reported(Ok(ProviderUsage {
        provider: "claude".to_string(),
        account_label: None,
        windows: vec![],
        note: None,
        resets: None,
    }));
    let usable = login(Source::Config, Some("work"), None);

    assert_eq!(
        candidate_order(&[&broken, &reported, &usable]),
        vec![2, 1, 0]
    );
    assert_eq!(candidate_order(&[&broken, &reported]), vec![1, 0]);
    assert_eq!(candidate_order(&[&broken]), vec![0]);
}

#[test]
fn claude_accounts_match_claude_swap_by_organization_and_address() {
    assert_eq!(
        claude_key(Some("org-1"), " Someone@Example.com "),
        claude_key(Some("org-1"), "someone@example.com")
    );
    assert_ne!(
        claude_key(Some("org-1"), "someone@example.com"),
        claude_key(Some("org-2"), "someone@example.com")
    );
    assert_eq!(
        claude_key(None, "someone@example.com"),
        claude_key(Some(""), "someone@example.com")
    );
}

#[test]
fn naming_the_default_claude_directory_means_the_default_login() {
    // Claude Code keeps the default login under the plain Keychain item. The
    // hashed item only exists when CLAUDE_CONFIG_DIR is set.
    let home = dirs::home_dir().expect("home directory");
    let default_dir = home.join(".claude");

    let login = claude_login_for(&default_dir.to_string_lossy());
    assert_eq!(login.keychain_service(), "Claude Code-credentials");

    let with_slash = claude_login_for(&format!("{}/", default_dir.display()));
    assert_eq!(with_slash.keychain_service(), "Claude Code-credentials");

    let other = claude_login_for("/Users/test/.claude-work");
    assert_eq!(other.keychain_service(), "Claude Code-credentials-03abf0ee");
}

#[test]
fn sign_in_hints_name_the_command_for_where_the_login_lives() {
    let mut managed = login(Source::Quota, Some("work account"), None);
    assert_eq!(
        managed.sign_in_hint(),
        "Sign in again with: quota-cli accounts add codex 'work account'"
    );

    managed.provider = "claude";
    managed.source = Source::Config;
    managed.location = Some("~/.claude-work".to_string());
    assert_eq!(
        managed.sign_in_hint(),
        "Sign in again with: CLAUDE_CONFIG_DIR=~/.claude-work claude auth login"
    );
}
